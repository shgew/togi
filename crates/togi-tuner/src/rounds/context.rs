use std::num::NonZeroU32;

use togi_journal::{Activity, Seq, TrialIntent};
use togi_machine::Condition;

use super::{Check, Round};
use crate::state::Run;
use crate::{
    ClassKey, Profile, Requirement, RequirementKind, TrialContext, TrialPlan, Window,
    WorkloadChoice,
};

pub(super) fn round_requirement(round: &Round, check: &Check) -> Requirement {
    Requirement {
        kind: RequirementKind::Round,
        class: check.class,
        window: Window::LiveOrCarried {
            since: Some(check.since),
        },
        needed: round.trials(),
        step: None,
        part: None,
    }
}

impl Run {
    /// The requirement of a deepening intent, when it is a check of the round in progress: a
    /// round that is not open, another round's id, or a class the round does not check is a
    /// trial the tuner never planned.
    pub(crate) fn round_context(
        &self,
        intent: &TrialIntent,
        class: ClassKey,
    ) -> Option<Requirement> {
        let round = self.rounds.active()?;
        let check = round.checks().iter().find(|check| check.class == class)?;
        let loads_class = match check.condition {
            Condition::Alone => {
                intent.cores.is_empty()
                    && intent.core.is_some_and(|core| {
                        class.loaded().contains(core) && intent.offset == check.profile.get(core)
                    })
                    && class.loaded().len() == 1
            }
            Condition::Together | Condition::Parked => {
                intent.core.is_none() && intent.cores == class.loaded()
            }
        };
        let at_profile = Profile::new(&self.topo, intent.profile).ok() == Some(check.profile);
        let shaped = intent.round == Some(round.id())
            && intent.activity == Some(Activity::Deepening)
            && intent.condition == check.condition
            && intent.cycle.is_none()
            && intent.step.is_none()
            && intent.hunt.is_none()
            && intent.group.is_none()
            && !intent.rerun
            && at_profile
            && loads_class;
        shaped.then(|| round_requirement(round, check))
    }

    /// The requirement a saved round retry would carry now: its round still open and the check
    /// still the round's.
    pub(crate) fn round_retry_requirement(
        &self,
        plan: &TrialPlan,
        round: NonZeroU32,
    ) -> Option<Requirement> {
        let open = self.rounds.active().filter(|open| open.id() == round)?;
        let WorkloadChoice::Frozen(workload) = plan.workload else {
            return None;
        };
        let class = ClassKey::new(workload, plan.load.cores(), plan.duration);
        let check = open
            .checks()
            .iter()
            .find(|check| check.class == class && check.profile == plan.profile)?;
        Some(round_requirement(open, check))
    }

    /// The saved retry as a trial plan, when it was scheduled under `requirement`.
    pub(crate) fn serve_round_retry(
        &self,
        requirement: &Requirement,
        cause: Vec<Seq>,
    ) -> Option<TrialPlan> {
        let retry = self.retry.as_ref()?;
        let holds = matches!(retry.plan.context, TrialContext::Round { .. })
            && retry.requirement == *requirement
            && self.retry_requirement(retry).as_ref() == Some(requirement);
        holds.then(|| TrialPlan {
            cause,
            ..retry.plan.clone()
        })
    }
}
