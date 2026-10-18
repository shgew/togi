use std::num::{NonZeroU16, NonZeroU32};

use togi_journal::{Activity, Seq, TrialIntent};
use togi_machine::{Condition, CoreId, Regime, TargetOffset, WorkloadId};

use crate::flight::OpenTrial;
use crate::state::Run;
use crate::{
    ClassKey, FoldError, Load, Profile, Requirement, Secs, TrialContext, TrialPlan, Window,
    WorkloadChoice,
};

/// The search turn or candidate check a trial belongs to: its core and regime, and the offset the
/// core searched at, none for a candidate check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Turn {
    pub(crate) core: CoreId,
    pub(crate) regime: Regime,
    pub(crate) offset: Option<TargetOffset>,
}

impl TrialContext {
    /// The search turn or candidate check this context is, when it is one.
    pub(crate) const fn turn(self) -> Option<Turn> {
        match self {
            Self::Search {
                core,
                regime,
                offset,
            } => Some(Turn {
                core,
                regime,
                offset: Some(offset),
            }),
            Self::SoloLimit { core, regime } => Some(Turn {
                core,
                regime,
                offset: None,
            }),
            Self::Cycle { .. }
            | Self::Rerun { .. }
            | Self::ZeroRerun { .. }
            | Self::Round { .. }
            | Self::Hunt { .. } => None,
        }
    }
}

/// The inconclusive trial to repeat; its plan carries the requirement it was scheduled under.
/// Only a builder selecting the same requirement serves it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Retry {
    pub(crate) plan: TrialPlan,
    pub(crate) workload: WorkloadId,
}

impl Retry {
    pub(crate) const fn requirement(&self) -> Requirement {
        self.plan.requirement
    }

    pub(crate) const fn turn(&self) -> Option<Turn> {
        self.plan.requirement.context.turn()
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
        let loaded = Load::decode(intent, &self.topo)?.cores();
        let duration = NonZeroU32::new(intent.duration_s).ok_or(FoldError::ZeroDuration)?;
        Ok(ClassKey::new(intent.workload, loaded, Secs::new(duration)))
    }

    /// The obligation of an alone search trial: a candidate check needs `n` passes since the
    /// check began; a search step needs the one pass at the core's offset.
    pub(crate) fn solo_requirement(&self, core: CoreId, class: ClassKey) -> Requirement {
        let state = self.core(core);
        let regime = class.regime();
        if let Some(check) = state.phase.check() {
            Requirement {
                context: TrialContext::SoloLimit { core, regime },
                class,
                window: Window::LiveOrCarried {
                    since: Some(check.since),
                },
                needed: self.config.needed,
            }
        } else {
            Requirement {
                context: TrialContext::Search {
                    core,
                    regime,
                    offset: state.offset,
                },
                class,
                window: Window::Live { since: None },
                needed: NonZeroU16::MIN,
            }
        }
    }

    /// The core whose turn an alone intent belongs to, when the intent is the trial that turn
    /// asks for: the core is in search at the intent's offset, runs the search duration with every
    /// other core at zero, and takes R1, or R2 once its R1 passed, with the workload its rotation
    /// names or its check froze.
    fn search_turn(&self, intent: &TrialIntent) -> Option<CoreId> {
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
        let expected = match (core.phase.check(), intent.regime) {
            (Some(check), Regime::R1) => check.r1,
            (Some(check), Regime::R2) => check.r2,
            (None, Regime::R1) => self.rotation_workload(id, Regime::R1),
            (None, Regime::R2) if self.r1_passed(id) => self.rotation_workload(id, Regime::R2),
            (None, Regime::R2)
            | (Some(_) | None, Regime::R3 | Regime::R4 | Regime::R5 | Regime::R6 | Regime::R7) => {
                return None;
            }
        };
        (intent.workload == expected).then_some(id)
    }

    /// The requirement captured before an intent folds. An intent that belongs to no open
    /// scheduling context is one the tuner never planned.
    pub(crate) fn schedule_intent(
        &self,
        intent: &TrialIntent,
        class: ClassKey,
        cause: &[Seq],
    ) -> Result<Requirement, FoldError> {
        if let Some(core) = self.search_turn(intent) {
            return Ok(self.solo_requirement(core, class));
        }
        self.hunt_intent(intent, class)
            .or_else(|| self.zero_context(intent, class, cause))
            .or_else(|| self.r7_probe_context(intent, class))
            .or_else(|| self.rerun_context(intent, class))
            .or_else(|| self.round_context(intent, class))
            .or_else(|| self.cycle_context(intent, class))
            .ok_or_else(|| FoldError::UnscheduledIntent(intent.trial.clone()))
    }

