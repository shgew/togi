use std::num::{NonZeroU16, NonZeroU32};

use togi_journal::{Activity, TrialIntent};
use togi_machine::{Condition, CoreId, Regime, TargetOffset, WorkloadId};

use crate::flight::OpenTrial;
use crate::state::Run;
use crate::{
    ClassKey, FoldError, Load, Profile, Requirement, RequirementKind, Secs, TrialContext,
    TrialPlan, Window, WorkloadChoice,
};

/// The search turn or candidate check a trial belongs to: its core and regime, and the offset the
/// core searched at, none for a candidate check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Turn {
    pub(crate) core: CoreId,
    pub(crate) regime: Regime,
    pub(crate) offset: Option<TargetOffset>,
}

impl Turn {
    pub(crate) const fn context(self) -> TrialContext {
        match self.offset {
            Some(offset) => TrialContext::Search {
                core: self.core,
                regime: self.regime,
                offset,
            },
            None => TrialContext::SoloLimit {
                core: self.core,
                regime: self.regime,
            },
        }
    }
}

/// The inconclusive trial to repeat, with the requirement it was scheduled under. Only a builder
/// selecting the same context and requirement serves it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Retry {
    pub(crate) plan: TrialPlan,
    pub(crate) requirement: Requirement,
    pub(crate) turn: Turn,
    pub(crate) workload: WorkloadId,
}

impl Run {
    pub(crate) fn class_of_intent(&self, intent: &TrialIntent) -> Result<ClassKey, FoldError> {
        if intent.workload.regime() != intent.regime {
            return Err(FoldError::WorkloadRegime {
                regime: intent.regime,
                workload: intent.workload,
            });
        }
        let loaded = match intent.core {
            Some(core) => Load::Single(core).cores(),
            None => intent.cores,
        };
        if loaded.is_empty() {
            return Err(FoldError::NoLoad(intent.trial.clone()));
        }
        if let Some(core) = loaded.iter().find(|&core| !self.topo.contains(core)) {
            return Err(FoldError::UnknownCore(core));
        }
        let duration = NonZeroU32::new(intent.duration_s).ok_or(FoldError::ZeroDuration)?;
        Ok(ClassKey::new(intent.workload, loaded, Secs::new(duration)))
    }

    /// The obligation of an alone search trial: a candidate check needs `n` passes since the
    /// check began; a search step needs the one pass.
    pub(crate) fn solo_requirement(&self, core: CoreId, class: ClassKey) -> Requirement {
        let state = self.core(core);
        if state.check.is_some() {
            Requirement {
                kind: RequirementKind::SoloLimit,
                class,
                window: Window::LiveOrCarried {
                    since: state.phase_seq,
                },
                needed: self.config.needed,
                step: None,
                part: None,
            }
        } else {
            Requirement {
                kind: RequirementKind::Search,
                class,
                window: Window::Live { since: None },
                needed: NonZeroU16::MIN,
                step: None,
                part: None,
            }
        }
    }

    /// The turn an alone intent belongs to, when the intent is the trial that turn asks for: the
    /// core is in search at the intent's offset, runs the search duration with every other core at
    /// zero, and takes R1, or R2 once its R1 passed, with the workload its rotation names or its
    /// check froze.
    fn search_turn(&self, intent: &TrialIntent) -> Option<Turn> {
        let alone = intent.condition == Condition::Alone
            && intent.activity == Some(Activity::Search)
            && intent.cores.is_empty()
            && intent.cycle.is_none()
            && intent.step.is_none()
            && intent.hunt.is_none()
            && intent.group.is_none()
            && intent.round.is_none()
            && !intent.rerun;
        let id = intent.core.filter(|_| alone)?;
        let core = self.searching(id)?;
        let at_offset = intent.offset == Some(core.offset)
            && Profile::new(&self.topo, intent.profile).ok()
                == Some(Profile::alone(&self.topo, id, core.offset))
            && intent.duration_s == self.config.search_trial.get();
        if !at_offset {
            return None;
        }
        let expected = match (core.check, intent.regime) {
            (Some(check), Regime::R1) => check.r1,
            (Some(check), Regime::R2) => check.r2,
            (None, Regime::R1) => self.rotation_workload(id, Regime::R1),
            (None, Regime::R2) if self.r1_passed(id) => self.rotation_workload(id, Regime::R2),
            _ => return None,
        };
        (intent.workload == expected).then_some(Turn {
            core: id,
            regime: intent.regime,
            offset: core.check.is_none().then_some(core.offset),
        })
    }

