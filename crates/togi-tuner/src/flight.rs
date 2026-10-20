use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use togi_journal::{Event, Mce, Outcome, Seq, TrialEnd, TrialId, TrialIntent, TrialProgress};
use togi_machine::{Condition, CoreId, CoreSet, Regime, Signal};

use crate::r7::failed::FailedTrial;
use crate::search::SearchRegime;
use crate::state::Run;
use crate::{BackendId, ClassKey, FoldError, Profile, Requirement, StepNo, TrialContext};

/// The cores of the MCEs seen in a trial's window that its end cites.
pub(crate) fn cited_mces(seen: &BTreeMap<Seq, CoreId>, cause: &[Seq]) -> CoreSet {
    seen.iter()
        .filter(|(seq, _)| cause.contains(seq))
        .map(|(_, &core)| core)
        .collect()
}

/// What a trial's requirement stood at just before it ended, never more than its count.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TrialHistory {
    pub passed: u32,
    pub trial: u32,
    pub needed: u32,
    pub step: Option<StepNo>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct OpenTrial {
    pub(crate) trial: TrialId,
    pub(crate) seq: Seq,
    pub(crate) intent: TrialIntent,
    pub(crate) profile: Profile,
    pub(crate) class: ClassKey,
    pub(crate) requirement: Requirement,
    pub(crate) backend: BackendId,
    pub(crate) crashed: bool,
    pub(crate) mces: BTreeMap<Seq, CoreId>,
}

/// A failed trial the tuner has yet to attribute.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Awaiting {
    pub(crate) intent: TrialIntent,
    pub(crate) profile: Profile,
    pub(crate) class: ClassKey,
    pub(crate) end_seq: Seq,
    pub(crate) cause: Vec<Seq>,
    pub(crate) signal: Signal,
    pub(crate) core: Option<CoreId>,
    pub(crate) mces: CoreSet,
    pub(crate) r7: Option<FailedTrial>,
    pub(crate) zero: Option<Seq>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ClosedTrial {
    pub(crate) trial: TrialId,
    pub(crate) history: TrialHistory,
}

/// Where the current trial stands: none, in flight, or failed and awaiting attribution.
#[expect(
    clippy::large_enum_variant,
    reason = "a run holds one, stored inline like the rest of its state"
)]
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) enum InFlight {
    #[default]
    Idle,
    Open(OpenTrial),
    Awaiting(Awaiting),
}

/// The current trial and the last trial closed.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Flight {
    pub(crate) current: InFlight,
    pub(crate) last_closed: Option<ClosedTrial>,
}

impl Flight {
    pub(crate) const fn open(&self) -> Option<&OpenTrial> {
        match &self.current {
            InFlight::Open(open) => Some(open),
            InFlight::Idle | InFlight::Awaiting(_) => None,
        }
    }

    pub(crate) const fn awaiting(&self) -> Option<&Awaiting> {
        match &self.current {
            InFlight::Awaiting(awaiting) => Some(awaiting),
            InFlight::Idle | InFlight::Open(_) => None,
        }
    }
}

impl Run {
    pub(crate) fn trial_intent(
        &mut self,
        event: &Event,
        intent: &TrialIntent,
    ) -> Result<(), FoldError> {
        let open = match &self.flight.current {
            InFlight::Idle => None,
            InFlight::Open(open) => Some(&open.trial),
            InFlight::Awaiting(awaiting) => Some(&awaiting.intent.trial),
        };
        if let Some(open) = open {
            return Err(FoldError::OverlappingIntent {
                trial: intent.trial.clone(),
                open: open.clone(),
            });
        }
        let class = self.class_of_intent(intent)?;
        let profile = Profile::new(&self.topo, intent.profile)?;
        let requirement = self.schedule_intent(intent, class, &event.cause)?;
        if let Some(turn) = requirement.context.turn() {
            self.search
                .turn(self.topo.order().iter().position(|&id| id == turn.core));
        }
        self.retry = None;
        self.zero_intent(event, intent);
        self.flight.current = InFlight::Open(OpenTrial {
            trial: intent.trial.clone(),
            seq: event.seq,
            intent: intent.clone(),
            profile,
            class,
            requirement,
            backend: self.evidence.active_backend(intent.workload.backend()),
            crashed: false,
            mces: BTreeMap::new(),
        });
        Ok(())
    }

    fn open_for(&self, trial: &TrialId) -> Result<&OpenTrial, FoldError> {
        self.flight
            .open()
            .filter(|open| &open.trial == trial)
            .ok_or_else(|| FoldError::NoOpenTrial(trial.clone()))
    }

    pub(crate) fn trial_progress(&mut self, progress: &TrialProgress) -> Result<(), FoldError> {
        self.open_for(&progress.trial)?;
        self.require_cores(progress.core.into_iter().collect())
    }

    pub(crate) fn mce(&mut self, event: &Event, mce: &Mce) -> Result<(), FoldError> {
        if let Some(core) = mce.core {
            self.require_core(core)?;
        }
        let Some(core) = mce.core.filter(|_| mce.bank_type.core_local()) else {
            return Ok(());
        };
        let InFlight::Open(open) = &mut self.flight.current else {
            return Ok(());
        };
        if mce.trial.as_ref().is_none_or(|trial| trial == &open.trial) {
            open.mces.insert(event.seq, core);
        }
        Ok(())
    }

