mod config;
mod day;
mod domain;
mod message;
mod ruleset13;
mod ruleset13_day;
mod session;
mod smu;
mod trial;
mod tuner;

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use togi_machine::CoreId;

pub use config::{
    ConfigBackends, ConfigChecking, ConfigDeadEnds, ConfigDurations, ConfigEvidence, ConfigSnapshot,
};
pub use day::{
    CrashedBoot, DayAnswer, DayAnswerKind, DayBootStart, DayCheckpoint, DayCleanStop, DayCrashes,
    DayFault, DayIntervalBoot, DayIntervalCredit, DayIntervals, DayOffsetsChanged, DayQuestions,
    DayRecord, DayUncleanEnd, EndClass, LiveFault, LoadMix, OffsetSource, OffsetsError,
    RecordableEnd, RecordedOffsets, Running, Trap,
};
pub use domain::{
    Activity, Attribution, Certainty, CorePhase, CycleEvent, DeadEndAction, DeadEndCondition,
    Decision, DefectAnswer, DefectDirection, FailureTime, FreezeChoice, HuntOutcome, HuntStage,
    ImportClass, Inference, IntervalState, MissingFact, Notice, Outcome, Placement,
    QualificationPurpose, Queue, ReproductionState, RetryTarget, RoundEvent, ShutdownReason, SmuOp,
    TaskStatus, UncoveredReason, VetoState, WarningKind,
};
pub use ruleset13::{
    ApplicationId, Binding, Budget, Candidate, CcdCap, CcdEdge, CcdVector, CellEvidence,
    CellWitness, CreditExclusion, DayEventId, DecisionId, Edge, ExposureClass, ExposureFailure,
    ExposureObserved, ExposureOrigin, Finite, InvalidNumber, LowerEndpoint, NightId, NightPlan,
    NightQualification, NightReproduction, ObservedProfile, OriginalSource, PlannedTask, Position,
    QualificationState, Rate, RateBound, RateCell, ReproductionTarget, Slope, TaskId, TaskStart,
    Tested, UnavailableBackend, UncoveredClass, WorkloadContent,
};
pub use ruleset13_day::{
    ApplyCause, CcdCursor, Classification, DayApplied, DayEvent, DayFreeze, DayHold, DayImported,
    DayInterval, DayStep, DayVeto, Escalation, FreezeSource, HoldChange, HoldReason, ImportCounts,
    ImportedRecordRef, Resolution, RetreatCause, SessionImported, StepCauses, StepKind,
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
    BackendIdentity, ControllerEvidence, CrashDetected, FactSource, Failure, FailureCarried, Mce,
    ProcessFaultEvidence, TrialCarried, TrialClass, TrialEnd, TrialId, TrialInstance, TrialIntent,
    TrialProgress, TrialSample, TrialSignal, TrialStart,
};
pub use tuner::{
    BackendRetry, CheckingChain, CheckingCycle, CheckingStep, Combination, CoreTransition,
    DeepeningRound, HostRanking, HuntEnd, HuntGroup, HuntSkipped, HuntStart, Member, Rederived,
    TunerDecision, TunerWarning,
};

