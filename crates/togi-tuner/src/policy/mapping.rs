//! Mapping: the edge U each CCD's isolated trials reveal, the cap S the exposure ledger supports and
//! the next offset to map, all derived from raw trials and exposures on every query.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::num::NonZeroU32;
use std::sync::Arc;

use togi_journal::{
    Binding, Candidate, CcdCap, CcdEdge, Edge, ExposureClass, Finite, LowerEndpoint,
    Rate as JournalRate, RateBound, RateCell, Seq, Tested,
};
use togi_machine::{CcdId, ContentId, TargetOffset, WorkloadId};

use super::State;
use super::books::Trial;
use super::decision::{Decision, PolicyPayload};
use super::evidence::ClassCap;
use super::ledger::{Sig, is_adequate, saturating_u32};
use super::night_query::NotReady;
use super::plan::workloads_of;
use crate::cap::{
    self, Beta, CapLimits, EDGE_MARGIN, Hours, LikelihoodCutoff, Rate, Refresh, Slope,
};
use crate::night::{
    Cursor, EdgeU, IsolatedObservation, IsolatedResult, Layout, Location, MappingBoard,
    MappingTarget, StepResult, mapping_cells,
};
use togi_journal::Outcome;

/// Offsets run 0 to -50, so a cursor can never need more alignment steps than this.
const MAX_ALIGN: usize = 60;

/// A class of the interned cap input; the cap's classes must be ordered, an exposure class is not.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct ClassId(u32);

/// The mapping board replayed from the journal.
#[derive(Clone, Debug)]
pub(super) struct Mapped {
    pub(super) board: MappingBoard,
    ccds: Vec<CcdId>,
    /// The passing trial ends of the step that censored a CCD's edge.
    pub(super) censored: BTreeMap<CcdId, Vec<Seq>>,
    /// Each CCD's latest mapping task still without a finished step.
    pub(super) open: BTreeMap<CcdId, (u32, TargetOffset)>,
    pub(super) next_ordinal: u32,
}

/// The cap S of one CCD and what produced it.
#[derive(Clone, Debug)]
pub(super) struct CapOutcome {
    pub(super) cap: cap::Cap<ClassId>,
    pub(super) classes: Vec<ExposureClass>,
    pub(super) previous: Slope,
    /// Eligible seconds per mapping class and offset, for the journal's rate cells.
    pub(super) mapping_secs: BTreeMap<(ClassId, TargetOffset), u64>,
}

/// Everything mapping decides this query.
#[derive(Clone, Debug)]
pub(super) struct MappingPlan {
    pub(super) mapped: Mapped,
    pub(super) edges: BTreeMap<CcdId, EdgeU>,
    pub(super) caps: BTreeMap<CcdId, Arc<CapOutcome>>,
    pub(super) target: Option<MappingTarget>,
}

fn cursor_offset(cursor: Cursor) -> i64 {
    match cursor {
        Cursor::Coarse { offset } => offset,
        Cursor::Bracket { next, .. } | Cursor::Sweep { next } => next,
    }
}

/// What a CCD's cap derives from: the cap is recomputed only when one of these moves.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CapKey {
    edge: EdgeU,
    ordinal: u32,
    committed: Option<Seq>,
    /// The current groups and the latest exposure of each that counted for the CCD.
    scoped: Vec<(usize, Option<Seq>)>,
    partials: BTreeMap<CcdId, crate::HonestChain>,
    contents: BTreeMap<WorkloadId, ContentId>,
}

/// What one mapping task has seen so far.
#[derive(Clone, Debug)]
struct TaskProgress {
    ccd: CcdId,
    offset: TargetOffset,
    wanted: HashSet<Sig>,
    passed: HashSet<Sig>,
    passes: Vec<Seq>,
}

