use std::num::{NonZeroU16, NonZeroU32};

use togi_journal::{
    Activity, CheckingChain, CheckingCycle, CheckingStep, Combination, CoreTransition, DeadEnd,
    DeepeningRound, Failure, HuntEnd, HuntGroup, HuntSkipped, HuntStart, Payload, ProfileChange,
    Seq, TrialId, TrialIntent, TunerDecision, TunerWarning,
};
use togi_machine::{Condition, CoreId, CoreSet, Regime, WorkloadId, pick_workload};

use crate::{ClassKey, Profile, Secs, Window};

macro_rules! ordinal {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(NonZeroU32);

        impl $name {
            pub const fn new(value: NonZeroU32) -> Self {
                Self(value)
            }

            pub const fn get(self) -> NonZeroU32 {
                self.0
            }
        }
    };
}

ordinal!(
    /// The one-based place of an R7 occurrence in a checking cycle.
    StepNo
);
ordinal!(
    /// The one-based place of a part within its checking step.
    PartNo
);

/// The cores a trial loads: one core, or a set of them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Load {
    Single(CoreId),
    Many(CoreSet),
}

impl Load {
    pub fn cores(self) -> CoreSet {
        match self {
            Self::Single(core) => std::iter::once(core).collect(),
            Self::Many(cores) => cores,
        }
    }
}

/// Which workload of its regime a trial runs: the next in the rotation, or one a decision froze.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkloadChoice {
    Rotate,
    Frozen(WorkloadId),
}

/// The identity a scheduled trial's requirement belongs to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrialContext {
    Search {
        core: CoreId,
        regime: Regime,
        offset: togi_machine::TargetOffset,
    },
    SoloLimit {
        core: CoreId,
        regime: Regime,
    },
    Cycle {
        cycle: NonZeroU32,
        step: StepNo,
        part: PartNo,
    },
    Rerun {
        failure: Seq,
    },
    ZeroRerun {
        failure: Seq,
    },
    Round {
        round: NonZeroU32,
    },
    Hunt {
        hunt: NonZeroU32,
        group: NonZeroU32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequirementKind {
    Search,
    SoloLimit,
    Cycle,
    Rerun,
    ZeroRerun,
    Round,
    Hunt,
}

/// The evidence obligation a trial was scheduled for, captured before its intent folds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Requirement {
    pub kind: RequirementKind,
    pub class: ClassKey,
    pub window: Window,
    pub needed: NonZeroU16,
    pub step: Option<StepNo>,
    pub part: Option<PartNo>,
}

/// A trial the tuner asks for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrialPlan {
    pub regime: Regime,
    pub workload: WorkloadChoice,
    pub load: Load,
    pub condition: Condition,
    pub activity: Activity,
    pub profile: Profile,
    pub duration: Secs,
    pub context: TrialContext,
    pub requirement: Requirement,
    pub retry: bool,
    pub cause: Vec<Seq>,
}

impl TrialPlan {
    /// The workload this trial runs when the regime's rotation stands at `rotation`.
    pub fn workload_at(&self, rotation: usize) -> WorkloadId {
        match self.workload {
            WorkloadChoice::Frozen(workload) => workload,
            WorkloadChoice::Rotate => pick_workload(self.regime, rotation).id(),
        }
    }

    /// The `trial.intent` payload that records this plan as trial `trial`.
    pub fn intent(&self, trial: TrialId, rotation: usize) -> TrialIntent {
        let (core, cores, offset) = match self.load {
            Load::Single(core) => (Some(core), CoreSet::default(), Some(self.profile.get(core))),
            Load::Many(cores) => (None, cores, None),
        };
        let mut intent = TrialIntent {
            trial,
            kernel_cursor: None,
            kernel_error: None,
            profile: self.profile.offsets(),
            core,
            cores,
            offset,
            regime: self.regime,
            workload: self.workload_at(rotation),
            duration_s: self.duration.get(),
            condition: self.condition,
            activity: Some(self.activity),
            retry: self.retry,
            cycle: None,
            step: None,
            hunt: None,
            group: None,
            round: None,
            rerun: false,
        };
        match self.context {
            TrialContext::Search { .. } | TrialContext::SoloLimit { .. } => {}
            TrialContext::Cycle { cycle, step, .. } => {
                intent.cycle = Some(cycle);
                intent.step = Some(step.get());
            }
            TrialContext::Rerun { .. } | TrialContext::ZeroRerun { .. } => intent.rerun = true,
            TrialContext::Round { round } => intent.round = Some(round),
            TrialContext::Hunt { hunt, group } => {
                intent.hunt = Some(hunt);
                intent.group = Some(group);
            }
        }
        intent
    }
}

/// A decision's journal payload, one of the kinds the tuner decides.
#[derive(Clone, Debug, PartialEq)]
pub enum DecisionPayload {
    Failure(Failure),
    TunerWarning(TunerWarning),
    DeadEnd(DeadEnd),
    CorePhase(CoreTransition),
    TunerDecision(TunerDecision),
    ProfileChange(ProfileChange),
    CheckingCycle(CheckingCycle),
    CheckingStep(CheckingStep),
    CheckingChain(CheckingChain),
    HuntStart(HuntStart),
    HuntGroup(HuntGroup),
    HuntEnd(HuntEnd),
    HuntSkipped(HuntSkipped),
    Combination(Combination),
    DeepeningRound(DeepeningRound),
}

macro_rules! decision_from {
    ($($variant:ident($ty:ty)),+ $(,)?) => {
        $(impl From<$ty> for DecisionPayload {
            fn from(payload: $ty) -> Self {
                Self::$variant(payload)
            }
        })+

        impl From<DecisionPayload> for Payload {
            fn from(payload: DecisionPayload) -> Self {
                match payload {
                    $(DecisionPayload::$variant(payload) => Self::$variant(payload),)+
                }
            }
        }
    };
}

decision_from!(
    Failure(Failure),
    TunerWarning(TunerWarning),
    DeadEnd(DeadEnd),
    CorePhase(CoreTransition),
    TunerDecision(TunerDecision),
    ProfileChange(ProfileChange),
    CheckingCycle(CheckingCycle),
    CheckingStep(CheckingStep),
    CheckingChain(CheckingChain),
    HuntStart(HuntStart),
    HuntGroup(HuntGroup),
    HuntEnd(HuntEnd),
    HuntSkipped(HuntSkipped),
    Combination(Combination),
    DeepeningRound(DeepeningRound),
);

/// A decision to record, with the events it cites.
#[derive(Clone, Debug, PartialEq)]
pub struct DecisionAction {
    pub payload: DecisionPayload,
    pub cause: Vec<Seq>,
}

impl DecisionAction {
    pub fn new(payload: impl Into<DecisionPayload>, cause: Vec<Seq>) -> Self {
        Self {
            payload: payload.into(),
            cause,
        }
    }
}

/// What the tuner asks the session to do next.
#[expect(
    clippy::large_enum_variant,
    reason = "one action is built per step and consumed at once"
)]
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Decide(DecisionAction),
    RunTrial(TrialPlan),
    ReadRanking,
}

#[cfg(test)]
impl TrialPlan {
    pub(crate) fn window_since(&self) -> Option<Option<Seq>> {
        match self.requirement.window {
            Window::Live { since } | Window::LiveOrCarried { since } => Some(since),
        }
    }
}
