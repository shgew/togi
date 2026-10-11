use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU16;

use togi_journal::{Activity, DeadEnd, DeadEndCondition, Outcome, Seq, TrialId};
use togi_machine::{Condition, CoreId};

use crate::failures::Pending;
use crate::state::Run;
use crate::{
    ClassKey, DecisionAction, Profile, Requirement, RequirementKind, TrialContext, TrialPlan,
    Window, WorkloadChoice,
};

const NOT_CURVE_OPTIMIZER: &str = "the instability is not caused by Curve Optimizer";

/// The all-zero rerun of a failure that would otherwise end the session at CO 0. A diagnostic
/// not yet running is the one `at_zero` still asks for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Diagnostic {
    Running { trial: TrialId },
    Passed { end: Seq },
    Failed { end: Seq },
}

/// The diagnostics by the failure they confirm, and the trial each running one is.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Zero {
    by_failure: BTreeMap<Seq, Diagnostic>,
    by_trial: HashMap<TrialId, Seq>,
}

impl Zero {
    pub(crate) fn get(&self, failure: Seq) -> Option<&Diagnostic> {
        self.by_failure.get(&failure)
    }

    /// The rerun of `failure` started as `trial`. A trial it replaces no longer answers it.
    pub(crate) fn run(&mut self, failure: Seq, trial: TrialId) {
        let replaced = self.by_failure.insert(
            failure,
            Diagnostic::Running {
                trial: trial.clone(),
            },
        );
        if let Some(Diagnostic::Running { trial: old }) = replaced {
            self.by_trial.remove(&old);
        }
        self.by_trial.insert(trial, failure);
    }

    /// The trial ended. Returns the failure it was the rerun of when it still is; a conclusive end
    /// settles that diagnostic.
    pub(crate) fn end(&mut self, trial: &TrialId, seq: Seq, outcome: Outcome) -> Option<Seq> {
        let failure = self.by_trial.remove(trial)?;
        let end = match outcome {
            Outcome::Pass => Diagnostic::Passed { end: seq },
            Outcome::Failure => Diagnostic::Failed { end: seq },
            Outcome::Inconclusive => return Some(failure),
        };
        self.by_failure.insert(failure, end);
        Some(failure)
    }

    pub(crate) fn retain(&mut self, keep: impl Fn(Seq) -> bool) {
        self.by_failure.retain(|&failure, _| keep(failure));
        self.by_trial.retain(|_, &mut failure| keep(failure));
    }
}

/// What a failure at CO 0 needs first.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum AtZero {
    DeadEnd(Box<DecisionAction>),
    Rerun(Box<TrialPlan>),
    Released,
}

pub(crate) fn zero_requirement(class: ClassKey, failure: Seq) -> Requirement {
    Requirement {
        kind: RequirementKind::ZeroRerun,
        class,
        window: Window::LiveOrCarried {
            since: Some(failure),
        },
        needed: NonZeroU16::MIN,
        step: None,
        part: None,
    }
}

pub(crate) fn dead_end(
    core: CoreId,
    head: &str,
    rerun: Option<Seq>,
    cause: Vec<Seq>,
) -> DecisionAction {
    let detail = match rerun {
        None => format!("{head}; {NOT_CURVE_OPTIMIZER}"),
        Some(end) => format!(
            "{head}; the rerun with every core at CO 0 failed too (#{end}), so {NOT_CURVE_OPTIMIZER}"
        ),
    };
    DecisionAction::new(
        DeadEnd {
            condition: DeadEndCondition::FailureAtZero,
            core: Some(core),
            detail,
            action: None,
        },
        cause,
    )
}

impl Run {
    /// What failure `record` at CO 0 needs: an already all-zero failing profile ends at once;
    /// otherwise the failed trial reruns with every core at CO 0 and ends the session citing both
    /// failures when that fails too. Once the rerun passed the failure belongs to the nonzero
    /// cores and nothing is left here.
    pub(crate) fn at_zero(
        &self,
        seq: Seq,
        record: &Pending,
        core: CoreId,
        head: &str,
        mut cause: Vec<Seq>,
    ) -> AtZero {
        if record.profile.is_zero() {
            return AtZero::DeadEnd(Box::new(dead_end(core, head, None, cause)));
        }
        match self.zero.get(seq) {
            None | Some(Diagnostic::Running { .. }) => {
                AtZero::Rerun(Box::new(self.zero_plan(seq, record)))
            }
            Some(Diagnostic::Passed { .. }) => AtZero::Released,
            Some(&Diagnostic::Failed { end }) => {
                cause.push(end);
                AtZero::DeadEnd(Box::new(dead_end(core, head, Some(end), cause)))
            }
        }
    }

    /// The failed trial run again with every core at CO 0 in its own shape: a single-core trial
    /// stays one, so the rerun loads the same CPUs as the failure it confirms. A saved retry of
    /// this failure's rerun is served instead of a new one.
    fn zero_plan(&self, seq: Seq, record: &Pending) -> TrialPlan {
        self.zero_retry(seq)
            .unwrap_or_else(|| self.zero_plan_fresh(seq, record))
    }