/// The incremental mapping replay and the layout it is valid for.
#[derive(Clone, Debug)]
pub(super) struct MapReplay {
    key: Option<(
        BTreeMap<CcdId, crate::HonestChain>,
        Vec<togi_machine::CoreInfo>,
    )>,
    /// How many of the ended trials it has folded.
    processed: usize,
    /// The tasks without a finished step.
    tasks: HashMap<u32, TaskProgress>,
    finished: HashSet<u32>,
    unfinished: BTreeMap<CcdId, BTreeMap<u32, TargetOffset>>,
    mapped: Mapped,
}

impl Default for MapReplay {
    fn default() -> Self {
        Self {
            key: None,
            processed: 0,
            tasks: HashMap::new(),
            finished: HashSet::new(),
            unfinished: BTreeMap::new(),
            mapped: Mapped {
                board: MappingBoard::new(Vec::new()),
                ccds: Vec::new(),
                censored: BTreeMap::new(),
                open: BTreeMap::new(),
                next_ordinal: 0,
            },
        }
    }
}

impl MapReplay {
    fn new(layout: &Layout<'_>, ccds: Vec<CcdId>) -> Self {
        Self {
            key: Some((layout.partials.clone(), layout.cores.to_vec())),
            mapped: Mapped {
                board: MappingBoard::new(ccds.clone()),
                ccds,
                censored: BTreeMap::new(),
                open: BTreeMap::new(),
                next_ordinal: 0,
            },
            ..Self::default()
        }
    }

    fn fits(&self, layout: &Layout<'_>) -> bool {
        self.key
            .as_ref()
            .is_some_and(|(partials, cores)| partials == layout.partials && cores == layout.cores)
    }

    /// Each CCD's latest task still without a finished step.
    fn refresh_open(&mut self) {
        self.mapped.open = self
            .unfinished
            .iter()
            .filter_map(|(&ccd, tasks)| {
                tasks
                    .last_key_value()
                    .map(|(&ordinal, &offset)| (ccd, (ordinal, offset)))
            })
            .collect();
    }
}

impl State {
    pub(super) fn known_contents(
        &self,
        extra: Option<&BTreeMap<WorkloadId, ContentId>>,
    ) -> BTreeMap<WorkloadId, ContentId> {
        let mut known = self.machine.contents.clone();
        if let Some(extra) = extra {
            known.extend(extra.iter().map(|(&w, c)| (w, c.clone())));
        }
        known
    }

    /// The class of a cell under `contents`, `None` when a workload's content is not known.
    pub(super) fn class_of(
        cell: &crate::night::Cell,
        contents: &BTreeMap<WorkloadId, ContentId>,
    ) -> Option<ExposureClass> {
        let sig = Sig::of_cell(cell);
        Some(ExposureClass {
            regime: cell.regime(),
            workloads: Self::contents_of(&workloads_of(cell.workloads), contents)?,
            cores: cell.loaded,
            position: sig.position(),
            duration_s: cell.duration.get(),
            placement: sig.placement(),
        })
    }

    /// The mapping board as the journal's mapping tasks and isolated reproduction failures replay it, in
    /// the order the trials ended. Each query folds only the trials that ended since the last one; a
    /// changed layout, which can change what a task's cells are, replays from the start.
    pub(super) fn replay_mapping(&self, layout: &Layout<'_>) -> Result<Mapped, NotReady> {
        let mut replay = self.caches.mapping.lock();
        if !replay.fits(layout) {
            *replay = MapReplay::new(layout, self.ccds());
        }
        while let Some(&at) = self.trials.ended_order.get(replay.processed) {
            if let Some(trial) = self.trials.all.get(at) {
                self.replay_trial(&mut replay, layout, trial)?;
            }
            replay.processed += 1;
        }
        Ok(replay.mapped.clone())
    }

