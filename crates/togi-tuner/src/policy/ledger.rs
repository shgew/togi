//! The exposure ledger: what each class of trial was exposed to, derived once per ended trial from its
//! journaled intent, start and end, and matched to matrix cells by class signature.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use jiff::Timestamp;
use togi_journal::{
    Activity, Attribution, CreditExclusion, ExposureClass, ExposureFailure, ExposureObserved,
    ExposureOrigin, Failure, FailureTime, OriginalSource, Outcome, Placement, Position, Seq,
    SessionId, TaskId, Tested, TunerWarning, WarningKind, WorkloadContent,
};
use togi_machine::{
    BiosContext, CcdId, CcdProfile, CoreId, CoreSet, PhaseSchedule, Regime, Signal, TargetOffset,
    WorkloadId,
};

use super::State;
use super::books::{Exposure, Trial};
use super::decision::{Decision, PolicyPayload};
use super::plan::workloads_of;
use crate::EVIDENCE_EPOCH;
use crate::Secs;
use crate::cap;
use crate::night::{
    Cell, Conclusion, Credit, Launch, PlacementValidity, Purpose, Roles, adequately_exposed,
    warm_up_secs,
};

/// The evidence context a monotonicity cache was built under.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct WarningContext {
    session: Option<SessionId>,
    since: Option<Seq>,
    firmware: Option<BiosContext>,
    contents: usize,
}

/// The unwarned isolated failures, sorted by whether the current evidence has a deeper pass of their class.
#[derive(Clone, Debug, Default)]
pub(super) struct Monotonicity {
    context: Option<WarningContext>,
    /// How many ledger records and ended trials it has read.
    records: usize,
    ended: usize,
    /// The failures without a deeper pass yet, by CCD and class: each one's offset and trial index.
    waiting: HashMap<(CcdId, Sig), Vec<(TargetOffset, usize)>>,
    /// The failures with a deeper pass, by trial index.
    warnable: BTreeSet<usize>,
}

/// What makes two trials the same class: the matrix cell and the journal's exposure class both reduce
/// to it, so a cell is matched to its observations without storing cells in the journal.
#[derive(Clone, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Sig {
    regime: Regime,
    workloads: Vec<WorkloadId>,
    cores: CoreSet,
    position: Option<Position>,
    duration_s: u32,
    placement: Placement,
}

impl Sig {
    pub(super) const fn position(&self) -> Option<Position> {
        self.position
    }

    pub(super) const fn placement(&self) -> Placement {
        self.placement
    }

    pub(super) fn of_class(class: &ExposureClass) -> Self {
        Self {
            regime: class.regime,
            workloads: class
                .workloads
                .iter()
                .map(|content| content.workload.runs())
                .collect(),
            cores: class.cores,
            position: class.position,
            duration_s: class.duration_s,
            placement: class.placement,
        }
    }

    pub(super) fn of_cell(cell: &Cell) -> Self {
        Self {
            regime: cell.regime(),
            workloads: workloads_of(cell.workloads)
                .into_iter()
                .map(WorkloadId::runs)
                .collect(),
            cores: cell.loaded,
            position: match cell.roles {
                Roles::Neighbours { light } => Some(Position::Light { core: light }),
                Roles::AllThreads { serial, .. } => Some(Position::Serial { cpu: serial }),
                Roles::SingleCore { .. }
                | Roles::SmtPair { .. }
                | Roles::AllCoresIdle
                | Roles::Package(_) => None,
            },
            duration_s: cell.duration.get(),
            placement: match cell.purpose {
                Purpose::Isolated => Placement::Isolated,
                Purpose::Exact => Placement::Exact,
            },
        }
    }
}

/// Whole wall seconds from `from` to `to`; a clock that ran backwards counts as none.
pub(super) fn wall_secs(from: Timestamp, to: Timestamp) -> u64 {
    (to.as_second() - from.as_second()).max(0).unsigned_abs()
}

/// The cap purpose of a trial activity; the old per-core activities carry no Ruleset 13 evidence.
pub(super) const fn cap_purpose(activity: Activity) -> Option<cap::Purpose> {
    match activity {
        Activity::Mapping => Some(cap::Purpose::Mapping),
        Activity::Qualification => Some(cap::Purpose::Qualification),
        Activity::Reproduction => Some(cap::Purpose::Reproduction),
        Activity::Control => Some(cap::Purpose::Control),
        Activity::Search | Activity::Checking | Activity::Hunt | Activity::Deepening => None,
    }
}

