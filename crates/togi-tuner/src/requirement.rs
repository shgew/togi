use std::num::{NonZeroU16, NonZeroU32};

use togi_journal::{Activity, TrialIntent};
use togi_machine::{Condition, CoreId, Regime};

use crate::flight::OpenTrial;
use crate::state::Run;
use crate::{
    ClassKey, FoldError, Load, Profile, Requirement, RequirementKind, Secs, TrialContext,
    TrialPlan, Window, WorkloadChoice,
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

    /// The core whose search turn or candidate check an alone intent belongs to, when the intent
    /// is the trial that turn asks for: the core is in search at the intent's offset, runs the
    /// search duration with every other core at zero, and takes R1, or R2 once its R1 passed,
    /// with the workload its rotation names or its check froze.
    fn search_context(&self, intent: &TrialIntent) -> Option<CoreId> {
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
            (None, Regime::R2) if self.search.current(id, &self.evidence).0.is_some() => {
                self.rotation_workload(id, Regime::R2)
            }
            _ => return None,
        };
        (intent.workload == expected).then_some(id)
    }

    /// The requirement captured before an intent folds: the one of the search turn or candidate
    /// check it belongs to. An intent that belongs to no open scheduling context is one the
    /// tuner never planned.
    pub(crate) fn intent_requirement(
        &self,
        intent: &TrialIntent,
        class: ClassKey,
    ) -> Result<Requirement, FoldError> {
        self.search_context(intent)
            .map(|core| self.solo_requirement(core, class))
            .ok_or_else(|| FoldError::UnscheduledIntent(intent.trial.clone()))
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
                offset: intent.offset.unwrap_or_else(|| self.core(core).offset),
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
            && plan.profile.get(core) == Some(state.offset)
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