    fn replay_trial(
        &self,
        replay: &mut MapReplay,
        layout: &Layout<'_>,
        trial: &Trial,
    ) -> Result<(), NotReady> {
        let (Some(task), Some(Tested::Isolated { ccd, offset }), Some(ended)) =
            (trial.task(), trial.intent.tested, trial.end.as_ref())
        else {
            return Ok(());
        };
        match task.queue {
            // Reproduction attempts and filler cap probes are isolated observations: they move U but never
            // finish a mapping step.
            togi_journal::Queue::Reproduction | togi_journal::Queue::Filler => {
                if let Some(result) = self.isolated_result(trial, ccd) {
                    replay.mapped.board.observe(IsolatedObservation {
                        ccd,
                        offset,
                        result,
                    });
                }
            }
            togi_journal::Queue::Mapping => {
                let ordinal = task.ordinal;
                replay.mapped.next_ordinal = replay.mapped.next_ordinal.max(ordinal + 1);
                if replay.finished.contains(&ordinal) {
                    return Ok(());
                }
                if let std::collections::hash_map::Entry::Vacant(slot) = replay.tasks.entry(ordinal)
                {
                    let wanted: HashSet<Sig> = mapping_cells(layout, ccd, offset, ordinal)?
                        .fresh()
                        .map(Sig::of_cell)
                        .collect();
                    slot.insert(TaskProgress {
                        ccd,
                        offset,
                        wanted,
                        passed: HashSet::new(),
                        passes: Vec::new(),
                    });
                    replay
                        .unfinished
                        .entry(ccd)
                        .or_default()
                        .insert(ordinal, offset);
                }
                let Some(observed) = self.derive_exposure(trial) else {
                    return Ok(());
                };
                let Some(progress) = replay.tasks.get_mut(&ordinal) else {
                    return Ok(());
                };
                let (ccd, offset) = (progress.ccd, progress.offset);
                let outcome = if observed.outcome == Outcome::Failure
                    && self.isolated_result(trial, ccd)
                        == Some(IsolatedResult::Fail {
                            location: Location::TestedCcd,
                            witness: ended.seq,
                        }) {
                    Some((StepResult::Failed { witness: ended.seq }, Vec::new()))
                } else if is_adequate(&observed, &self.machine.cores) {
                    progress.passed.insert(Sig::of_class(&observed.class));
                    progress.passes.push(ended.seq);
                    progress
                        .wanted
                        .is_subset(&progress.passed)
                        .then(|| (StepResult::Passed, progress.passes.clone()))
                } else {
                    None
                };
                if let Some((result, passes)) = outcome {
                    replay.tasks.remove(&ordinal);
                    replay.finished.insert(ordinal);
                    if let Some(open) = replay.unfinished.get_mut(&ccd) {
                        open.remove(&ordinal);
                    }
                    replay.mapped.finish(ccd, offset, result, passes);
                }
            }
            togi_journal::Queue::Qualification
            | togi_journal::Queue::Control
            | togi_journal::Queue::Requalification => {}
        }
        replay.refresh_open();
        Ok(())
    }

    /// The isolated result of an ended trial: a pass, or a failure located on the tested CCD or elsewhere.
    /// Inconclusive trials have none.
    fn isolated_result(&self, trial: &Trial, tested: CcdId) -> Option<IsolatedResult> {
        let ended = trial.end.as_ref()?;
        match ended.end.outcome {
            Outcome::Pass => Some(IsolatedResult::Pass),
            Outcome::Inconclusive => None,
            Outcome::Failure => {
                let on_tested = ended.end.core.is_none_or(|core| {
                    self.machine
                        .cores
                        .iter()
                        .any(|info| info.core == core && info.ccd == tested)
                });
                Some(IsolatedResult::Fail {
                    location: if on_tested {
                        Location::TestedCcd
                    } else {
                        Location::Elsewhere
                    },
                    witness: ended.seq,
                })
            }
        }
    }
}

