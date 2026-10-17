mod config;
mod domain;
mod message;
mod session;
mod smu;
mod trial;
mod tuner;

use std::fmt;
use std::str::FromStr;

use serde::{Serialize, Serializer};
use togi_machine::CoreId;

pub use config::{
    ConfigBackends, ConfigChecking, ConfigDeadEnds, ConfigDurations, ConfigEvidence, ConfigSnapshot,
};
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

impl Payload {
    /// The trial this event belongs to, if any.
    pub(crate) fn trial(&self) -> Option<&TrialId> {
        match self {
            Self::TrialIntent(p) => Some(&p.trial),
            Self::TrialStart(p) => Some(&p.trial),
            Self::TrialProgress(p) => Some(&p.trial),
            Self::TrialSignal(p) => Some(&p.trial),
            Self::TrialSample(p) => Some(&p.trial),
            Self::TrialEnd(p) => Some(&p.trial),
            Self::Failure(p) => p.trial.as_ref(),
            Self::Mce(p) => p.trial.as_ref(),
            Self::HuntStart(p) => p.trial.as_ref(),
            Self::TunerWarning(p) => p.trial.as_ref(),
            Self::SessionWarning(p) => p.trial.as_ref(),
            Self::TrialCarried(p) => p.source.trial.as_ref(),
            Self::FailureCarried(p) => p.source.trial.as_ref(),
            Self::SessionStart(_)
            | Self::SessionContext(_)
            | Self::SessionBaseline(_)
            | Self::SessionNotice(_)
            | Self::SessionArchived(_)
            | Self::SessionCarried(_)
            | Self::ConfigLoaded(_)
            | Self::PreflightCheck(_)
            | Self::SmuIntent(_)
            | Self::SmuWrite(_)
            | Self::SmuReadback(_)
            | Self::SmuError(_)
            | Self::ProfileApplied(_)
            | Self::ProfileChange(_)
            | Self::ProfileRestored(_)
            | Self::CrashDetected(_)
            | Self::TunerDecision(_)
            | Self::CorePhase(_)
            | Self::CheckingCycle(_)
            | Self::CheckingStep(_)
            | Self::CheckingChain(_)
            | Self::HostRanking(_)
            | Self::HuntGroup(_)
            | Self::HuntEnd(_)
            | Self::HuntSkipped(_)
            | Self::Combination(_)
            | Self::DeepeningRound(_)
            | Self::BackendRetry(_)
            | Self::CommandReset(_)
            | Self::DefectFound(_)
            | Self::DefectAnswered(_)
            | Self::DeadEnd(_)
            | Self::BootSavedEntry(_)
            | Self::BootLeaveReason(_)
            | Self::Shutdown(_)
            | Self::JournalTorn(_)
            | Self::StateRebuilt(_) => None,
        }
    }

    /// Whether this event names `core` in its own fields.
    pub(crate) fn names_core(&self, core: CoreId) -> bool {
        match self {
            Self::SessionNotice(p) => p.cores.contains(core),
            Self::SmuIntent(p) => p.command().core() == Some(core),
            Self::SmuWrite(p) => p.command().core() == Some(core),
            Self::SmuReadback(p) => p.core == core,
            Self::SmuError(p) => p.core == Some(core),
            Self::TrialIntent(p) => p.core == Some(core) || p.cores.contains(core),
            Self::TrialStart(p) => p.instances.iter().any(|instance| instance.core == core),
            Self::TrialProgress(p) => p.core == Some(core),
            Self::TrialEnd(p) => p.core == Some(core),
            Self::TrialCarried(p) => p.core == Some(core) || p.class.cores.contains(core),
            Self::Failure(p) => p.core == Some(core),
            Self::FailureCarried(p) => p.failure.core == Some(core) || p.class.cores.contains(core),
            Self::Mce(p) => p.core == Some(core),
            Self::TunerDecision(p) => p.core == core,
            Self::CorePhase(p) => p.core == core,
            Self::CheckingChain(p) => p.cores.contains(core),
            Self::HuntStart(p) => p.cores.contains(core) || p.candidates.contains(core),
            Self::HuntGroup(p) => {
                p.cores.contains(core) || p.probe.is_some_and(|probe| probe.core == core)
            }
            Self::HuntEnd(p) => {
                p.cores.contains(core) || p.members.iter().any(|member| member.core == core)
            }
            Self::Combination(p) => p.members.iter().any(|member| member.core == core),
            Self::DeepeningRound(p) => p.cores.contains(core),
            Self::CommandReset(p) => p.target == ResetTarget::Core(core),
            Self::DefectFound(p) => p.cores.contains(core),
            Self::DefectAnswered(p) => p.cores.contains(core),
            Self::DeadEnd(p) => p.core == Some(core),
            Self::SessionStart(_)
            | Self::SessionContext(_)
            | Self::SessionBaseline(_)
            | Self::SessionWarning(_)
            | Self::SessionArchived(_)
            | Self::SessionCarried(_)
            | Self::ConfigLoaded(_)
            | Self::PreflightCheck(_)
            | Self::ProfileApplied(_)
            | Self::ProfileChange(_)
            | Self::ProfileRestored(_)
            | Self::TrialSignal(_)
            | Self::TrialSample(_)
            | Self::CrashDetected(_)
            | Self::CheckingCycle(_)
            | Self::CheckingStep(_)
            | Self::HostRanking(_)
            | Self::HuntSkipped(_)
            | Self::TunerWarning(_)
            | Self::BackendRetry(_)
            | Self::BootSavedEntry(_)
            | Self::BootLeaveReason(_)
            | Self::Shutdown(_)
            | Self::JournalTorn(_)
            | Self::StateRebuilt(_) => false,
        }
    }
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
