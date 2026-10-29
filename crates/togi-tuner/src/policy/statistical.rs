//! Non-authorizing statistical diagnostics and `ccd.cap` commitments, independent of scheduling.

use super::decision::{Decision, PolicyPayload};
use super::evidence::ClassCap;
use super::ledger::{Sig, saturating_u32};
use super::night_query::NotReady;
use super::{State, mapping::MappingPlan};
use crate::cap::{
    self, Beta, CapLimits, EDGE_MARGIN, Hours, LikelihoodCutoff, Rate, Refresh, Slope,
};
use crate::night::{EdgeU, Layout, mapping_cells};
use crate::{UnitId, UnitProfile};
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU32;
use std::sync::Arc;
use togi_journal::{
    Binding, Candidate, CcdCap, Edge, ExposureClass, Finite, LowerEndpoint, Rate as JournalRate,
    RateBound, RateCell, Seq, UncoveredClass, UncoveredReason,
};
use togi_machine::{CcdId, ContentId, TargetOffset, WorkloadId};

/// A class of the interned cap input; the cap's classes must be ordered, an exposure class is not.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct ClassId(u32);
/// The diagnostic S of one CCD and what produced it; never a scheduling or target permission.
#[derive(Clone, Debug)]
pub(super) struct CapOutcome {
    pub(super) cap: cap::Cap<ClassId>,
    pub(super) classes: Vec<ExposureClass>,
    pub(super) previous: Slope,
    /// Eligible seconds per mapping class and offset, for the journal's rate cells.
    pub(super) mapping_secs: BTreeMap<(ClassId, TargetOffset), u64>,
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
        let seeds = mapping_cells(layout, UnitId::Ccd(ccd), TargetOffset::ZERO, ordinal)?;
        for cell in seeds.fresh() {
            if let Some(class) = Self::class_of(cell, contents) {
                let id = interner.id(&class);
                cells.entry(id).or_default();
            }
        }
        // A cap committed under an earlier evidence context carries no slope or cap forward
        // (`docs/spec/tuner.md`, Firmware).
        let committed = self.nights.caps.get(&ccd).filter(|committed| {
            self.machine
                .context_since
                .is_none_or(|since| committed.seq >= since)
        });
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

    pub(super) fn caps_of(
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
            uncovered: cap
                .uncovered
                .iter()
                .map(|uncovered| UncoveredClass {
                    class: class(uncovered.class),
                    reason: uncovered_reason(uncovered.reason),
                })
                .collect(),
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
    pub(super) fn committed_caps(&self) -> Result<UnitProfile, NotReady> {
        let ccds = self.ccds();
        let mut caps = togi_machine::CcdProfile::zero(ccds.iter().copied())?;
        for ccd in ccds {
            if let Some(cap) = self.nights.caps.get(&ccd) {
                caps = caps.with(ccd, cap.payload.new);
            }
        }
        Ok(UnitProfile::of_ccd(&caps))
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

fn uncovered_reason(reason: cap::UncoveredReason) -> UncoveredReason {
    match reason {
        cap::UncoveredReason::NoObservations => UncoveredReason::NoObservations,
        cap::UncoveredReason::NonMappingOnly => UncoveredReason::NonMappingOnly,
        cap::UncoveredReason::MappingInadequate => UncoveredReason::MappingInadequate,
    }
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