impl Mapped {
    /// Finishes the task's step. Steps the board skipped past were covered when the policy chose to jump over
    /// them, so the cursor advances as passed to reach the task's offset; a task the cursor cannot reach is stale.
    fn finish(&mut self, ccd: CcdId, offset: TargetOffset, result: StepResult, passes: Vec<Seq>) {
        let mut board = self.board.clone();
        let reached = (0..MAX_ALIGN).any(|_| {
            let Some(mapping) = board.ccd(ccd) else {
                return false;
            };
            if cursor_offset(mapping.cursor()) == offset.get() {
                return true;
            }
            board.finish_step(ccd, StepResult::Passed);
            false
        });
        if !reached {
            return;
        }
        let before = board.ccd(ccd).map(super::super::night::CcdMapping::edge);
        board.finish_step(ccd, result);
        let after = board.ccd(ccd).map(super::super::night::CcdMapping::edge);
        if after == Some(EdgeU::Censored) && before != after {
            self.censored.insert(ccd, passes);
        }
        self.board = board;
    }
}

impl Mapped {
    pub(super) fn edges(&self) -> BTreeMap<CcdId, EdgeU> {
        self.ccds
            .iter()
            .filter_map(|&ccd| self.board.ccd(ccd).map(|mapping| (ccd, mapping.edge())))
            .collect()
    }

    /// The ordinal a step at `target` runs under: its open task's, or the next one.
    pub(super) fn ordinal_for(&self, target: &MappingTarget) -> u32 {
        match self.open.get(&target.ccd) {
            Some(&(ordinal, offset)) if offset == target.offset => ordinal,
            Some(_) | None => self.next_ordinal,
        }
    }
}

/// Assigns each distinct class signature an orderable id, keeping the first class seen for it.
#[derive(Default)]
struct Interner {
    ids: HashMap<Sig, ClassId>,
    classes: Vec<ExposureClass>,
}

impl Interner {
    fn id(&mut self, class: &ExposureClass) -> ClassId {
        let sig = Sig::of_class(class);
        if let Some(&id) = self.ids.get(&sig) {
            return id;
        }
        let id = ClassId(saturating_u32(
            u64::try_from(self.classes.len()).unwrap_or(u64::MAX),
        ));
        self.ids.insert(sig, id);
        self.classes.push(class.clone());
        id
    }
}

fn slope_of(slope: &togi_journal::Slope) -> Slope {
    match slope {
        togi_journal::Slope::Fallback { beta } => {
            Beta::new(beta.get()).map_or(Slope::FALLBACK, Slope::Fallback)
        }
        togi_journal::Slope::Measured { lower, .. } => match lower {
            LowerEndpoint::Bounded { beta } => match Beta::new(beta.get()) {
                Ok(beta) if beta.get() > 0.0 => Slope::Measured { lower: beta },
                Ok(beta) => Slope::NoJustifiedDecay(cap::NoDecay::Nonpositive(beta)),
                Err(_) => Slope::NoJustifiedDecay(cap::NoDecay::Unbounded),
            },
            LowerEndpoint::Unbounded {} => Slope::NoJustifiedDecay(cap::NoDecay::Unbounded),
        },
    }
}

fn journal_slope(
    slope: Slope,
    historical_min: Option<Beta>,
) -> Result<togi_journal::Slope, NotReady> {
    Ok(match slope {
        Slope::Fallback(beta) => togi_journal::Slope::Fallback {
            beta: Finite::new(beta.get())?,
        },
        Slope::Measured { lower } => togi_journal::Slope::Measured {
            lower: LowerEndpoint::Bounded {
                beta: Finite::new(lower.get())?,
            },
            historical_min: historical_min
                .map(|beta| Finite::new(beta.get()))
                .transpose()?,
        },
        Slope::NoJustifiedDecay(cap::NoDecay::Unbounded) => togi_journal::Slope::Measured {
            lower: LowerEndpoint::Unbounded {},
            historical_min: None,
        },
        Slope::NoJustifiedDecay(cap::NoDecay::Nonpositive(beta)) => togi_journal::Slope::Measured {
            lower: LowerEndpoint::Bounded {
                beta: Finite::new(beta.get())?,
            },
            historical_min: None,
        },
    })
}

