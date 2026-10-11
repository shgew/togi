use togi_journal::{
    Event, Mce, Outcome, Seq, TrialEnd, TrialId, TrialIntent, TrialProgress,
};
use togi_machine::{Condition, CoreId, CoreSet, Signal};

use crate::state::Run;
use crate::{
    ClassKey, FoldError, Profile, Requirement, RequirementKind, StepNo,
};

/// What a trial's requirement stood at just before it ended, never more than its count.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
    pub(crate) crashed: bool,
    pub(crate) signalled: bool,
    pub(crate) mces: CoreSet,
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
    pub(crate) fn trial_intent(&mut self, event: &Event, intent: &TrialIntent) -> Result<(), FoldError> {
        let class = self.class_of_intent(intent)?;
        let profile = Profile::new(&self.topo, intent.profile)?;
        let requirement = self.intent_requirement(intent, class)?;
        let alone_core = intent
            .core
            .filter(|_| intent.condition == Condition::Alone);
        let cursor = alone_core.and_then(|core| self.topo.order().iter().position(|&id| id == core));
        if cursor.is_some() {
            self.search.turn(cursor);
        }
        self.retry = None;
        self.flight.open = Some(OpenTrial {
            trial: intent.trial.clone(),
            seq: event.seq,
            intent: intent.clone(),
            profile,
            class,
            requirement,
            crashed: false,
            signalled: false,
            mces: CoreSet::default(),
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
        if progress.signal.is_some() {
            if let Some(open) = self.flight.open.as_mut() {
                open.signalled = true;
            }
        }
        Ok(())
    }

    pub(crate) fn mce(&mut self, mce: &Mce) -> Result<(), FoldError> {
        let Some(core) = mce.core.filter(|_| mce.bank_type.core_local()) else {
            return Ok(());
        };
        let Some(open) = self.flight.open.as_ref() else {
            return Ok(());
        };
        if mce.trial.as_ref().is_some_and(|trial| trial != &open.trial) {
            return Ok(());
        }
        self.require_core(core)?;
        if let Some(open) = self.flight.open.as_mut() {
            open.mces.insert(core);
        }
        Ok(())
    }

    pub(crate) fn crash_detected(&mut self) {
        if let Some(open) = self.flight.open.as_mut() {
            open.crashed = true;
        }
    }

    fn history(&self, open: &OpenTrial) -> TrialHistory {
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
                    .valid;
                (valid.min(needed), valid.saturating_add(1).min(needed.max(1)))
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
        self.open_for(&end.trial)?;
        if end.outcome == Outcome::Failure && end.signal.is_none() {
            return Err(FoldError::MissingSignal);
        }
        let Some(open) = self.flight.open.take() else {
            return Err(FoldError::NoOpenTrial(end.trial.clone()));
        };
        let history = self.history_of(&open);
        self.record_end(event.seq, &open, end);
        let conclusive = matches!(end.outcome, Outcome::Pass | Outcome::Failure);
        if conclusive {
            match open.intent.core {
                Some(core) => self.update_core(core, |state| {
                    state.rotation.advance(open.intent.regime);
                }),
                None => self.all_rotation.advance(open.intent.regime),
            }
        }
        self.flight.last_closed = Some(ClosedTrial {
            trial: open.trial.clone(),
            history,
        });
        let alone = open.intent.condition == Condition::Alone;
        if alone {
            if let Some(core) = open.intent.core {
                self.update_core(core, |state| state.last_seq = Some(event.seq));
            }
        }
        match end.outcome {
            Outcome::Pass => {
                if alone {
                    self.record_step_pass(event.seq, &open);
                }
            }
            Outcome::Inconclusive => self.save_retry(&open),
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
                    mces: open.mces,
                });
            }
        }
        Ok(())
    }

    fn history_of(&self, open: &OpenTrial) -> TrialHistory {
        self.history(open)
    }

    /// An alone search pass at the core's current offset, under the backend package now in force,
    /// is part of its unfinished step.
    fn record_step_pass(&mut self, seq: Seq, open: &OpenTrial) {
        let intent = &open.intent;
        let Some(core) = intent.core else {
            return;
        };
        let state = self.core(core);
        let searching = state.phase == Some(togi_journal::CorePhase::Search);
        if searching
            && intent.offset == Some(state.offset)
            && intent.activity == Some(togi_journal::Activity::Search)
            && state.check.is_none()
        {
            let backend = intent.workload.backend();
            let id = self.evidence.active_backend(backend);
            self.search
                .record_pass(core, intent.regime, backend, id, seq);
        }
    }
}