    pub(crate) fn crash_detected(&mut self) {
        if let InFlight::Open(open) = &mut self.flight.current {
            open.crashed = true;
        }
    }

    pub(crate) fn history(&self, open: &OpenTrial) -> TrialHistory {
        let requirement = open.requirement;
        let needed = u32::from(requirement.needed.get());
        let single = match requirement.context {
            TrialContext::Search { .. } | TrialContext::ZeroRerun { .. } => true,
            TrialContext::SoloLimit { .. }
            | TrialContext::Cycle { .. }
            | TrialContext::Rerun { .. }
            | TrialContext::Round { .. }
            | TrialContext::Hunt { .. } => false,
        };
        let (passed, trial) = if single {
            (0, 1)
        } else {
            let valid = self
                .evidence
                .count(
                    requirement.class,
                    &open.profile,
                    requirement.window,
                    requirement.needed,
                )
                .map_or(0, |count| count.valid);
            (
                valid.min(needed),
                valid.saturating_add(1).min(needed.max(1)),
            )
        };
        TrialHistory {
            passed,
            trial,
            needed: if single { 1 } else { needed },
            step: match requirement.context {
                TrialContext::Cycle { step, .. } => Some(step),
                TrialContext::Search { .. }
                | TrialContext::SoloLimit { .. }
                | TrialContext::Rerun { .. }
                | TrialContext::ZeroRerun { .. }
                | TrialContext::Round { .. }
                | TrialContext::Hunt { .. } => None,
            },
        }
    }

    pub(crate) fn trial_end(&mut self, event: &Event, end: &TrialEnd) -> Result<(), FoldError> {
        if end.outcome == Outcome::Failure && end.signal.is_none() {
            return Err(FoldError::MissingSignal);
        }
        self.require_cores(end.core.into_iter().chain(end.stalled_core).collect())?;
        self.require_cores(end.excluded.iter().copied().collect())?;
        self.require_cores(end.top_requesters)?;
        self.require_cores(
            end.voltage_requests_v
                .iter()
                .map(|(core, _)| core)
                .collect(),
        )?;
        let open = match std::mem::take(&mut self.flight.current) {
            InFlight::Open(open) if open.trial == end.trial => open,
            other @ (InFlight::Idle | InFlight::Open(_) | InFlight::Awaiting(_)) => {
                self.flight.current = other;
                return Err(FoldError::NoOpenTrial(end.trial.clone()));
            }
        };
        let history = self.history(&open);
        self.record_end(event.seq, &open, end)?;
        let cited = cited_mces(&open.mces, &event.cause);
        let (failed, zero) = self.end_r7_and_zero(event.seq, &open, end, cited);
        let outcome = end.evidence_outcome();
        self.r7_probe_end(&open, outcome);
        let conclusive = matches!(outcome, Outcome::Pass | Outcome::Failure);
        if conclusive {
            self.advance_rotation(open.intent.core, open.intent.regime);
        }
        self.flight.last_closed = Some(ClosedTrial {
            trial: open.trial.clone(),
            history,
        });
        let class = open.class;
        let together = open.intent.condition == Condition::Together;
        if let Some(turn) = open.requirement.context.turn() {
            self.update_core(turn.core, |state| state.last_seq = Some(event.seq));
        }
        match outcome {
            Outcome::Pass => self.record_step_pass(event.seq, &open),
            Outcome::Inconclusive => {
                self.save_retry(&open);
                self.revalidate_retry();
            }

            Outcome::Failure => {
                let signal = end.signal.ok_or(FoldError::MissingSignal)?;
                self.flight.current = InFlight::Awaiting(Awaiting {
                    intent: open.intent,
                    profile: open.profile,
                    class: open.class,
                    end_seq: event.seq,
                    cause: event.cause.clone(),
                    signal,
                    core: end.core,
                    mces: cited,
                    r7: failed,
                    zero,
                });
            }
        }
        if conclusive {
            self.cycle_evidence_changed(class);
        }
        if outcome == Outcome::Pass && together {
            self.checking.note_pass(event.seq);
            if let Some(celsius) = end.tctl_max_c {
                self.checking.note_temperature(celsius, event.seq);
            }
        }
        self.retire_reruns();
        self.revalidate_retry();
        Ok(())
    }

    /// A search pass at the core's current offset, under the backend package its trial began with,
    /// is part of its unfinished step; a candidate check's passes are counted by the ledger alone.
    fn record_step_pass(&mut self, seq: Seq, open: &OpenTrial) {
        let Some(turn) = open.requirement.context.turn() else {
            return;
        };
        let regime = match turn.regime {
            Regime::R1 => SearchRegime::R1,
            Regime::R2 => SearchRegime::R2,
            Regime::R3
            | Regime::R4
            | Regime::R5
            | Regime::R6
            | Regime::R7
            | Regime::R8
            | Regime::R9 => return,
        };
        let state = self.core(turn.core);
        let current = state.phase.searching() && turn.offset == Some(state.offset);
        if current {
            self.search.record_pass(
                turn.core,
                regime,
                open.intent.workload.backend(),
                open.backend,
                seq,
            );
        }
    }
}