/// The deepest offset where the witness's projection stays within `budget`, CO 0 when none does.
fn candidate_offset(witness: &cap::Witness<ClassId>, beta: Beta, budget: Rate) -> TargetOffset {
    (TargetOffset::clamped(i64::MIN).get()..0)
        .map(TargetOffset::clamped)
        .find(|&offset| cap::project(witness, beta, offset).within(budget))
        .unwrap_or(TargetOffset::ZERO)
}

fn rate_bound(bound: cap::Bound) -> Result<RateBound, NotReady> {
    Ok(match bound {
        cap::Bound::Finite(rate) => RateBound::Finite {
            per_hour: JournalRate::new(rate.get())?,
        },
        cap::Bound::Unbounded => RateBound::Unbounded {},
    })
}

impl State {
    fn cap_outcome(
        &self,
        ccd: CcdId,
        edge: EdgeU,
        layout: &Layout<'_>,
        contents: &BTreeMap<WorkloadId, ContentId>,
        ordinal: u32,
    ) -> Result<CapOutcome, NotReady> {
        let mut interner = Interner::default();
        let mut cells: BTreeMap<ClassId, Vec<cap::Cell>> = BTreeMap::new();
        let mut mapping_secs: BTreeMap<(ClassId, TargetOffset), u64> = BTreeMap::new();
        let mut latest_mapping: Option<Seq> = None;
        let mut classes: Vec<&ClassCap> = self
            .current_groups()
            .filter_map(|group| group.cap.get(&ccd))
            .flatten()
            .collect();
        classes.sort_by_key(|class| class.first);
        for class in classes {
            let id = interner.id(&class.class);
            let merged = cells.entry(id).or_default();
            merged.extend(class.cells.iter().map(|cell| cap::Cell {
                offset: cell.offset,
                purpose: cell.purpose,
                failures: cell.failures,
                hours: Hours::new(cell.hours).unwrap_or(Hours::ZERO),
                exposure: cell.exposure,
            }));
            for (&offset, &secs) in &class.mapping_secs {
                *mapping_secs.entry((id, offset)).or_default() += secs;
            }
            latest_mapping = latest_mapping.max(class.latest_mapping);
        }
        // Every class the mapping matrix runs is listed, observed or not: omission is never safety.
        let seeds = mapping_cells(layout, ccd, TargetOffset::ZERO, ordinal)?;
        for cell in seeds.fresh() {
            if let Some(class) = Self::class_of(cell, contents) {
                let id = interner.id(&class);
                cells.entry(id).or_default();
            }
        }
        let committed = self.nights.caps.get(&ccd);
        let previous = committed.map_or(Slope::Fallback(self.params.fallback_slope), |committed| {
            slope_of(&committed.payload.slope)
        });
        let slope = previous.measure(&cells, LikelihoodCutoff::CHI_SQUARE_95);
        let mapping_changed = committed
            .is_none_or(|committed| latest_mapping.is_some_and(|latest| latest > committed.seq));
        let refresh = match (previous, committed) {
            (Slope::Measured { .. }, Some(committed)) if !mapping_changed => {
                Refresh::NonMappingSlopeOnly {
                    previous: committed.payload.new,
                }
            }
            _ => Refresh::Full,
        };
        let limits = CapLimits {
            machine_budget: self.params.machine_budget,
            ccds: NonZeroU32::new(saturating_u32(
                u64::try_from(self.ccds().len()).unwrap_or(u64::MAX),
            ))
            .unwrap_or(NonZeroU32::MIN),
            edge_margin: EDGE_MARGIN,
        };
        let cap = cap::cap(&cells, slope, edge.offset(), refresh, &limits);
        Ok(CapOutcome {
            cap,
            classes: interner.classes,
            previous,
            mapping_secs,
        })
    }

