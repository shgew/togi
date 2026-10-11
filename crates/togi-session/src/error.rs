use std::io;
use std::sync::Arc;

use togi_journal::{AppendError, DeadEndCondition, EventKind, LookupError, Refusal, Seq};

use crate::boot::BootError;
use crate::write::WriteFailure;

/// The first failed journal append of a run. Cloning shares the original error.
#[derive(Clone, Debug, thiserror::Error)]
#[error("journal write: {0}")]
pub struct JournalFailure(#[source] Arc<AppendError>);

impl JournalFailure {
    pub(crate) fn new(error: AppendError) -> Self {
        Self(Arc::new(error))
    }

    pub fn error(&self) -> &AppendError {
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
}

impl From<AppendFailure> for SessionError {
    fn from(failure: AppendFailure) -> Self {
        match failure {
            AppendFailure::Journal(failure) => Self::Journal(failure),
            AppendFailure::Fold(failure) => Self::Fold(failure),
            AppendFailure::Boot(BootError::Bootloader(error)) => Self::Bootloader(error),
            AppendFailure::Boot(BootError::Lookup(error)) => Self::Lookup(error),
        }
    }
}

/// How a step leaves the straight path: a dead end the evidence already recorded, or an error.
#[derive(Debug)]
pub enum Flow {
    DeadEnd(DeadEndCondition),
    Fatal(SessionError),
}

pub type Step<T> = Result<T, Flow>;

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