/// The physical core an exposure class tests, when it tests one.
pub(super) fn position_core(
    class: &ExposureClass,
    cores: &[togi_machine::CoreInfo],
) -> Option<CoreId> {
    match class.position {
        Some(Position::Light { core }) => Some(core),
        Some(Position::Serial { cpu }) => cores
            .iter()
            .find(|info| info.cpus.contains(&cpu))
            .map(|info| info.core),
        None => (class.cores.len() == 1)
            .then(|| class.cores.first())
            .flatten(),
    }
}

/// Whether the recorded exposure is an adequate one: a conclusive pass that ran its whole intended
/// duration with valid placement and the tested position credited.
pub(super) fn is_adequate(observed: &ExposureObserved, cores: &[togi_machine::CoreInfo]) -> bool {
    let Some(intended) = std::num::NonZeroU32::new(observed.class.duration_s).map(Secs::new) else {
        return false;
    };
    let regime = observed.class.regime;
    let withheld = observed
        .exclusions
        .iter()
        .any(|exclusion| matches!(exclusion, CreditExclusion::PlacementWithheld {}));
    let host = position_core(&observed.class, cores).is_some_and(|position| {
        observed
            .exclusions
            .iter()
            .any(|exclusion| matches!(exclusion, CreditExclusion::HostCore { core } if *core == position))
    });
    let ran = saturating_u32(observed.eligible_s).saturating_add(warm_up_secs(regime));
    adequately_exposed([&Launch {
        regime,
        intended,
        ran,
        conclusion: match observed.outcome {
            Outcome::Pass => Conclusion::Conclusive,
            Outcome::Failure | Outcome::Inconclusive => Conclusion::Inconclusive,
        },
        placement: if withheld {
            PlacementValidity::Withheld
        } else {
            PlacementValidity::Valid
        },
        credit: if host {
            Credit::Withheld
        } else {
            Credit::Granted
        },
    }])
}

impl State {
    /// Whether the exposure holds under the current firmware, epoch and workload content.
    pub(super) fn is_current(&self, observed: &ExposureObserved) -> bool {
        self.machine.firmware.as_ref() == Some(&observed.firmware)
            && observed.epoch == EVIDENCE_EPOCH
            && observed.class.workloads.iter().all(|content| {
                self.machine.contents.get(&content.workload.runs()) == Some(&content.content)
            })
    }

    /// The current exposures of one task.
    pub(super) fn task_exposures(&self, task: TaskId) -> impl Iterator<Item = &Exposure> {
        self.ledger
            .by_task
            .get(&task)
            .into_iter()
            .flatten()
            .filter_map(|&index| self.ledger.records.get(index))
            .filter(|record| self.is_current(&record.observed))
    }