    fn caps_of(
        &self,
        layout: &Layout<'_>,
        contents: &BTreeMap<WorkloadId, ContentId>,
        edges: &BTreeMap<CcdId, EdgeU>,
        ordinal: u32,
    ) -> Result<BTreeMap<CcdId, Arc<CapOutcome>>, NotReady> {
        self.ccds()
            .into_iter()
            .map(|ccd| {
                let edge = edges.get(&ccd).copied().unwrap_or(EdgeU::Unknown);
                let committed = self.nights.caps.get(&ccd).map(|committed| committed.seq);
                let scoped: Vec<(usize, Option<Seq>)> = self
                    .ledger
                    .groups
                    .iter()
                    .enumerate()
                    .filter(|(_, group)| self.is_current_group(group))
                    .map(|(at, group)| (at, group.latest_scoped.get(&ccd).copied()))
                    .collect();
                if let Some((known, outcome)) = self.caches.caps.lock().get(&ccd)
                    && known.edge == edge
                    && known.ordinal == ordinal
                    && known.committed == committed
                    && known.scoped == scoped
                    && known.partials == *layout.partials
                    && known.contents == *contents
                {
                    return Ok((ccd, Arc::clone(outcome)));
                }
                let outcome = Arc::new(self.cap_outcome(ccd, edge, layout, contents, ordinal)?);
                let key = CapKey {
                    edge,
                    ordinal,
                    committed,
                    scoped,
                    partials: layout.partials.clone(),
                    contents: contents.clone(),
                };
                self.caches
                    .caps
                    .lock()
                    .insert(ccd, (key, Arc::clone(&outcome)));
                Ok((ccd, outcome))
            })
            .collect()
    }

    /// Whether every class of the target's mapping step already has an adequate mapping exposure there.
    fn step_covered(
        &self,
        layout: &Layout<'_>,
        mapped: &Mapped,
        target: &MappingTarget,
    ) -> Result<bool, NotReady> {
        let ordinal = mapped.ordinal_for(target);
        let wanted: HashSet<Sig> = mapping_cells(layout, target.ccd, target.offset, ordinal)?
            .fresh()
            .map(Sig::of_cell)
            .collect();
        let covered: HashSet<Sig> = self
            .current_groups()
            .filter_map(|group| group.mapping_covered.get(&(target.ccd, target.offset)))
            .flatten()
            .cloned()
            .collect();
        Ok(wanted.is_subset(&covered))
    }

    /// The mapping board, every CCD's edge and cap, and the next uncovered mapping step. A step whose cells
    /// are all covered finishes as passed without spending night time.
    pub(super) fn mapping_plan(
        &self,
        layout: &Layout<'_>,
        contents: &BTreeMap<WorkloadId, ContentId>,
    ) -> Result<MappingPlan, NotReady> {
        let mut mapped = self.replay_mapping(layout)?;
        let mut plan = self.settle(layout, contents, &mapped)?;
        for _ in 0..MAX_ALIGN * 4 {
            let Some(target) = plan.target else {
                break;
            };
            if !self.step_covered(layout, &mapped, &target)? {
                break;
            }
            mapped.board.finish_step(target.ccd, StepResult::Passed);
            plan = self.settle(layout, contents, &mapped)?;
        }
        Ok(MappingPlan { mapped, ..plan })
    }

    fn settle(
        &self,
        layout: &Layout<'_>,
        contents: &BTreeMap<WorkloadId, ContentId>,
        mapped: &Mapped,
    ) -> Result<MappingPlan, NotReady> {
        let edges = mapped.edges();
        let caps = self.caps_of(layout, contents, &edges, mapped.next_ordinal)?;
        let limits: BTreeMap<CcdId, TargetOffset> = caps
            .iter()
            .map(|(&ccd, outcome)| (ccd, outcome.cap.offset))
            .collect();
        let target = mapped.board.next_target(&limits);
        Ok(MappingPlan {
            mapped: mapped.clone(),
            edges,
            caps,
            target,
        })
    }