    /// The requirement captured before an intent folds, with the turn it belongs to: the search
    /// turn's or the candidate check's. An intent that belongs to no open scheduling context is
    /// one the tuner never planned.
    pub(crate) fn schedule_intent(
        &self,
        intent: &TrialIntent,
        class: ClassKey,
    ) -> Result<(Requirement, Turn), FoldError> {
        let turn = self
            .search_turn(intent)
            .ok_or_else(|| FoldError::UnscheduledIntent(intent.trial.clone()))?;
        Ok((self.solo_requirement(turn.core, class), turn))
    }

    /// Whether the core's unfinished search step holds an R1 pass under the backend packages now
    /// in force, which its R2 trial needs.
    fn r1_passed(&self, id: CoreId) -> bool {
        self.search.current(id, &self.evidence).0.is_some()
    }

    /// Keeps an inconclusive search or candidate-check trial to retry.
    pub(crate) fn save_retry(&mut self, open: &OpenTrial) {
        let intent = &open.intent;
        self.retry = Some(Retry {
            plan: TrialPlan {
                regime: intent.regime,
                workload: WorkloadChoice::Frozen(intent.workload),
                load: Load::Single(open.turn.core),
                condition: Condition::Alone,
                activity: Activity::Search,
                profile: open.profile,
                duration: open.class.duration(),
                context: open.turn.context(),
                requirement: open.requirement,
                retry: true,
                cause: Vec::new(),
            },
            requirement: open.requirement,
            turn: open.turn,
            workload: intent.workload,
        });
    }

    /// The requirement a retry's shape would carry now, or `None` when the retry's context ended.
    fn retry_requirement(&self, retry: &Retry) -> Option<Requirement> {
        let turn = retry.turn;
        let state = self.core(turn.core);
        let current = state.phase == Some(togi_journal::CorePhase::Search)
            && retry.plan.profile.get(turn.core) == Some(state.offset)
            && turn.offset.is_none_or(|offset| offset == state.offset)
            && (turn.regime != Regime::R2 || turn.offset.is_none() || self.r1_passed(turn.core))
            && retry.plan.duration == self.config.search_trial;
        current.then(|| {
            let class = ClassKey::new(
                retry.workload,
                Load::Single(turn.core).cores(),
                retry.plan.duration,
            );
            self.solo_requirement(turn.core, class)
        })
    }

    /// Drops the saved retry when its context ended or its requirement changed.
    pub(crate) fn revalidate_retry(&mut self) {
        let holds = self
            .retry
            .as_ref()
            .is_none_or(|retry| self.retry_requirement(retry) == Some(retry.requirement));
        if !holds {
            self.retry = None;
        }
    }

    /// The saved retry as a trial plan. A retry that stopped holding was dropped by the fold of
    /// the event that ended its context, so a saved one is always current.
    pub(crate) fn serve_retry(&self) -> Option<TrialPlan> {
        let retry = self.retry.as_ref()?;
        let mut plan = retry.plan.clone();
        plan.cause = self.core(retry.turn.core).last_seq.into_iter().collect();
        Some(plan)
    }

    pub(crate) fn rotation(&self, plan: &TrialPlan) -> usize {
        match plan.load {
            Load::Single(core) => self.core(core).rotation.get(plan.regime),
            Load::Many(_) => self.all_rotation.get(plan.regime),
        }
    }

    /// A conclusive trial ended: the next workload of its regime is the next in the list, for the
    /// core that ran it alone or for the all-load rotation.
    pub(crate) fn advance_rotation(&mut self, core: Option<CoreId>, regime: Regime) {
        match core {
            Some(core) => self.update_core(core, |state| state.rotation.advance(regime)),
            None => self.all_rotation.advance(regime),
        }
    }
}