macro_rules! payloads {
    ($($variant:ident($ty:ty) = $wire:literal,)+) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
        pub enum EventKind { $(#[serde(rename = $wire)] $variant,)+ }

        impl EventKind {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)+];

            pub const fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $wire,)+ }
            }
        }

        #[derive(Clone, Debug, PartialEq, Serialize)]
        #[serde(untagged)]
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
    SessionImported(SessionImported) = "session.imported",
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
    DayBootStart(DayBootStart) = "day.boot_start",
    DayCheckpoint(DayCheckpoint) = "day.checkpoint",
    DayOffsetsChanged(DayOffsetsChanged) = "day.offsets_changed",
    DayCleanStop(DayCleanStop) = "day.clean_stop",
    DayUncleanEnd(DayUncleanEnd) = "day.unclean_end",
    DayAnswer(DayAnswer) = "day.answer",
    DayRecord(DayRecord) = "day.record",
    DayFault(DayFault) = "day.fault",
    ExposureObserved(ExposureObserved) = "exposure.observed",
    CcdEdge(CcdEdge) = "ccd.edge",
    CcdCap(CcdCap) = "ccd.cap",
    NightPlan(NightPlan) = "night.plan",
    TaskStart(TaskStart) = "task.start",
    NightQualification(NightQualification) = "night.qualification",
    NightReproduction(NightReproduction) = "night.reproduction",
    DayStep(DayStep) = "day.step",
    DayApplied(DayApplied) = "day.applied",
    DayEvent(DayEvent) = "day.event",
    DayVeto(DayVeto) = "day.veto",
    DayHold(DayHold) = "day.hold",
    DayFreeze(DayFreeze) = "day.freeze",
    DayInterval(DayInterval) = "day.interval",
    DayImported(DayImported) = "day.imported",
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
            Self::ExposureObserved(p) => p.source.trial.as_ref(),
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
            | Self::StateRebuilt(_)
            | Self::DayBootStart(_)
            | Self::DayCheckpoint(_)
            | Self::DayOffsetsChanged(_)
            | Self::DayCleanStop(_)
            | Self::DayUncleanEnd(_)
            | Self::DayAnswer(_)
            | Self::DayRecord(_)
            | Self::DayFault(_)
            | Self::SessionImported(_)
            | Self::CcdEdge(_)
            | Self::CcdCap(_)
            | Self::NightPlan(_)
            | Self::TaskStart(_)
            | Self::NightQualification(_)
            | Self::NightReproduction(_)
            | Self::DayStep(_)
            | Self::DayApplied(_)
            | Self::DayEvent(_)
            | Self::DayVeto(_)
            | Self::DayHold(_)
            | Self::DayFreeze(_)
            | Self::DayInterval(_)
            | Self::DayImported(_) => None,
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
            Self::TrialProgress(p) => {
                p.core == Some(core)
                    || p.fault.as_ref().is_some_and(|fault| {
                        fault.core == Some(core) || fault.worker_core == Some(core)
                    })
            }
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
            Self::DayFault(p) => p.fault.core() == Some(core),
            Self::ExposureObserved(p) => {
                p.class.cores.contains(core)
                    || p.failures.iter().any(|failure| failure.core == Some(core))
            }
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
            | Self::StateRebuilt(_)
            | Self::DayBootStart(_)
            | Self::DayCheckpoint(_)
            | Self::DayOffsetsChanged(_)
            | Self::DayCleanStop(_)
            | Self::DayUncleanEnd(_)
            | Self::DayAnswer(_)
            | Self::DayRecord(_)
            | Self::SessionImported(_)
            | Self::CcdEdge(_)
            | Self::CcdCap(_)
            | Self::NightPlan(_)
            | Self::TaskStart(_)
            | Self::NightQualification(_)
            | Self::NightReproduction(_)
            | Self::DayStep(_)
            | Self::DayApplied(_)
            | Self::DayEvent(_)
            | Self::DayVeto(_)
            | Self::DayHold(_)
            | Self::DayFreeze(_)
            | Self::DayInterval(_)
            | Self::DayImported(_) => false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("unknown event kind {0:?}")]
pub struct UnknownKind(pub String);

impl EventKind {
    /// Whether this is a day kind: a `togi day` observer event, or a Ruleset 13 day decision, event or import.
    /// Observer events record what the machine did outside tuning and never feed a tuning decision.
    pub const fn is_day(self) -> bool {
        matches!(
            self,
            Self::DayBootStart
                | Self::DayCheckpoint
                | Self::DayOffsetsChanged
                | Self::DayCleanStop
                | Self::DayUncleanEnd
                | Self::DayAnswer
                | Self::DayRecord
                | Self::DayFault
                | Self::DayStep
                | Self::DayApplied
                | Self::DayEvent
                | Self::DayVeto
                | Self::DayHold
                | Self::DayFreeze
                | Self::DayInterval
                | Self::DayImported
        )
    }

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