    /// The journal's edge for what the board derived.
    fn journal_edge(plan: &MappingPlan, ccd: CcdId) -> Edge {
        match plan.edges.get(&ccd).copied().unwrap_or(EdgeU::Unknown) {
            EdgeU::Unknown => Edge::Unknown {},
            EdgeU::Observed { offset, witness } => Edge::Observed { offset, witness },
            EdgeU::Censored => Edge::Censored {
                limit: crate::night::write_limit(),
                witnesses: plan
                    .mapped
                    .censored
                    .get(&ccd)
                    .map_or_else(Vec::new, Clone::clone),
            },
        }
    }

    /// A `ccd.edge` owed because U moved.
    pub(super) fn owed_edge(&self, plan: &MappingPlan) -> Option<Decision> {
        self.ccds().into_iter().find_map(|ccd| {
            let edge = Self::journal_edge(plan, ccd);
            let committed = self.nights.edges.get(&ccd);
            let old =
                committed.map_or(Edge::Unknown {}, |committed| committed.payload.edge.clone());
            if edge == old {
                return None;
            }
            let (reason, causes) = match &edge {
                Edge::Unknown {} => ("no failure observed".to_owned(), Vec::new()),
                Edge::Observed { offset, witness } => (
                    format!("first observed failure at {}", offset.get()),
                    vec![*witness],
                ),
                Edge::Censored { limit, witnesses } => (
                    format!("no failure down to the write limit {}", limit.get()),
                    witnesses.clone(),
                ),
            };
            Some(Decision::new(
                PolicyPayload::CcdEdge(CcdEdge {
                    ccd,
                    edge,
                    old,
                    reason,
                }),
                causes,
            ))
        })
    }

    /// The `ccd.cap` of `ccd` as computed now.
    fn cap_payload(&self, plan: &MappingPlan, ccd: CcdId) -> Result<Arc<Option<CcdCap>>, NotReady> {
        let Some(outcome) = plan.caps.get(&ccd) else {
            return Ok(Arc::new(None));
        };
        let committed = self.nights.caps.get(&ccd).map(|committed| committed.seq);
        let edge = Self::journal_edge(plan, ccd);
        if let Some((known, seq, known_edge, payload)) = self.caches.payloads.lock().get(&ccd)
            && Arc::ptr_eq(known, outcome)
            && *seq == committed
            && *known_edge == edge
        {
            return Ok(Arc::clone(payload));
        }
        let payload = Arc::new(self.build_cap_payload(ccd, outcome, &edge)?);
        self.caches.payloads.lock().insert(
            ccd,
            (Arc::clone(outcome), committed, edge, Arc::clone(&payload)),
        );
        Ok(payload)
    }

