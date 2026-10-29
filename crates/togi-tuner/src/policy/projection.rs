//! What status and watch show of the policy, and the `tuner` key of `state.json`: the committed edges and
//! caps, the applied profile and its hold, the events and vetoes, the qualifications and the latest night.

use std::collections::HashMap;
use crate::{UnitId, UnitProfile};
use serde::{Deserialize, Serialize};
use togi_journal::{
    Activity, Binding, Budget, Classification, DayEventId, DayStep, Edge, ExposureClass, NightId,
    PlannedTask, QualificationPurpose, QualificationState, ReproductionState, Resolution, Seq,
    Slope, TaskId, TaskStatus, UnavailableBackend, UncoveredClass, VetoState,
};
use togi_machine::{BootId, CcdId, CoreSet, TargetOffset};

use super::State;
use super::ledger::{Sig, saturating_u32};
use super::qualify::QualStatus;
use crate::day::{Freeze, QuestionState};
use crate::night::Cursor;

/// The cap owner's reason, without the journal's lossy `baseline` label.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CapReason {
    NoAdequateClass,
    UnknownEdge,
    NoJustifiedDecay,
    Edge { rate_limited: TargetOffset },
    Rate { class: ExposureClass },
    Retained { recomputed: TargetOffset },
}

impl CapReason {
    /// A concise label shared by evidence readers, never a safety verdict.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::NoAdequateClass => "no adequate mapping class",
            Self::UnknownEdge => "unknown edge",
            Self::NoJustifiedDecay => "no justified decay",
            Self::Edge { .. } => "edge margin",
            Self::Rate { .. } => "mapping rate bound",
            Self::Retained { .. } => "retained shallower cap",
        }
    }
}

/// A current-context input retained by the slope owner, including non-mapping purposes.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SlopeCell {
    pub class: ExposureClass,
    pub offset: TargetOffset,
    pub purpose: crate::cap::Purpose,
    pub failures: u32,
    pub hours: f64,
}

/// Current read-only calculation, distinct from the last committed S.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CapEvidence {
    pub offset: TargetOffset,
    pub reason: CapReason,
    pub slope: Slope,
    pub budget: crate::cap::Rate,
    pub cells: Vec<togi_journal::RateCell>,
    /// Each rate cell projected at `offset`, in the same order; absent without justified decay.
    pub projected: Vec<togi_journal::RateBound>,
    pub uncovered: Vec<UncoveredClass>,
    /// Observations with no hours and no failures contribute nothing; interrupted empty cells are omitted.
    pub slope_inputs: Vec<SlopeCell>,
}
/// A committed cap S and what produced it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CapProjection {
    pub offset: TargetOffset,
    pub binding: Binding,
    pub slope: Slope,
    pub adequate: Vec<ExposureClass>,
    pub uncovered: Vec<UncoveredClass>,
    /// The `ccd.cap` event.
    pub witness: Seq,
}

/// One tuning group's committed edge U, cap S, applied day offset D and their distance.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct UnitProjection {
    pub unit: UnitId,
    pub cores: CoreSet,
    /// `None` until a `ccd.edge` was committed; absence is not offset zero.
    pub edge: Option<Edge>,
    pub cap: Option<CapProjection>,
    /// Recomputed by the existing cap owner; absence means the evidence context is not queryable.
    #[serde(default)]
    pub evidence: Option<CapEvidence>,
    /// D: CO 0 before any `day.applied`.
    pub day: TargetOffset,
    /// Counts from D down to C; unknown U gives no distance. Statistical S is diagnostic only.
    pub remaining: Option<u32>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EventProjection {
    pub event: DayEventId,
    pub classification: Classification,
    pub resolution: Resolution,
    pub reproduction: ReproductionState,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VetoProjection {
    pub event: DayEventId,
    pub state: VetoState,
}

/// Where a qualification record stands.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum QualificationStage {
    Start,
    Progress,
    Complete,
    Failed,
}

