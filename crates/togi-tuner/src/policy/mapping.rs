//! Mapping replay derives isolated edges and the next uncovered mapping step.
//! Statistical limits and their commitments are computed by `statistical`.

use super::State;
use super::books::Trial;
use super::decision::{Decision, PolicyPayload};
use super::ledger::{Sig, is_adequate};
use super::night_query::NotReady;
use super::plan::workloads_of;
use super::statistical::CapOutcome;
use crate::UnitId;
use crate::night::{
    Cursor, EdgeU, IsolatedObservation, IsolatedResult, Layout, Location, MappingBoard,
    MappingTarget, StepResult, mapping_cells,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use togi_journal::Outcome;
use togi_journal::{CcdEdge, Edge, ExposureClass, Seq, Tested};
use togi_machine::{CcdId, ContentId, TargetOffset, WorkloadId};

/// Offsets run 0 to -50, so a cursor can never need more alignment steps than this.
const MAX_ALIGN: usize = 60;

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

/// What one mapping task has seen so far.
#[derive(Clone, Debug)]
struct TaskProgress {
    ccd: CcdId,
    offset: TargetOffset,
    wanted: HashSet<Sig>,
    passed: HashSet<Sig>,
    passes: Vec<Seq>,
}

/// The layout and evidence context a mapping replay is valid for.
#[derive(Clone, Debug, PartialEq)]
struct ReplayKey {
    partials: BTreeMap<CcdId, crate::HonestChain>,
    cores: Vec<togi_machine::CoreInfo>,
    context_since: Option<Seq>,
}

/// The incremental mapping replay and what it is valid for.
#[derive(Clone, Debug)]
pub(super) struct MapReplay {
    key: Option<ReplayKey>,
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
    fn new(layout: &Layout<'_>, ccds: Vec<CcdId>, context_since: Option<Seq>) -> Self {
        Self {
            key: Some(ReplayKey {
                partials: layout.partials.clone(),
                cores: layout.cores.to_vec(),
                context_since,
            }),
            mapped: Mapped {
                board: MappingBoard::new(ccds.iter().copied().map(UnitId::Ccd)),
                ccds,
                censored: BTreeMap::new(),
                open: BTreeMap::new(),
                next_ordinal: 0,
            },
            ..Self::default()
        }
    }

    fn fits(&self, layout: &Layout<'_>, context_since: Option<Seq>) -> bool {
        self.key.as_ref().is_some_and(|key| {
            key.partials == *layout.partials
                && key.cores == layout.cores
                && key.context_since == context_since
        })
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

    /// The mapping board as the current evidence context's mapping tasks and isolated reproduction failures
    /// replay it, in the order the trials ended: an earlier firmware or content leaves the board unmapped
    /// (`docs/spec/tuner.md`, Firmware). Each query folds only the trials that ended since the last one; a
    /// changed layout, which can change what a task's cells are, or a new evidence context replays from the
    /// start.
    pub(super) fn replay_mapping(&self, layout: &Layout<'_>) -> Result<Mapped, NotReady> {
        let mut replay = self.caches.mapping.lock();
        let since = self.machine.context_since;
        if !replay.fits(layout, since) {
            *replay = MapReplay::new(layout, self.ccds(), since);
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
        if task.queue == togi_journal::Queue::Mapping {
            // Task ordinals stay unique across evidence contexts.
            replay.mapped.next_ordinal = replay.mapped.next_ordinal.max(task.ordinal + 1);
        }
        if !self.is_current_trial(trial) {
            return Ok(());
        }
        match task.queue {
            // Reproduction attempts and filler cap probes are isolated observations: they move U but never
            // finish a mapping step.
            togi_journal::Queue::Reproduction | togi_journal::Queue::Filler => {
                if let Some(result) = self.isolated_result(trial, ccd) {
                    replay.mapped.board.observe(IsolatedObservation {
                        unit: UnitId::Ccd(ccd),
                        offset,
                        result,
                    });
                }
            }
            togi_journal::Queue::Mapping => {
                let ordinal = task.ordinal;
                if replay.finished.contains(&ordinal) {
                    return Ok(());
                }
                if let std::collections::hash_map::Entry::Vacant(slot) = replay.tasks.entry(ordinal)
                {
                    let wanted: HashSet<Sig> =
                        mapping_cells(layout, UnitId::Ccd(ccd), offset, ordinal)?
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

    /// Whether the trial ran under the current firmware and workload contents.
    fn is_current_trial(&self, trial: &Trial) -> bool {
        trial.firmware == self.machine.firmware
            && trial.intent.content.iter().all(|entry| {
                self.machine.contents.get(&entry.workload.runs()) == Some(&entry.content)
            })
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
            let Some(mapping) = board.unit(UnitId::Ccd(ccd)) else {
                return false;
            };
            if cursor_offset(mapping.cursor()) == offset.get() {
                return true;
            }
            board.finish_step(UnitId::Ccd(ccd), StepResult::Passed);
            false
        });
        if !reached {
            return;
        }
        let before = board
            .unit(UnitId::Ccd(ccd))
            .map(super::super::night::UnitMapping::edge);
        board.finish_step(UnitId::Ccd(ccd), result);
        let after = board
            .unit(UnitId::Ccd(ccd))
            .map(super::super::night::UnitMapping::edge);
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
            .filter_map(|&ccd| {
                self.board
                    .unit(UnitId::Ccd(ccd))
                    .map(|mapping| (ccd, mapping.edge()))
            })
            .collect()
    }

    /// The ordinal a step at `target` runs under: its open task's, or the next one.
    pub(super) fn ordinal_for(&self, target: &MappingTarget) -> u32 {
        match target.unit.ccd().and_then(|ccd| self.open.get(&ccd)) {
            Some(&(ordinal, offset)) if offset == target.offset => ordinal,
            Some(_) | None => self.next_ordinal,
        }
    }
}

impl State {
    /// Whether every class of the target's mapping step already has an adequate mapping exposure there.
    fn step_covered(
        &self,
        layout: &Layout<'_>,
        mapped: &Mapped,
        target: &MappingTarget,
    ) -> Result<bool, NotReady> {
        let ordinal = mapped.ordinal_for(target);
        let ccd = target.unit.ccd().ok_or(NotReady::NonCoarse)?;
        let wanted: HashSet<Sig> = mapping_cells(layout, target.unit, target.offset, ordinal)?
            .fresh()
            .map(Sig::of_cell)
            .collect();
        let covered: HashSet<Sig> = self
            .current_groups()
            .filter_map(|group| group.mapping_covered.get(&(ccd, target.offset)))
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
            mapped.board.finish_step(target.unit, StepResult::Passed);
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
        let limits: BTreeMap<UnitId, TargetOffset> = caps
            .iter()
            .map(|(&ccd, outcome)| (UnitId::Ccd(ccd), outcome.cap.offset))
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
    pub(super) fn journal_edge(plan: &MappingPlan, ccd: CcdId) -> Edge {
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
}