    fn build_cap_payload(
        &self,
        ccd: CcdId,
        outcome: &CapOutcome,
        edge: &Edge,
    ) -> Result<Option<CcdCap>, NotReady> {
        let cap = &outcome.cap;
        let committed = self.nights.caps.get(&ccd);
        let class = |id: ClassId| outcome.classes[usize_of(id)].clone();
        let beta = cap.slope.beta();
        let budget_rate = CapLimits {
            machine_budget: self.params.machine_budget,
            ccds: NonZeroU32::new(saturating_u32(
                u64::try_from(self.ccds().len()).unwrap_or(u64::MAX),
            ))
            .unwrap_or(NonZeroU32::MIN),
            edge_margin: EDGE_MARGIN,
        }
        .per_ccd_budget();
        let historical_min = match (previous_lower(outcome.previous), cap.slope) {
            (Some(previous), Slope::Measured { .. }) => Some(previous),
            _ => None,
        };
        let mut cells = Vec::new();
        let mut candidates = Vec::new();
        for witness in &cap.witnesses {
            cells.push(RateCell {
                class: class(witness.class),
                offset: witness.offset,
                eligible_s: outcome
                    .mapping_secs
                    .get(&(witness.class, witness.offset))
                    .copied()
                    .unwrap_or(0),
                failures: witness.failures,
                bound: rate_bound(witness.bound)?,
            });
            if let Some(beta) = beta {
                candidates.push(Candidate {
                    class: class(witness.class),
                    offset: candidate_offset(witness, beta, budget_rate),
                });
            }
        }
        let binding = match &cap.binding {
            cap::Binding::Rate { class: id } => Binding::Rate { class: class(*id) },
            cap::Binding::Edge { .. } => Binding::EdgeMargin {},
            cap::Binding::NoAdequateClass
            | cap::Binding::UnknownEdge
            | cap::Binding::NoJustifiedDecay => Binding::Baseline {},
            cap::Binding::Retained { .. } => committed.map_or(Binding::Baseline {}, |committed| {
                committed.payload.binding.clone()
            }),
        };
        if committed.is_none() && cells.is_empty() && outcome_is_unobserved(outcome) {
            return Ok(None);
        }
        Ok(Some(CcdCap {
            ccd,
            old: committed.map_or(TargetOffset::ZERO, |committed| committed.payload.new),
            new: cap.offset,
            budget: JournalRate::new(budget_rate.get())?,
            edge: edge.clone(),
            adequate: cap.witnesses.iter().map(|w| class(w.class)).collect(),
            uncovered: cap.uncovered.iter().map(|u| class(u.class)).collect(),
            cells,
            slope: journal_slope(cap.slope, historical_min)?,
            candidates,
            binding,
        }))
    }

    /// A `ccd.cap` owed because S, its binding, slope, edge, coverage or candidates changed. Eligible-hour
    /// drift alone commits nothing.
    pub(super) fn owed_cap(&self, plan: &MappingPlan) -> Result<Option<Decision>, NotReady> {
        for ccd in self.ccds() {
            let payload = self.cap_payload(plan, ccd)?;
            let Some(desired) = payload.as_ref() else {
                continue;
            };
            let committed = self.nights.caps.get(&ccd);
            let same = committed.is_some_and(|committed| same_cap(&committed.payload, desired));
            if same {
                continue;
            }
            let mut causes: Vec<Seq> = self
                .nights
                .edges
                .get(&ccd)
                .map(|edge| edge.seq)
                .into_iter()
                .collect();
            causes.extend(
                self.current_groups()
                    .filter_map(|group| group.latest_scoped.get(&ccd).copied())
                    .max(),
            );
            return Ok(Some(Decision::new(
                PolicyPayload::CcdCap(desired.clone()),
                causes,
            )));
        }
        Ok(None)
    }

    /// The committed cap S of every CCD; CO 0 until a cap is committed.
    pub(super) fn committed_caps(&self) -> Result<togi_machine::CcdProfile, NotReady> {
        let ccds = self.ccds();
        let mut caps = togi_machine::CcdProfile::zero(ccds.iter().copied())?;
        for ccd in ccds {
            if let Some(cap) = self.nights.caps.get(&ccd) {
                caps = caps.with(ccd, cap.payload.new);
            }
        }
        Ok(caps)
    }
}

fn same_cap(a: &CcdCap, b: &CcdCap) -> bool {
    a.new == b.new
        && a.binding == b.binding
        && a.slope == b.slope
        && a.edge == b.edge
        && a.adequate == b.adequate
        && a.uncovered == b.uncovered
        && a.candidates == b.candidates
}

fn previous_lower(slope: Slope) -> Option<Beta> {
    match slope {
        Slope::Measured { lower } => Some(lower),
        Slope::Fallback(_) | Slope::NoJustifiedDecay(_) => None,
    }
}

/// Whether nothing has been observed for the CCD: only seeded, empty classes.
fn outcome_is_unobserved(outcome: &CapOutcome) -> bool {
    outcome.mapping_secs.is_empty() && outcome.cap.witnesses.is_empty()
}

fn usize_of(id: ClassId) -> usize {
    usize::try_from(id.0).unwrap_or(usize::MAX)
}