    fn zero_plan_fresh(&self, seq: Seq, record: &Pending) -> TrialPlan {
        let class = record.class;
        TrialPlan {
            regime: class.regime(),
            workload: WorkloadChoice::Frozen(class.workload()),
            load: record.load,
            condition: Condition::Parked,
            activity: if record.failure.condition == Some(Condition::Parked) {
                Activity::Hunt
            } else {
                Activity::Checking
            },
            profile: Profile::zero(&self.topo),
            duration: class.duration(),
            context: TrialContext::ZeroRerun { failure: seq },
            requirement: zero_requirement(class, seq),
            retry: false,
            cause: vec![seq],
        }
    }

    fn zero_retry(&self, failure: Seq) -> Option<TrialPlan> {
        let retry = self.retry.as_ref()?;
        if retry.plan.context != (TrialContext::ZeroRerun { failure })
            || self.retry_requirement(retry)? != retry.requirement
        {
            return None;
        }
        let mut plan = retry.plan.clone();
        plan.cause = vec![failure];
        Some(plan)
    }

    /// The rerun of failure `failure` passed: it releases the core a non-R7 failure named and
    /// queues a failure that was not a hunt group's for the nonzero cores.
    pub(crate) fn zero_passed(&mut self, failure: Seq) {
        let Some(record) = self.failures.pending(failure) else {
            return;
        };
        if record.class.multi_r7() {
            return;
        }
        let core = record.failure.core;
        let queue = record.failure.condition != Some(Condition::Parked);
        if let Some(core) = core {
            self.update_core(core, |state| {
                if state.pending == Some(failure) {
                    state.pending = None;
                }
            });
        }
        if queue {
            self.failures.queue_back(failure);
        }
    }

    /// Whether `intent` is the all-zero rerun the tuner plans for the pending failure its cause
    /// names, and the requirement that rerun carries.
    pub(crate) fn zero_context(
        &self,
        intent: &togi_journal::TrialIntent,
        class: ClassKey,
        cause: &[Seq],
    ) -> Option<Requirement> {
        let failure = *cause.first()?;
        let record = self.failures.pending(failure)?;
        let plan = self.zero_plan_fresh(failure, record);
        let shape_core = intent.core.filter(|_| intent.cores.is_empty());
        let shape = match (plan.load, shape_core) {
            (crate::Load::Single(core), Some(found)) => core == found,
            (crate::Load::Many(cores), None) => cores == intent.cores,
            _ => false,
        };
        let matches = intent.rerun
            && intent.condition == Condition::Parked
            && intent.hunt.is_none()
            && intent.group.is_none()
            && intent.round.is_none()
            && intent.cycle.is_none()
            && intent.step.is_none()
            && intent.activity == Some(plan.activity)
            && class == record.class
            && shape
            && Profile::new(&self.topo, intent.profile).ok() == Some(plan.profile);
        matches.then(|| zero_requirement(class, failure))
    }

    pub(crate) fn zero_retry_requirement(
        &self,
        plan: &TrialPlan,
        failure: Seq,
    ) -> Option<Requirement> {
        self.failures.pending(failure)?;
        let WorkloadChoice::Frozen(workload) = plan.workload else {
            return None;
        };
        let class = ClassKey::new(workload, plan.load.cores(), plan.duration);
        Some(zero_requirement(class, failure))
    }

    /// Keeps an inconclusive all-zero rerun to retry. Returns whether `open` was one.
    pub(crate) fn save_zero_retry(
        &mut self,
        open: &crate::flight::OpenTrial,
        zero: Option<Seq>,
    ) -> bool {
        let (RequirementKind::ZeroRerun, Some(failure)) = (open.requirement.kind, zero) else {
            return false;
        };
        let intent = &open.intent;
        let load = match intent.core {
            Some(core) if intent.cores.is_empty() => crate::Load::Single(core),
            _ => crate::Load::Many(intent.cores),
        };
        self.retry = Some(crate::requirement::Retry {
            plan: TrialPlan {
                regime: intent.regime,
                workload: WorkloadChoice::Frozen(intent.workload),
                load,
                condition: Condition::Parked,
                activity: intent.activity.unwrap_or(Activity::Checking),
                profile: open.profile,
                duration: open.class.duration(),
                context: TrialContext::ZeroRerun { failure },
                requirement: open.requirement,
                retry: true,
                cause: vec![failure],
            },
            requirement: open.requirement,
            turn: None,
            workload: intent.workload,
        });
        true
    }

    pub(crate) fn zero_intent(
        &mut self,
        event: &togi_journal::Event,
        intent: &togi_journal::TrialIntent,
    ) {
        let rerun = intent.condition == Condition::Parked && intent.rerun && intent.hunt.is_none();
        if let (true, Some(&failure)) = (rerun, event.cause.first()) {
            self.zero.run(failure, intent.trial.clone());
        }
    }
}
