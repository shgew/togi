use std::num::{NonZeroU16, NonZeroU32};

use togi_journal::{Activity, Seq, TrialIntent};
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
    pub(crate) turn: Option<Turn>,
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
        cause: &[Seq],
    ) -> Result<(Requirement, Option<Turn>), FoldError> {
        if let Some(turn) = self.search_turn(intent) {
            return Ok((self.solo_requirement(turn.core, class), Some(turn)));
        }
        self.hunt_intent(intent, class)
            .or_else(|| self.zero_context(intent, class, cause))
            .or_else(|| self.r7_probe_context(intent, class))
            .or_else(|| self.rerun_context(intent, class))
            .or_else(|| self.round_context(intent, class))
            .or_else(|| self.cycle_context(intent, class))
            .map(|requirement| (requirement, None))
            .ok_or_else(|| FoldError::UnscheduledIntent(intent.trial.clone()))
    }

    /// Whether the core's unfinished search step holds an R1 pass under the backend packages now
    /// in force, which its R2 trial needs.
    fn r1_passed(&self, id: CoreId) -> bool {
        self.search.current(id, &self.evidence).0.is_some()
    }

    /// The context an inconclusive trial's retry belongs to, when the trial belongs to one that
    /// keeps retries.
    fn retry_context(&self, open: &OpenTrial) -> Option<TrialContext> {
        let intent = &open.intent;
        if let Some(turn) = open.turn {
            return Some(turn.context());
        }
        match open.requirement.kind {
            RequirementKind::Cycle => Some(TrialContext::Cycle {
                cycle: intent.cycle?,
                step: open.requirement.step?,
                part: open.requirement.part?,
            }),
            RequirementKind::Rerun => match open.requirement.window {
                Window::Live { since } | Window::LiveOrCarried { since } => {
                    Some(TrialContext::Rerun {
                        failure: self.r7_probe_source(open.requirement).or(since)?,
                    })
                }
            },
            RequirementKind::Round => Some(TrialContext::Round {
                round: intent.round?,
            }),
            RequirementKind::Hunt => Some(TrialContext::Hunt {
                hunt: intent.hunt?,
                group: intent.group?,
            }),
            RequirementKind::Search | RequirementKind::SoloLimit | RequirementKind::ZeroRerun => {
                None
            }
        }
    }

    /// Keeps an inconclusive trial to retry when its context keeps retries.
    pub(crate) fn save_retry(&mut self, open: &OpenTrial, zero: Option<Seq>) {
        let intent = &open.intent;
        if self.save_zero_retry(open, zero) {
            return;
        }
        let Some(context) = self.retry_context(open) else {
            return;
        };
        let load = match intent.core {
            Some(core) if intent.cores.is_empty() => Load::Single(core),
            _ => Load::Many(intent.cores),
        };
        self.retry = Some(Retry {
            plan: TrialPlan {
                regime: intent.regime,
                workload: WorkloadChoice::Frozen(intent.workload),
                load,
                condition: intent.condition,
                activity: intent.activity.unwrap_or(Activity::Search),
                profile: open.profile,
                duration: open.class.duration(),
                context,
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
    pub(crate) fn retry_requirement(&self, retry: &Retry) -> Option<Requirement> {
        let plan = &retry.plan;
        let Some(turn) = retry.turn else {
            return match plan.context {
                TrialContext::ZeroRerun { failure } => self.zero_retry_requirement(plan, failure),
                TrialContext::Cycle { .. } => self.cycle_retry_requirement(plan),
                TrialContext::Rerun { failure } => self
                    .r7_probe_retry_requirement(plan, failure)
                    .or_else(|| self.rerun_retry_requirement(plan, failure)),
                TrialContext::Round { round } => self.round_retry_requirement(plan, round),
                TrialContext::Hunt { hunt, group } => self.hunt_retry_requirement(hunt, group),
                TrialContext::Search { .. } | TrialContext::SoloLimit { .. } => {
                    Some(retry.requirement)
                }
            };
        };
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

    /// The saved checking retry as a trial plan on the current checking profile, when its
    /// requirement is the one the cycle or rerun builder selected.
    pub(crate) fn serve_checking_retry(
        &self,
        requirement: &Requirement,
        cause: Vec<Seq>,
    ) -> Option<TrialPlan> {
        let retry = self.retry.as_ref()?;
        let checking = matches!(
            retry.plan.context,
            TrialContext::Cycle { .. } | TrialContext::Rerun { .. }
        );
        let holds = checking
            && retry.requirement == *requirement
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
            .is_none_or(|retry| self.retry_requirement(retry) == Some(retry.requirement));
        if !holds {
            self.retry = None;
        }
    }

    /// The saved retry as a trial plan. A retry that stopped holding was dropped by the fold of
    /// the event that ended its context, so a saved one is always current.
    pub(crate) fn serve_retry(&self) -> Option<TrialPlan> {
        let retry = self.retry.as_ref()?;
        let turn = retry.turn?;
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
