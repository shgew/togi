use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use togi_journal::{Event, Mce, Outcome, Seq, TrialEnd, TrialId, TrialIntent, TrialProgress};
use togi_machine::{Condition, CoreId, CoreSet, Regime, Signal};

use crate::r7::failed::FailedTrial;
use crate::requirement::Turn;
use crate::state::Run;
use crate::{BackendId, ClassKey, FoldError, Profile, Requirement, RequirementKind, StepNo};

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

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpenTrial {
    pub(crate) trial: TrialId,
    pub(crate) seq: Seq,
    pub(crate) intent: TrialIntent,
    pub(crate) profile: Profile,
    pub(crate) class: ClassKey,
    pub(crate) requirement: Requirement,
    pub(crate) turn: Option<Turn>,
    pub(crate) backend: BackendId,
    pub(crate) crashed: bool,
    pub(crate) mces: BTreeMap<Seq, CoreId>,
}

/// A failed trial the tuner has yet to attribute.
#[derive(Clone, Debug, PartialEq)]
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ClosedTrial {
    pub(crate) trial: TrialId,
    pub(crate) history: TrialHistory,
}

/// The trial in flight, the failed end awaiting attribution, and the last trial closed.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Flight {
    pub(crate) open: Option<OpenTrial>,
    pub(crate) awaiting: Option<Awaiting>,
    pub(crate) last_closed: Option<ClosedTrial>,
}

impl Run {
    pub(crate) fn trial_intent(
        &mut self,
        event: &Event,
        intent: &TrialIntent,
    ) -> Result<(), FoldError> {
        if let Some(open) = self.flight.open.as_ref().map(|open| &open.trial).or(self
            .flight
            .awaiting
            .as_ref()
            .map(|awaiting| &awaiting.intent.trial))
        {
            return Err(FoldError::OverlappingIntent {
                trial: intent.trial.clone(),
                open: open.clone(),
            });
        }
        let class = self.class_of_intent(intent)?;
        let profile = Profile::new(&self.topo, intent.profile)?;
        self.require_cores(intent.core.into_iter().collect())?;
        let (requirement, turn) = self.schedule_intent(intent, class, &event.cause)?;
        if let Some(turn) = turn {
            self.search
                .turn(self.topo.order().iter().position(|&id| id == turn.core));
        }
        self.retry = None;
        self.zero_intent(event, intent);
        self.flight.open = Some(OpenTrial {
            trial: intent.trial.clone(),
            seq: event.seq,
            intent: intent.clone(),
            profile,
            class,
            requirement,
            turn,
            backend: self.evidence.active_backend(intent.workload.backend()),
            crashed: false,
            mces: BTreeMap::new(),
        });
        Ok(())
    }

    fn open_for(&self, trial: &TrialId) -> Result<&OpenTrial, FoldError> {
        self.flight
            .open
            .as_ref()
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
        let Some(open) = self.flight.open.as_ref() else {
            return Ok(());
        };
        if mce.trial.as_ref().is_some_and(|trial| trial != &open.trial) {
            return Ok(());
        }
        if let Some(open) = self.flight.open.as_mut() {
            open.mces.insert(event.seq, core);
        }
        Ok(())
    }

    pub(crate) fn crash_detected(&mut self) {
        if let Some(open) = self.flight.open.as_mut() {
            open.crashed = true;
        }
    }

    pub(crate) fn history(&self, open: &OpenTrial) -> TrialHistory {
        let requirement = open.requirement;
        let needed = u32::from(requirement.needed.get());
        let (passed, trial) = match requirement.kind {
            RequirementKind::Search | RequirementKind::ZeroRerun => (0, 1),
            RequirementKind::SoloLimit
            | RequirementKind::Cycle
            | RequirementKind::Rerun
            | RequirementKind::Round
            | RequirementKind::Hunt => {
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
            }
        };
        TrialHistory {
            passed,
            trial,
            needed: match requirement.kind {
                RequirementKind::Search | RequirementKind::ZeroRerun => 1,
                _ => needed,
            },
            step: requirement.step,
        }
    }

    pub(crate) fn trial_end(&mut self, event: &Event, end: &TrialEnd) -> Result<(), FoldError> {
        if end.outcome == Outcome::Failure && end.signal.is_none() {
            return Err(FoldError::MissingSignal);
        }
        self.require_cores(end.core.into_iter().chain(end.stalled_core).collect())?;
        self.require_cores(end.top_requesters)?;
        self.require_cores(
            end.voltage_requests_v
                .iter()
                .map(|(core, _)| core)
                .collect(),
        )?;
        let open = match self.flight.open.take() {
            Some(open) if open.trial == end.trial => open,
            _ => return Err(FoldError::NoOpenTrial(end.trial.clone())),
        };
        let history = self.history(&open);
        self.record_end(event.seq, &open, end)?;
        let cited = cited_mces(&open.mces, &event.cause);
        let (failed, zero) = self.end_r7_and_zero(event.seq, &open, end, cited);
        let conclusive = matches!(end.outcome, Outcome::Pass | Outcome::Failure);
        if conclusive {
            self.advance_rotation(open.intent.core, open.intent.regime);
        }
        self.flight.last_closed = Some(ClosedTrial {
            trial: open.trial.clone(),
            history,
        });
        let class = open.class;
        let together = open.intent.condition == Condition::Together;
        if let Some(turn) = open.turn {
            self.update_core(turn.core, |state| state.last_seq = Some(event.seq));
        }
        match end.outcome {
            Outcome::Pass => self.record_step_pass(event.seq, &open),
            Outcome::Inconclusive => {
                self.save_retry(&open, zero);
                self.revalidate_retry();
            }

            Outcome::Failure => {
                let signal = end.signal.ok_or(FoldError::MissingSignal)?;
                self.flight.awaiting = Some(Awaiting {
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
        if end.outcome == Outcome::Pass && together {
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
        let Some(turn) = open.turn else {
            return;
        };
        let state = self.core(turn.core);
        let current = state.phase == Some(togi_journal::CorePhase::Search)
            && turn.offset == Some(state.offset);
        if current {
            self.search.record_pass(
                turn.core,
                turn.regime == Regime::R1,
                open.intent.workload.backend(),
                open.backend,
                seq,
            );
        }
    }
}