/// A qualification record and where it stands.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QualificationProjection {
    pub profile: UnitProfile,
    pub purpose: QualificationPurpose,
    pub state: QualificationStage,
    /// Whether it was made under the current firmware, epoch and content.
    pub current: bool,
    /// Measured task exposure plus cited reuse, deduplicated; includes partials and failures, not cells.
    #[serde(default)]
    pub eligible_s: u64,
}

/// The applied-hour hold at the current D.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HoldProjection {
    pub profile: UnitProfile,
    pub eligible_s: u64,
    pub required_s: u64,
    /// The `day.hold` event.
    pub witness: Seq,
}

/// Whether the committed firmware context still matches the one D was applied under.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FirmwareValidity {
    Valid,
    /// The context changed at `since` and D has not been reset yet.
    Invalidated {
        since: Seq,
    },
    /// No firmware context was reported yet.
    Unknown,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DayProjection {
    pub hold: Option<HoldProjection>,
    pub last_step: Option<(Seq, DayStep)>,
    pub freeze: bool,
    pub firmware: FirmwareValidity,
    /// Boots whose crash question is still pending.
    pub pending_questions: Vec<BootId>,
}

/// Where one CCD's mapping descent stands.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MappingProjection {
    pub unit: UnitId,
    /// The offset in progress.
    pub cursor: Option<TargetOffset>,
    /// The first failing coarse step and the bracket offset in progress.
    pub bracket: Option<(TargetOffset, TargetOffset)>,
}

/// The open qualification's progress.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct QualificationProgress {
    pub profile: UnitProfile,
    pub outstanding: Vec<ExposureClass>,
    pub witnessed: usize,
    #[serde(default)]
    pub eligible_s: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReproductionProjection {
    pub source: DayEventId,
    pub state: ReproductionState,
    pub consumed_s: u64,
    pub remaining: Vec<ExposureClass>,
}

/// The latest plan of the latest night.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NightProjection {
    pub night: NightId,
    /// When the plan that opened the night was committed.
    pub started: jiff::Timestamp,
    /// The event that closed the night (a later boot's `day.boot_start`, the operator's signal stop or a
    /// session reset); `None` while it is open.
    pub closed_by: Option<Seq>,
    pub budgets: Vec<Budget>,
    pub tasks: Vec<PlannedTask>,
    /// The running task and its activity. Filler repeats exact-current-D qualification cells;
    /// its activity is unknown before its first trial.
    pub current: Option<(TaskId, Option<Activity>)>,
    pub mapping: Vec<MappingProjection>,
    pub qualification: Option<QualificationProgress>,
    pub reproductions: Vec<ReproductionProjection>,
    /// The backends the night runs no cell on and the events that excluded each, as its plan records them.
    pub unavailable: Vec<UnavailableBackend>,
    /// The durable filler cursor: how many filler trials ran across every night, which rotates the next
    /// filler cell.
    pub filler: u32,
}

/// The `tuner` key of `state.json` and what `status` and `watch` read.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Projection {
    #[serde(default)]
    pub climb: super::ClimbProjection,
    pub applied: Option<UnitProfile>,
    pub units: Vec<UnitProjection>,
    pub day: DayProjection,
    pub events: Vec<EventProjection>,
    pub vetoes: Vec<VetoProjection>,
    pub qualifications: Vec<QualificationProjection>,
    pub night: Option<NightProjection>,
    /// A conclusive failure at all-zero ended the run.
    pub stopped: bool,
}

