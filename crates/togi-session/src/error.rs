use std::io;
use std::sync::Arc;

use togi_journal::{AppendError, DeadEndCondition, DirError, EventKind, LookupError, Refusal, Seq};
use togi_machine::{CoreId, CoreSet};

use crate::boot::BootError;
use crate::write::{ProfileFailure, WriteFailure};

/// Why the journal can no longer be trusted to match the run: an append failed, or an event reached the journal that
/// the run's state cannot absorb.
#[derive(Debug, thiserror::Error)]
pub enum JournalCause {
    #[error("journal write: {0}")]
    Append(#[source] AppendError),
    #[error("an event is durable but the run's state does not take it in: {0}")]
    Fold(#[source] FoldError),
}

/// The first failure that latched a run's journal. Cloning shares the original cause.
#[derive(Clone, Debug, thiserror::Error)]
#[error(transparent)]
pub struct JournalFailure(Arc<JournalCause>);

impl JournalFailure {
    pub(crate) fn new(error: AppendError) -> Self {
        Self(Arc::new(JournalCause::Append(error)))
    }

    pub(crate) fn fold(error: FoldError) -> Self {
        Self(Arc::new(JournalCause::Fold(error)))
    }

    pub fn cause(&self) -> &JournalCause {
        &self.0
    }
}

/// A journal that cannot be folded into the run's state.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum FoldError {
    #[error("event #{seq} is {kind} before session.start")]
    BeforeStart { seq: Seq, kind: EventKind },
    #[error("event #{seq} is a second session.start")]
    DuplicateStart { seq: Seq },
    /// The tuner cannot have written the journal: an event contradicts its state.
    #[error("event #{seq} does not fold into the tuner state: {error}")]
    Tuner {
        seq: Seq,
        #[source]
        error: togi_tuner::FoldError,
    },
    #[error("event does not fold into the shared journal facts: {0}")]
    Facts(#[from] togi_journal::FoldError),
}

/// Why an event that reached the journal was not fully taken in.
#[derive(Debug, thiserror::Error)]
pub enum AppendFailure {
    /// The append itself failed: nothing is durable and the journal accepts nothing more.
    #[error(transparent)]
    Journal(#[from] JournalFailure),
    /// The event is durable but the run's state cannot absorb it.
    #[error(transparent)]
    Fold(#[from] FoldError),
    /// The event is durable; the tuning-boot handoff that followed it failed.
    #[error(transparent)]
    Boot(#[from] BootError),
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Journal(#[from] JournalFailure),
    #[error(transparent)]
    Fold(#[from] FoldError),
    #[error(transparent)]
    Machine(#[from] togi_machine::Error),
    #[error(transparent)]
    Compatibility(#[from] Refusal),
    #[error(transparent)]
    Config(#[from] togi_config::ConfigError),
    #[error(transparent)]
    Bootloader(#[from] togi_tuningboot::Error),
    #[error("look up journaled tuning-boot reason: {0}")]
    Lookup(#[from] LookupError),
    #[error(transparent)]
    Write(#[from] WriteFailure),
    #[error("write the event log: {0}")]
    Log(#[source] io::Error),
    #[error("an initial offset is configured for core {core:02}, which this machine does not have")]
    NoSuchCore { core: CoreId },
    /// The session was recorded on other cores than the machine has now: a resumed session cannot change them.
    #[error(
        "the session was recorded on other cores than this machine has now (added: {}; removed: {}); \
         archive the session to start over",
        listed(*added),
        listed(*removed)
    )]
    TopologyChanged { added: CoreSet, removed: CoreSet },
    #[error(
        "a trial is planned on core {core:02}, which this machine does not have or gives no CPU"
    )]
    NoSuchTrialCore { core: CoreId },
    /// A trial or a profile write was asked for without the proof that no workload runs.
    #[error("nothing proves that no workload runs, so nothing may be written or started")]
    Unproven,
    /// The profile a trial together was planned on is not the one the tuner stands at.
    #[error("the trial's profile is not the one the machine is set to")]
    PlanDisagrees,
    #[error("the tuner cannot choose the next action after startup: {0}")]
    NotReady(#[from] togi_tuner::NotReady),
    #[error("clear GRUB's saved entry after an incompatible journal: {0}")]
    ClearSavedEntry(#[source] io::Error),
    #[error("allocate the session id: {0}")]
    SessionId(#[from] DirError),
    /// A step reported dead-end evidence, and the journal supports no dead end.
    #[error("{0} dead-end evidence reported without a recorded dead end")]
    DeadEndUnrecorded(DeadEndCondition),
    #[error("ask whether to reset the cores of defect {defect}: {source}")]
    Prompt {
        defect: u32,
        #[source]
        source: io::Error,
    },
    /// A profile the run built is one it may not put on the machine, or not one of this machine.
    #[error("put a profile on the machine: {0}")]
    Profile(#[source] ProfileFailure),
}

impl SessionError {
    /// Whether the machine went down under the step that failed.
    pub const fn is_crash(&self) -> bool {
        matches!(
            self,
            Self::Machine(togi_machine::Error::Crashed) | Self::Write(WriteFailure::Crashed)
        )
    }
}

fn listed(cores: CoreSet) -> String {
    if cores.is_empty() {
        return "none".to_owned();
    }
    cores
        .iter()
        .map(|core| format!("{core:02}"))
        .collect::<Vec<_>>()
        .join(", ")
}

impl From<BootError> for SessionError {
    fn from(error: BootError) -> Self {
        match error {
            BootError::Bootloader(error) => Self::Bootloader(error),
            BootError::Lookup(error) => Self::Lookup(error),
        }
    }
}

impl From<AppendFailure> for SessionError {
    fn from(failure: AppendFailure) -> Self {
        match failure {
            AppendFailure::Journal(failure) => Self::Journal(failure),
            AppendFailure::Fold(failure) => Self::Fold(failure),
            AppendFailure::Boot(error) => error.into(),
        }
    }
}

/// How a step leaves the straight path: a dead end the evidence already recorded, or an error.
#[derive(Debug)]
pub(crate) enum Flow {
    DeadEnd(DeadEndCondition),
    Fatal(SessionError),
}

pub(crate) type Step<T> = Result<T, Flow>;

impl From<SessionError> for Flow {
    fn from(error: SessionError) -> Self {
        Self::Fatal(error)
    }
}

impl From<AppendFailure> for Flow {
    fn from(failure: AppendFailure) -> Self {
        Self::Fatal(failure.into())
    }
}

impl From<WriteFailure> for Flow {
    fn from(failure: WriteFailure) -> Self {
        match failure {
            WriteFailure::Smu { .. } | WriteFailure::Mismatch { .. } => {
                Self::DeadEnd(DeadEndCondition::Smu)
            }
            WriteFailure::Journal(_) | WriteFailure::Record(_) | WriteFailure::Crashed => {
                Self::Fatal(failure.into())
            }
        }
    }
}

impl From<ProfileFailure> for Flow {
    fn from(failure: ProfileFailure) -> Self {
        match failure {
            ProfileFailure::Write(failure) => failure.into(),
            ProfileFailure::Refused(_) | ProfileFailure::Shape { .. } => {
                Self::Fatal(SessionError::Profile(failure))
            }
        }
    }
}

#[cfg(test)]
mod tests;