    /// Whether the core's unfinished search step holds an R1 pass under the backend packages now
    /// in force, which its R2 trial needs.
    fn r1_passed(&self, id: CoreId) -> bool {
        self.search.current(id, &self.evidence).0.is_some()
    }

    /// Keeps an inconclusive trial to retry under the requirement it was scheduled for.
    pub(crate) fn save_retry(&mut self, open: &OpenTrial) {
        let intent = &open.intent;
        let load = Load::recorded(
            intent.core.filter(|_| intent.cores.is_empty()),
            intent.cores,
        );
        let (condition, activity, cause) = match open.requirement.context {
            TrialContext::ZeroRerun { failure } => (
                Condition::Parked,
                intent.activity.unwrap_or(Activity::Checking),
                vec![failure],
            ),
            TrialContext::Search { .. }
            | TrialContext::SoloLimit { .. }
            | TrialContext::Cycle { .. }
            | TrialContext::Rerun { .. }
            | TrialContext::Round { .. }
            | TrialContext::Hunt { .. } => (
                intent.condition,
                intent.activity.unwrap_or(Activity::Search),
                Vec::new(),
            ),
        };
        self.retry = Some(Retry {
            plan: TrialPlan {
                regime: intent.regime,
                workload: WorkloadChoice::Frozen(intent.workload),
                load,
                condition,
                activity,
                profile: open.profile,
                duration: open.class.duration(),
                requirement: open.requirement,
                retry: true,
                cause,
            },
            workload: intent.workload,
        });
    }

    /// The requirement a retry's shape would carry now, or `None` when the retry's context ended.
    pub(crate) fn retry_requirement(&self, retry: &Retry) -> Option<Requirement> {
        let plan = &retry.plan;
        let Some(turn) = retry.turn() else {
            return match plan.requirement.context {
                TrialContext::ZeroRerun { failure } => self.zero_retry_requirement(plan, failure),
                TrialContext::Cycle { .. } => self.cycle_retry_requirement(plan),
                TrialContext::Rerun { failure } => self
                    .r7_probe_retry_requirement(plan, failure)
                    .or_else(|| self.rerun_retry_requirement(plan, failure)),
                TrialContext::Round { round } => self.round_retry_requirement(plan, round),
                TrialContext::Hunt { hunt, group } => self.hunt_retry_requirement(hunt, group),
                TrialContext::Search { .. } | TrialContext::SoloLimit { .. } => {
                    Some(retry.requirement())
                }
            };
        };
        let state = self.core(turn.core);
        let current = state.phase.searching()
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

    /// The saved checking retry as a trial plan on the current checking profile, when its
    /// requirement is the one the cycle or rerun builder selected.
    pub(crate) fn serve_checking_retry(
        &self,
        requirement: &Requirement,
        cause: Vec<Seq>,
    ) -> Option<TrialPlan> {
        let retry = self.retry.as_ref()?;
        let checking = matches!(
            retry.plan.requirement.context,
            TrialContext::Cycle { .. } | TrialContext::Rerun { .. }
        );
        let holds = checking
            && retry.requirement() == *requirement
            && self.retry_requirement(retry).as_ref() == Some(requirement);
        holds.then(|| TrialPlan {
            profile: self.checking.profile().unwrap_or(retry.plan.profile),
            cause,
            ..retry.plan.clone()
        })
    }

    /// Drops the saved retry when its context ended or its requirement changed.
    pub(crate) fn revalidate_retry(&mut self) {
        let holds = self
            .retry
            .as_ref()
            .is_none_or(|retry| self.retry_requirement(retry) == Some(retry.requirement()));
        if !holds {
            self.retry = None;
        }
    }

    /// The saved retry as a trial plan. A retry that stopped holding was dropped by the fold of
    /// the event that ended its context, so a saved one is always current.
    pub(crate) fn serve_retry(&self) -> Option<TrialPlan> {
        let retry = self.retry.as_ref()?;
        let turn = retry.turn()?;
        let mut plan = retry.plan.clone();
        plan.cause = self.core(turn.core).last_seq.into_iter().collect();
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