    /// The `exposure.observed` of an ended Ruleset 13 trial, from its journaled intent, start and end.
    pub(super) fn derive_exposure(&self, trial: &Trial) -> Option<ExposureObserved> {
        let ended = trial.end.as_ref()?;
        let intent = &trial.intent;
        let session = self.machine.session?;
        let activity = intent.activity?;
        let tested = intent.tested?;
        let placement = intent.placement?;
        let ccd = CcdProfile::of_cores(&intent.profile, &self.machine.cores)?;
        let regime = intent.regime;
        let warm = warm_up_secs(regime);
        let ran = ended.end.duration_s;
        let eligible = ran.min(intent.duration_s).saturating_sub(warm);
        let cores: CoreSet = if intent.cores.is_empty() {
            intent.core.into_iter().collect()
        } else {
            intent.cores
        };
        let position = match regime {
            Regime::R8 => intent.core.map(|core| Position::Light { core }),
            Regime::R9 => trial.start.as_ref().and_then(|(_, start)| {
                start.roles.iter().find_map(|role| match role.schedule {
                    PhaseSchedule::Alternate { serial: true, .. } => {
                        Some(Position::Serial { cpu: role.cpu })
                    }
                    PhaseSchedule::Alternate { serial: false, .. }
                    | PhaseSchedule::Sustained {}
                    | PhaseSchedule::Steps { .. }
                    | PhaseSchedule::Burst { .. } => None,
                })
            }),
            Regime::R1
            | Regime::R2
            | Regime::R3
            | Regime::R4
            | Regime::R5
            | Regime::R6
            | Regime::R7 => None,
        };
        let failures = match ended.end.outcome {
            Outcome::Failure => vec![ExposureFailure {
                // A failed trial that names no signal is counted as the crash it most likely was.
                signal: ended.end.signal.unwrap_or(Signal::Crash),
                core: ended.end.core,
                time: if ran < warm {
                    FailureTime::WarmUp
                } else {
                    FailureTime::Eligible
                },
            }],
            Outcome::Pass | Outcome::Inconclusive => Vec::new(),
        };
        let mut exclusions: Vec<CreditExclusion> = ended
            .end
            .excluded
            .iter()
            .map(|&core| CreditExclusion::HostCore { core })
            .collect();
        if ended.end.containment_error.is_some()
            || !ended.end.escaped.is_empty()
            || ended.end.backend_missing
        {
            exclusions.push(CreditExclusion::PlacementWithheld {});
        }
        Some(ExposureObserved {
            source: OriginalSource {
                session,
                seq: trial.intent_seq,
                boot: trial.boot.clone(),
                trial: Some(intent.trial.clone()),
            },
            origin: ExposureOrigin::Live {},
            firmware: trial.firmware.clone()?,
            epoch: EVIDENCE_EPOCH,
            class: ExposureClass {
                regime,
                workloads: intent.content.clone(),
                cores,
                position,
                duration_s: intent.duration_s,
                placement,
            },
            profile: togi_journal::CcdVector {
                ccd,
                cores: intent.profile,
            },
            tested,
            activity,
            outcome: ended.end.outcome,
            failures,
            eligible_s: u64::from(eligible),
            uninterrupted_s: if ended.end.interrupted {
                0
            } else {
                u64::from(eligible)
            },
            exclusions,
        })
    }

    /// The `failure` an ended failing trial still owes.
    pub(super) fn owed_failure(&self) -> Option<Decision> {
        let trial = self
            .trials
            .unreported_failures
            .first()
            .and_then(|&at| self.trials.all.get(at))?;
        let ended = trial.end.as_ref()?;
        let intent = &trial.intent;
        Some(Decision::new(
            PolicyPayload::Failure(Failure {
                signal: ended.end.signal.unwrap_or(Signal::Crash),
                attribution: if ended.end.core.is_some() {
                    Attribution::Attributed
                } else {
                    Attribution::Unattributed
                },
                core: ended.end.core,
                offset: match intent.tested {
                    Some(Tested::Isolated { offset, .. }) => Some(offset),
                    Some(Tested::Exact {}) | None => None,
                },
                trial: Some(intent.trial.clone()),
                regime: Some(intent.regime),
                condition: Some(intent.condition),
                profile: Some(intent.profile),
                known_failure: None,
                reason: ended.end.reason.clone(),
                round: None,
            }),
            vec![trial.intent_seq, ended.seq],
        ))
    }

    /// The `exposure.observed` an ended Ruleset 13 trial still owes, citing its intent, end and failure.
    pub(super) fn owed_exposure(&self) -> Option<Decision> {
        self.trials
            .unobserved
            .iter()
            .filter_map(|&at| self.trials.all.get(at))
            .find_map(|trial| {
                let observed = self.derive_exposure(trial)?;
                let ended = trial.end.as_ref()?;
                let mut causes = vec![trial.intent_seq, ended.seq];
                causes.extend(trial.failure);
                Some(Decision::new(
                    PolicyPayload::ExposureObserved(observed),
                    causes,
                ))
            })
    }

