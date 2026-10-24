use std::fmt;
use std::process::ExitStatus;
use std::time::Duration;

use crate::cpu_times::CpuTimesError;
use crate::fault::FaultNumberError;
use crate::journal::DecodeError;
use crate::parse::StatusError;
use crate::pstore::PstoreError;
use crate::reset::VersionError;

/// What a failed journal command was asked to do.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Operation {
    ReadKernelLog { boot: String },
    ReadResetReason { boot: String },
    ListBoots,
    ReadJournalBoundary { boot: String },
}

impl fmt::Display for Operation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ReadKernelLog { boot } => write!(f, "read the kernel log of boot {boot}"),
            Self::ReadResetReason { boot } => write!(f, "read the reset reason of boot {boot}"),
            Self::ListBoots => f.write_str("list the boots of the system journal"),
            Self::ReadJournalBoundary { boot } => {
                write!(f, "read a journal boundary of boot {boot}")
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{id:?} is not a boot id")]
    InvalidBootId { id: String },
    #[error("cannot {operation}")]
    Command {
        operation: Operation,
        #[source]
        source: togi_command::Error,
    },
    #[error("cannot {operation}: journalctl only runs on Linux")]
    Unsupported { operation: Operation },
    #[error("cannot {operation}: the {budget:?} budget of the call is used up")]
    BudgetExhausted {
        operation: Operation,
        budget: Duration,
    },
    #[error("cannot {operation}: journalctl {status}: {stderr}")]
    Exit {
        operation: Operation,
        status: ExitStatus,
        stderr: String,
    },
    #[error("cannot {operation}: an entry cannot be decoded")]
    Decode {
        operation: Operation,
        #[source]
        source: DecodeError,
    },
    #[error("cannot {operation}")]
    Status {
        operation: Operation,
        #[source]
        source: StatusError,
    },
    #[error("cannot {operation}")]
    FaultStatus {
        operation: Operation,
        #[source]
        source: FaultNumberError,
    },
    #[error("cannot {operation}")]
    Version {
        operation: Operation,
        #[source]
        source: VersionError,
    },
    #[error("boot missing from the system journal")]
    BootMissing,
    #[error("kernel log cursor cannot be resumed")]
    CursorMissing,
    #[error(transparent)]
    Pstore(#[from] PstoreError),
    #[error("cannot read the boot id")]
    BootId(#[source] BootIdError),
    #[error("cannot read process thread identities")]
    ProcessThreads(#[source] std::io::Error),
    #[error("cannot read the per-CPU scheduler times")]
    CpuTimes(#[source] std::io::Error),
    #[error("cannot decode the per-CPU scheduler times")]
    CpuTimesFormat(#[source] CpuTimesError),
}

#[derive(Debug, thiserror::Error)]
pub enum BootIdError {
    #[error(transparent)]
    Read(#[from] std::io::Error),
    #[error(transparent)]
    Command(#[from] togi_command::Error),
    #[error("sysctl {status}: {stderr}")]
    Exit { status: ExitStatus, stderr: String },
    #[error("the boot id is not text")]
    NotText,
    #[error("{0:?} is not a boot id")]
    Invalid(String),
    #[error("boot ids can be read on Linux and macOS only")]
    Unsupported,
}

impl From<Error> for togi_machine::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::BootMissing => Self::BootMissing,
            Error::CursorMissing => Self::CursorMissing,
            other @ (Error::InvalidBootId { .. }
            | Error::Command { .. }
            | Error::Unsupported { .. }
            | Error::BudgetExhausted { .. }
            | Error::Exit { .. }
            | Error::Decode { .. }
            | Error::Status { .. }
            | Error::FaultStatus { .. }
            | Error::Version { .. }
            | Error::Pstore(_)
            | Error::BootId(_)
            | Error::ProcessThreads(_)
            | Error::CpuTimes(_)
            | Error::CpuTimesFormat(_)) => Self::Other(Box::new(other)),
        }
    }
}
