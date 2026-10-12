mod domain;
mod message;
mod session;
mod smu;
mod trial;
mod tuner;

use std::fmt;
use std::str::FromStr;

use serde::{Serialize, Serializer};

pub use domain::{
    Activity, Attribution, CorePhase, CycleEvent, DeadEndAction, DeadEndCondition, Decision,
    DefectAnswer, DefectDirection, HuntOutcome, HuntStage, Inference, Notice, Outcome, RetryTarget,
    RoundEvent, ShutdownReason, SmuOp, WarningKind,
};
pub use session::{
    BootLeaveReason, BootSavedEntry, CarriedCore, CarriedFailurePoint, CarriedLimit, CarriedSource,
    CommandReset, ConfigLoaded, DeadEnd, DefectAnswered, DefectFound, JournalTorn, PreflightCheck,
    ProfileApplied, ProfileChange, ProfileRestored, Provenance, ResetTarget, SessionArchived,
    SessionBaseline, SessionCarried, SessionContext, SessionNotice, SessionStart, SessionWarning,
    Shutdown, StateRebuilt,
};
pub use smu::{SmuCommand, SmuError, SmuIntentRecord, SmuReadback, SmuWriteRecord};
pub use trial::{
    BackendIdentity, CrashDetected, FactSource, Failure, FailureCarried, Mce, TrialCarried,
    TrialClass, TrialEnd, TrialId, TrialInstance, TrialIntent, TrialProgress, TrialSample,
    TrialSignal, TrialStart,
};
pub use tuner::{
    BackendRetry, CheckingChain, CheckingCycle, CheckingStep, Combination, CoreTransition,
    DeepeningRound, HostRanking, HuntEnd, HuntGroup, HuntSkipped, HuntStart, Member, Rederived,
    TunerDecision, TunerWarning,
};

macro_rules! payloads {
    ($($variant:ident($ty:ty) = $wire:literal,)+) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub enum EventKind { $($variant,)+ }

        impl EventKind {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $wire,)+ }
            }
        }

        #[derive(Clone, Debug, PartialEq)]
        pub enum Payload { $($variant($ty),)+ }

        impl Payload {
            pub const fn kind(&self) -> EventKind {
                match self { $(Self::$variant(_) => EventKind::$variant,)+ }
            }

            pub(crate) fn decode(kind: EventKind, data: &str) -> serde_json::Result<Self> {
                Ok(match kind {
                    $(EventKind::$variant => Self::$variant(serde_json::from_str(data)?),)+
                })
            }

            /// Decodes `data` again, tracking where in it an error occurs; for finding an unknown field.
            pub(crate) fn decode_tracked(
                kind: EventKind,
                data: &str,
            ) -> Result<(), serde_path_to_error::Error<serde_json::Error>> {
                let mut deserializer = serde_json::Deserializer::from_str(data);
                match kind {
                    $(EventKind::$variant => {
                        serde_path_to_error::deserialize::<_, $ty>(&mut deserializer).map(drop)
                    })+
                }
            }
        }

        impl Serialize for Payload {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                match self { $(Self::$variant(payload) => payload.serialize(serializer),)+ }
            }
        }

        impl FromStr for EventKind {
            type Err = UnknownKind;

            fn from_str(text: &str) -> Result<Self, UnknownKind> {
                match text {
                    $($wire => Ok(Self::$variant),)+
                    _ => Err(UnknownKind(text.to_owned())),
                }
            }
        }

        $(impl From<$ty> for Payload {
            fn from(payload: $ty) -> Self { Self::$variant(payload) }
        })+
    };
}

payloads! {
    SessionStart(SessionStart) = "session.start",
    SessionContext(SessionContext) = "session.context",
    SessionBaseline(SessionBaseline) = "session.baseline",
    SessionNotice(SessionNotice) = "session.notice",
    SessionWarning(SessionWarning) = "session.warning",
    SessionArchived(SessionArchived) = "session.archived",
    SessionCarried(SessionCarried) = "session.carried",
    ConfigLoaded(ConfigLoaded) = "config.loaded",
    PreflightCheck(PreflightCheck) = "preflight.check",
    SmuIntent(SmuIntentRecord) = "smu.intent",
    SmuWrite(SmuWriteRecord) = "smu.write",
    SmuReadback(SmuReadback) = "smu.readback",
    SmuError(SmuError) = "smu.error",
    ProfileApplied(ProfileApplied) = "profile.applied",
    ProfileChange(ProfileChange) = "profile.change",
    ProfileRestored(ProfileRestored) = "profile.restored",
    TrialIntent(TrialIntent) = "trial.intent",
    TrialStart(TrialStart) = "trial.start",
    TrialProgress(TrialProgress) = "trial.progress",
    TrialSignal(TrialSignal) = "trial.signal",
    TrialSample(TrialSample) = "trial.sample",
    TrialEnd(TrialEnd) = "trial.end",
    TrialCarried(TrialCarried) = "trial.carried",
    Failure(Failure) = "failure",
    FailureCarried(FailureCarried) = "failure.carried",
    Mce(Mce) = "mce",
    CrashDetected(CrashDetected) = "crash.detected",
    TunerDecision(TunerDecision) = "tuner.decision",
    CorePhase(CoreTransition) = "core.phase",
    CheckingCycle(CheckingCycle) = "checking.cycle",
    CheckingStep(CheckingStep) = "checking.step",
    CheckingChain(CheckingChain) = "checking.chain",
    HostRanking(HostRanking) = "host.ranking",
    HuntStart(HuntStart) = "hunt.start",
    HuntGroup(HuntGroup) = "hunt.group",
    HuntEnd(HuntEnd) = "hunt.end",
    HuntSkipped(HuntSkipped) = "hunt.skipped",
    Combination(Combination) = "combination",
    DeepeningRound(DeepeningRound) = "deepening.round",
    TunerWarning(TunerWarning) = "tuner.warning",
    BackendRetry(BackendRetry) = "backend.retry",
    CommandReset(CommandReset) = "command.reset",
    DefectFound(DefectFound) = "defect.found",
    DefectAnswered(DefectAnswered) = "defect.answered",
    DeadEnd(DeadEnd) = "deadend",
    BootSavedEntry(BootSavedEntry) = "boot.saved_entry",
    BootLeaveReason(BootLeaveReason) = "boot.leave_reason",
    Shutdown(Shutdown) = "shutdown",
    JournalTorn(JournalTorn) = "journal.torn",
    StateRebuilt(StateRebuilt) = "state.rebuilt",
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("unknown event kind {0:?}")]
pub struct UnknownKind(pub String);

impl EventKind {
    pub fn group(self) -> &'static str {
        let name = self.as_str();
        name.split_once('.').map_or(name, |(group, _)| group)
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for EventKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for EventKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}