    /// The monotonicity warning owed for the earliest unwarned isolated failure whose class passed
    /// adequately at a deeper offset on its CCD. The candidates are kept incrementally: each failure is
    /// checked once when it ends, and a waiting one is released by the first deeper pass folded after it.
    pub(super) fn owed_warning(&self) -> Option<Decision> {
        let mut cache = self.caches.warnings.lock();
        let context = WarningContext {
            session: self.machine.session,
            since: self.machine.context_since,
            firmware: self.machine.firmware.clone(),
            contents: self.machine.contents.len(),
        };
        if cache.context.as_ref() != Some(&context) {
            *cache = Monotonicity {
                context: Some(context),
                records: self.ledger.records.len(),
                ..Monotonicity::default()
            };
        }
        let Monotonicity {
            records,
            ended,
            waiting,
            warnable,
            ..
        } = &mut *cache;
        for record in self.ledger.records.get(*records..).unwrap_or_default() {
            let observed = &record.observed;
            let Tested::Isolated { ccd, offset } = observed.tested else {
                continue;
            };
            if !is_adequate(observed, &self.machine.cores) || !self.is_current_exposure(observed) {
                continue;
            }
            if let Some(failures) = waiting.get_mut(&(ccd, Sig::of_class(&observed.class))) {
                failures.retain(|&(failed, at)| {
                    let released = offset < failed;
                    if released {
                        warnable.insert(at);
                    }
                    !released
                });
            }
        }
        *records = self.ledger.records.len();
        for &at in self.trials.ended_order.get(*ended..).unwrap_or_default() {
            if !self.trials.unwarned.contains(&at) {
                continue;
            }
            let Some(trial) = self.trials.all.get(at) else {
                continue;
            };
            let Some(Tested::Isolated { ccd, offset }) = trial.intent.tested else {
                continue;
            };
            let Some(failed) = self.derive_exposure(trial) else {
                continue;
            };
            let sig = Sig::of_class(&failed.class);
            if self.deeper_passes(ccd, &sig, offset).is_empty() {
                waiting.entry((ccd, sig)).or_default().push((offset, at));
            } else {
                warnable.insert(at);
            }
        }
        *ended = self.trials.ended_order.len();
        warnable.retain(|at| self.trials.unwarned.contains(at));
        let at = *warnable.first()?;
        drop(cache);
        let trial = self.trials.all.get(at)?;
        let ended = trial.end.as_ref()?;
        let Some(Tested::Isolated { ccd, offset }) = trial.intent.tested else {
            return None;
        };
        let failed = self.derive_exposure(trial)?;
        let passes = self.deeper_passes(ccd, &Sig::of_class(&failed.class), offset);
        Some(Decision::new(
            PolicyPayload::TunerWarning(TunerWarning {
                warning: WarningKind::Monotonicity,
                trial: Some(trial.intent.trial.clone()),
                detail: format!(
                    "CCD{ccd} failed at {offset} where the same class passed adequately at a deeper offset",
                    offset = offset.get()
                ),
                passes,
            }),
            vec![trial.intent_seq, ended.seq],
        ))
    }

    /// The adequate isolated passes of the class on `ccd` deeper than `offset`, in order.
    fn deeper_passes(&self, ccd: CcdId, sig: &Sig, offset: TargetOffset) -> Vec<Seq> {
        let mut passes: Vec<Seq> = self
            .current_groups()
            .filter_map(|group| group.isolated_passes.get(&(ccd, sig.clone())))
            .flatten()
            .filter(|&&(deeper, _)| deeper < offset)
            .map(|&(_, seq)| seq)
            .collect();
        passes.sort();
        passes
    }

    /// The wall seconds a trial occupied the night: from its task's start, before the profile was prepared
    /// and the backends waited for, to its end, which for a trial a crash interrupted includes the downtime
    /// until recovery closed it. An intent that records no task start counts from itself.
    pub(super) fn trial_wall(trial: &Trial) -> u64 {
        let from = trial.intent.task_start.unwrap_or(trial.intent_time);
        trial
            .end
            .as_ref()
            .map_or(0, |ended| wall_secs(from, ended.time))
    }

    /// The workload content of the plan's workloads under `contents`.
    pub(super) fn contents_of(
        workloads: &[WorkloadId],
        contents: &BTreeMap<WorkloadId, togi_machine::ContentId>,
    ) -> Option<Vec<WorkloadContent>> {
        workloads
            .iter()
            .map(|&workload| {
                contents
                    .get(&workload.runs())
                    .map(|content| WorkloadContent {
                        workload,
                        content: content.clone(),
                    })
            })
            .collect()
    }
}

/// An exposure's failures that count against `ccd`: those on one of its cores, and those that name no
/// core, which a controlled isolated failure charges to the tested CCD.
pub(super) fn failures_on(
    observed: &ExposureObserved,
    ccd: CcdId,
    cores: &[togi_machine::CoreInfo],
) -> u32 {
    let count = observed
        .failures
        .iter()
        .filter(|failure| match failure.core {
            None => true,
            Some(core) => cores
                .iter()
                .any(|info| info.core == core && info.ccd == ccd),
        })
        .count();
    saturating_u32(u64::try_from(count).unwrap_or(u64::MAX))
}

/// A count too large for 32 bits stops at the largest one.
pub(super) fn saturating_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}
