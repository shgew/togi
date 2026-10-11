use std::num::{NonZeroU16, NonZeroU32};

use togi_journal::{Activity, TrialIntent};
use togi_machine::{Condition, CoreId};

use crate::flight::OpenTrial;
use crate::state::Run;
use crate::{
    ClassKey, FoldError, Load, Requirement, RequirementKind, Secs, StepNo, TrialContext, TrialPlan,
    Window, WorkloadChoice,
};

/// The inconclusive trial to repeat, with the requirement it was scheduled under. Only a builder
/// selecting the same context and requirement serves it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Retry {
    pub(crate) plan: TrialPlan,
    pub(crate) requirement: Requirement,
}

impl Retry {
    pub(crate) fn alone_core(&self) -> Option<CoreId> {
        match self.plan.context {
            TrialContext::Search { core, .. } | TrialContext::SoloLimit { core, .. } => Some(core),
            TrialContext::Cycle { .. }
            | TrialContext::Rerun { .. }
            | TrialContext::ZeroRerun { .. }
            | TrialContext::Round { .. }
            | TrialContext::Hunt { .. } => None,
        }
    }
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

    /// The requirement captured before an intent folds. An alone trial outside a round belongs to
    /// its core's search; one naming a hunt, round or rerun belongs to a context no state opens
    /// yet; any other is a checking trial counting live passes.
    pub(crate) fn intent_requirement(
        &self,
        intent: &TrialIntent,
        class: ClassKey,
    ) -> Result<Requirement, FoldError> {
        let unscheduled = || FoldError::UnscheduledIntent(intent.trial.clone());
        if intent.hunt.is_some() || intent.round.is_some() || intent.rerun {
            return Err(unscheduled());
        }
        if intent.condition == Condition::Alone {
            let core = intent.core.ok_or_else(unscheduled)?;
            if intent.offset.is_none() {
                return Err(unscheduled());
            }
            return Ok(self.solo_requirement(core, class));
        }
        Ok(Requirement {
            kind: RequirementKind::Cycle,
            class,
            window: Window::Live { since: None },
            needed: NonZeroU16::MIN,
            step: intent.step.map(StepNo::new),
            part: None,
        })
    }

    /// Keeps an inconclusive alone search trial to retry.
    pub(crate) fn save_retry(&mut self, open: &OpenTrial) {
        let intent = &open.intent;
        let Some(core) = intent.core.filter(|_| intent.condition == Condition::Alone) else {
            return;
        };
        let context = match open.requirement.kind {
            RequirementKind::Search => TrialContext::Search {
                core,
                regime: intent.regime,
                offset: open.profile.get(core),
            },
            RequirementKind::SoloLimit => TrialContext::SoloLimit {
                core,
                regime: intent.regime,
            },
            RequirementKind::Cycle
            | RequirementKind::Rerun
            | RequirementKind::ZeroRerun
            | RequirementKind::Round
            | RequirementKind::Hunt => return,
        };
        self.retry = Some(Retry {
            plan: TrialPlan {
                regime: intent.regime,
                workload: WorkloadChoice::Frozen(intent.workload),
                load: Load::Single(core),
                condition: Condition::Alone,
                activity: intent.activity.unwrap_or(Activity::Search),
                profile: open.profile,
                duration: Secs::new(NonZeroU32::new(intent.duration_s).unwrap_or(NonZeroU32::MIN)),
                context,
                requirement: open.requirement,
                retry: true,
                cause: Vec::new(),
            },
            requirement: open.requirement,
        });
    }

    /// The requirement a retry's shape would carry now, or `None` when the retry's context ended.
    fn retry_requirement(&self, retry: &Retry) -> Option<Requirement> {
        let plan = &retry.plan;
        let (core, offset) = match plan.context {
            TrialContext::Search { core, offset, .. } => (core, Some(offset)),
            TrialContext::SoloLimit { core, .. } => (core, None),
            TrialContext::Cycle { .. }
            | TrialContext::Rerun { .. }
            | TrialContext::ZeroRerun { .. }
            | TrialContext::Round { .. }
            | TrialContext::Hunt { .. } => return Some(retry.requirement),
        };
        let state = self.core(core);
        let current = state.phase == Some(togi_journal::CorePhase::Search)
            && plan.profile.get(core) == state.offset
            && offset.is_none_or(|offset| offset == state.offset)
            && plan.duration == self.config.search_trial;
        if !current {
            return None;
        }
        let WorkloadChoice::Frozen(workload) = plan.workload else {
            return None;
        };
        let class = ClassKey::new(workload, Load::Single(core).cores(), plan.duration);
        Some(self.solo_requirement(core, class))
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

    /// The saved retry as a trial plan, when the search turn's own builder selects its
    /// requirement.
    pub(crate) fn serve_retry(&self) -> Option<TrialPlan> {
        let retry = self.retry.as_ref()?;
        let core = retry.alone_core()?;
        if self.retry_requirement(retry)? != retry.requirement {
            return None;
        }
        let mut plan = retry.plan.clone();
        plan.cause = self.core(core).last_seq.into_iter().collect();
        Some(plan)
    }

    pub(crate) fn rotation(&self, plan: &TrialPlan) -> usize {
        match plan.load {
            Load::Single(core) => self.core(core).rotation.get(plan.regime),
            Load::Many(_) => self.all_rotation.get(plan.regime),
        }
    }
}