/// Which recorded profile an expanded evidence reader should inspect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvidenceProfile<'a> {
    Applied(&'a UnitProfile),
    NightQualified(&'a UnitProfile),
    Baseline(&'a UnitProfile),
}

impl Projection {
    /// A nonzero day profile takes priority; baseline stock must not hide completed night work.
    #[must_use]
    pub fn evidence_profile(&self) -> Option<EvidenceProfile<'_>> {
        if let Some(profile) = self.applied.as_ref().filter(|profile| !profile.is_zero()) {
            return Some(EvidenceProfile::Applied(profile));
        }
        self.qualifications
            .iter()
            .rev()
            .find(|record| {
                record.current
                    && record.state == QualificationStage::Complete
                    && !record.profile.is_zero()
            })
            .map(|record| EvidenceProfile::NightQualified(&record.profile))
            .or_else(|| self.applied.as_ref().map(EvidenceProfile::Baseline))
    }
}

const fn activity_of(queue: togi_journal::Queue) -> Option<Activity> {
    match queue {
        togi_journal::Queue::Reproduction => Some(Activity::Reproduction),
        togi_journal::Queue::Qualification | togi_journal::Queue::Requalification => {
            Some(Activity::Qualification)
        }
        togi_journal::Queue::Mapping => Some(Activity::Mapping),
        togi_journal::Queue::Control => Some(Activity::Control),
        togi_journal::Queue::Filler => None,
    }
}

fn offset(value: i64) -> TargetOffset {
    TargetOffset::clamped(value)
}

impl State {
    /// The committed state as status shows it. Pure: it reads only what the journal committed, plus the
    /// mapping cursors and the open qualification the same journal derives.
    #[must_use]
    pub fn project(&self) -> Projection {
        let mut events: Vec<_> = self.days.events.values().collect();
        events.sort_by_key(|book| book.first);
        let mut vetoes: Vec<_> = self.days.vetoes.values().collect();
        vetoes.sort_by_key(|commit| commit.seq);
        let applied = self
            .days
            .history
            .as_ref()
            .map(|history| history.current().profile.clone());
        Projection {
            climb: self.climb_projection(),
            units: self.unit_projections(applied.as_ref()),
            applied,
            day: self.day_projection(),
            events: events
                .into_iter()
                .map(|book| {
                    let event = &book.latest.payload;
                    EventProjection {
                        event: event.event.clone(),
                        classification: event.classification,
                        resolution: event.resolution,
                        reproduction: event.reproduction,
                    }
                })
                .collect(),
            vetoes: vetoes
                .into_iter()
                .map(|commit| VetoProjection {
                    event: commit.payload.event.clone(),
                    state: commit.payload.state,
                })
                .collect(),
            qualifications: self
                .qualification_records()
                .iter()
                .map(|record| QualificationProjection {
                    profile: UnitProfile::of_ccd(record.start.payload.profile.ccd()),
                    purpose: record.start.payload.purpose,
                    state: match record.latest.payload.state {
                        QualificationState::Start { .. } => QualificationStage::Start,
                        QualificationState::Progress { .. } => QualificationStage::Progress,
                        QualificationState::Complete { .. } => QualificationStage::Complete,
                        QualificationState::Failed { .. } => QualificationStage::Failed,
                    },
                    current: self.is_current_record(record),
                    eligible_s: self.qualification_seconds(record),
                })
                .collect(),
            night: self.night_projection(),
            stopped: self.stop.is_some(),
        }
    }

    /// Selected/stock exposure witnesses in journal order. The ledger owns original-identity
    /// deduplication, `Sig` owns class identity and `GroupKey` owns context compatibility. Each selected
    /// observation uses the latest stock observation of its class and context, including historical
    /// contexts rather than silently treating them as current evidence.
    #[must_use]
    pub fn comparable_exposures(&self, profile: &UnitProfile) -> Vec<(Seq, Seq)> {
        if profile.is_zero() {
            return Vec::new();
        }
        let mut stocks: HashMap<Sig, Vec<(&super::evidence::GroupKey, Seq)>> = HashMap::new();
        for record in &self.ledger.records {
            let observed = &record.observed;
            if !observed.profile.ccd().is_zero() {
                continue;
            }
            let Some(group) = self
                .ledger
                .groups
                .iter()
                .find(|group| group.key.matches(observed))
            else {
                continue;
            };
            let contexts = stocks.entry(Sig::of_class(&observed.class)).or_default();
            if let Some((_, seq)) = contexts.iter_mut().find(|(key, _)| *key == &group.key) {
                *seq = record.seq;
            } else {
                contexts.push((&group.key, record.seq));
            }
        }
        self.ledger
            .records
            .iter()
            .filter(|record| profile.matches_ccd(record.observed.profile.ccd()))
            .filter_map(|record| {
                let observed = &record.observed;
                let contexts = stocks.get(&Sig::of_class(&observed.class))?;
                let (_, stock) = contexts.iter().find(|(key, _)| key.matches(observed))?;
                Some((record.seq, *stock))
            })
            .collect()
    }

    fn unit_projections(&self, applied: Option<&UnitProfile>) -> Vec<UnitProjection> {
        let known = self.known_contents(None);
        let plan = self
            .with_layout(|layout| self.mapping_plan(layout, &known))
            .ok();
        let Some(topology) = self.machine.topology.as_ref() else {
            return Vec::new();
        };
        topology
            .ccds()
            .iter()
            .map(|&(ccd, cores)| {
                let day = applied
                    .and_then(|profile| profile.get(UnitId::Ccd(ccd)))
                    .unwrap_or(TargetOffset::ZERO);
                let cap = self.nights.caps.get(&ccd);
                UnitProjection {
                    unit: UnitId::Ccd(ccd),
                    cores,
                    evidence: plan
                        .as_ref()
                        .and_then(|plan| self.cap_evidence(ccd, plan.caps.get(&ccd)?).ok()),
                    edge: self
                        .nights
                        .edges
                        .get(&ccd)
                        .map(|edge| edge.payload.edge.clone()),
                    remaining: self.ceilings().get(&UnitId::Ccd(ccd)).and_then(|ceiling| ceiling.c).map(|ceiling| {
                        saturating_u32(day.get().saturating_sub(ceiling.get()).max(0).unsigned_abs())
                    }),
                    cap: cap.map(|cap| CapProjection {
                        offset: cap.payload.new,
                        binding: cap.payload.binding.clone(),
                        slope: cap.payload.slope,
                        adequate: cap.payload.adequate.clone(),
                        uncovered: cap.payload.uncovered.clone(),
                        witness: cap.seq,
                    }),
                    day,
                }
            })
            .collect()
    }

    fn day_projection(&self) -> DayProjection {
        let firmware = match (
            &self.machine.firmware,
            &self.machine.anchor,
            self.machine.changed_at,
        ) {
            (None, _, _) | (Some(_), None, _) => FirmwareValidity::Unknown,
            (Some(current), Some(anchor), Some(since)) if current != anchor => {
                FirmwareValidity::Invalidated { since }
            }
            (Some(current), Some(anchor), None) if current != anchor => FirmwareValidity::Unknown,
            (Some(_), Some(_), _) => FirmwareValidity::Valid,
        };
        let pending_questions = self
            .day_view()
            .map(|view| {
                view.events
                    .iter()
                    .filter(|event| event.question == QuestionState::Pending)
                    .filter_map(|event| match &event.id {
                        crate::day::EventId::Boot(boot) => Some(boot.clone()),
                        crate::day::EventId::Unobserved(_)
                        | crate::day::EventId::Requalification(_)
                        | crate::day::EventId::Imported(_) => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        DayProjection {
            hold: self.days.holds.last().and_then(|hold| {
                let profile = self.days.history.as_ref()?.current().profile.clone();
                Some(HoldProjection {
                    profile,
                    eligible_s: hold.payload.eligible_s,
                    required_s: self.params.hold.hold_secs,
                    witness: hold.seq,
                })
            }),
            last_step: self
                .days
                .steps
                .values()
                .next_back()
                .map(|step| (step.seq, step.payload.clone())),
            freeze: self.days.freeze == Freeze::On,
            firmware,
            pending_questions,
        }
    }

    fn night_projection(&self) -> Option<NightProjection> {
        let (night, book) = self.nights.plans.iter().next_back()?;
        let plan = &book.latest.payload;
        let known = self.known_contents(None);
        let mapping = self
            .with_layout(|layout| self.mapping_plan(layout, &known))
            .map(|plan| {
                self.ccds()
                    .into_iter()
                    .filter_map(|ccd| {
                        let mapping = plan.mapped.board.unit(UnitId::Ccd(ccd))?;
                        let (cursor, bracket) = match mapping.cursor() {
                            Cursor::Coarse { offset: at } => (Some(offset(at)), None),
                            Cursor::Sweep { next } => (Some(offset(next)), None),
                            Cursor::Bracket { failed, next } => {
                                (Some(offset(next)), Some((offset(failed), offset(next))))
                            }
                        };
                        Some(MappingProjection {
                            unit: UnitId::Ccd(ccd),
                            cursor,
                            bracket,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let qualification = self.with_layout(|_| Ok(())).ok().and_then(|()| {
            self.qualification_records()
                .iter()
                .rev()
                .find_map(|record| {
                    if !self.is_current_record(record) {
                        return None;
                    }
                    match &*self.qualification_status(record).ok()? {
                        QualStatus::Open {
                            witnesses,
                            outstanding,
                            ..
                        } => Some(QualificationProgress {
                            profile: UnitProfile::of_ccd(record.start.payload.profile.ccd()),
                            outstanding: outstanding.clone(),
                            witnessed: witnesses.len(),
                            eligible_s: self.record_seconds(record, witnesses),
                        }),
                        QualStatus::Stale
                        | QualStatus::Complete { .. }
                        | QualStatus::Failed { .. } => None,
                    }
                })
        });
        let mut reproductions: Vec<_> = self.nights.reproductions.iter().collect();
        reproductions.sort_by_key(|commit| commit.seq);
        let mut latest: Vec<ReproductionProjection> = Vec::new();
        for commit in reproductions {
            let entry = ReproductionProjection {
                source: commit.payload.source.clone(),
                state: commit.payload.state,
                consumed_s: commit.payload.consumed_s,
                remaining: commit.payload.remaining.clone(),
            };
            match latest.iter_mut().find(|known| known.source == entry.source) {
                Some(known) => *known = entry,
                None => latest.push(entry),
            }
        }
        Some(NightProjection {
            night: *night,
            started: book.first.time,
            closed_by: book.closed_by,
            budgets: plan.budgets.clone(),
            current: plan
                .tasks
                .iter()
                .find(|task| task.status == TaskStatus::Running)
                .map(|task| {
                    let latest = self
                        .trials
                        .by_task
                        .get(&task.task)
                        .and_then(|trials| trials.last())
                        .and_then(|&at| self.trials.all.get(at))
                        .and_then(|trial| trial.intent.activity);
                    (task.task, activity_of(task.task.queue).or(latest))
                }),
            tasks: plan.tasks.clone(),
            mapping,
            qualification,
            reproductions: latest,
            unavailable: plan.unavailable.clone(),
            filler: self.trials.filler_cursor(),
        })
    }
    fn record_seconds(
        &self,
        record: &super::qualify::QualRecord,
        witnesses: &[togi_journal::CellWitness],
    ) -> u64 {
        let mut cited: std::collections::BTreeSet<_> = witnesses
            .iter()
            .map(|witness| match witness.evidence {
                togi_journal::CellEvidence::Fresh { exposure }
                | togi_journal::CellEvidence::Reused { exposure } => exposure,
            })
            .collect();
        if let Some(exposures) = self.ledger.by_task.get(&record.task()) {
            cited.extend(exposures.iter().map(|&at| self.ledger.records[at].seq));
        }
        // Exposure commitments append in journal order; do not rescan the whole ledger per qualification.
        cited
            .into_iter()
            .filter_map(|seq| {
                self.ledger
                    .records
                    .binary_search_by_key(&seq, |record| record.seq)
                    .ok()
                    .map(|at| &self.ledger.records[at])
            })
            .fold(0_u64, |total, record| {
                total.saturating_add(record.observed.eligible_s)
            })
    }

    fn qualification_seconds(&self, record: &super::qualify::QualRecord) -> u64 {
        let witnesses = match &record.latest.payload.state {
            QualificationState::Start { .. } => &[][..],
            QualificationState::Progress { witnesses, .. }
            | QualificationState::Complete { witnesses }
            | QualificationState::Failed { witnesses, .. } => witnesses,
        };
        self.record_seconds(record, witnesses)
    }

    fn cap_evidence(
        &self,
        ccd: CcdId,
        outcome: &super::statistical::CapOutcome,
    ) -> Result<CapEvidence, super::NotReady> {
        use super::statistical::{rate_bound, usize_of};
        use crate::cap;
        let cap = &outcome.cap;
        let class = |id| outcome.classes[usize_of(id)].clone();
        let reason = match cap.binding {
            cap::Binding::NoAdequateClass => CapReason::NoAdequateClass,
            cap::Binding::UnknownEdge => CapReason::UnknownEdge,
            cap::Binding::NoJustifiedDecay => CapReason::NoJustifiedDecay,
            cap::Binding::Edge { rate_limited } => CapReason::Edge { rate_limited },
            cap::Binding::Rate { class: id } => CapReason::Rate { class: class(id) },
            cap::Binding::Retained { recomputed } => CapReason::Retained { recomputed },
        };
        let cells = cap
            .witnesses
            .iter()
            .map(|witness| {
                Ok(togi_journal::RateCell {
                    class: class(witness.class),
                    offset: witness.offset,
                    eligible_s: outcome
                        .mapping_secs
                        .get(&(witness.class, witness.offset))
                        .copied()
                        .unwrap_or(0),
                    failures: witness.failures,
                    bound: rate_bound(witness.bound)?,
                })
            })
            .collect::<Result<Vec<_>, super::NotReady>>()?;
        let projected = cap
            .slope
            .beta()
            .map(|beta| {
                cap.witnesses
                    .iter()
                    .map(|witness| rate_bound(cap::project(witness, beta, cap.offset)))
                    .collect::<Result<Vec<_>, super::NotReady>>()
            })
            .transpose()?
            .unwrap_or_default();
        let slope = super::statistical::journal_slope(cap.slope, None)?;
        let uncovered = cap
            .uncovered
            .iter()
            .map(|gap| UncoveredClass {
                class: class(gap.class),
                reason: match gap.reason {
                    cap::UncoveredReason::NoObservations => {
                        togi_journal::UncoveredReason::NoObservations
                    }
                    cap::UncoveredReason::NonMappingOnly => {
                        togi_journal::UncoveredReason::NonMappingOnly
                    }
                    cap::UncoveredReason::MappingInadequate => {
                        togi_journal::UncoveredReason::MappingInadequate
                    }
                },
            })
            .collect();
        let slope_inputs = self
            .current_groups()
            .filter_map(|group| group.cap.get(&ccd))
            .flatten()
            .flat_map(|class| {
                class
                    .cells
                    .iter()
                    .filter(|cell| cell.hours > 0.0 || cell.failures > 0)
                    .map(|cell| SlopeCell {
                        class: class.class.clone(),
                        offset: cell.offset,
                        purpose: cell.purpose,
                        failures: cell.failures,
                        hours: cell.hours,
                    })
            })
            .collect();
        Ok(CapEvidence {
            offset: cap.offset,
            reason,
            slope,
            budget: outcome.budget,
            cells,
            projected,
            uncovered,
            slope_inputs,
        })
    }
}
