use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use togi_machine::{CoreId, CpuId};

use crate::identity::IdentityError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{operation} blocks and cannot run inside an async runtime")]
    InsideRuntime { operation: &'static str },
    #[error("start the trial runtime thread")]
    RuntimeThread(#[source] io::Error),
    #[error("build the trial runtime")]
    RuntimeBuild(#[source] io::Error),
    #[error("the trial runtime thread is no longer running")]
    RuntimeGone,
    #[error("the trial specification is invalid")]
    Invalid(#[from] InvalidTrial),
    #[error("backend account")]
    Identity(#[from] IdentityError),
    #[error("backend")]
    Backend(#[from] togi_backend::Error),
    #[error("the trial runtime did not answer {operation} within {bound:?}")]
    Stuck {
        operation: &'static str,
        bound: std::time::Duration,
    },
    #[error("no {0} backend is configured")]
    BackendMissing(togi_machine::Backend),
    #[error("prepare the trial files")]
    Prepare(#[from] PrepareError),
    #[error("launch instance {instance} in scope {scope}")]
    Launch {
        instance: String,
        scope: String,
        #[source]
        source: io::Error,
    },
    #[error("take the output pipes of instance {instance}")]
    PipeSetup { instance: String },
    #[error("start the output log writer")]
    LogWriter(#[source] io::Error),
    #[error("signal instance {instance} with {signal}")]
    Signal {
        instance: String,
        signal: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("the preflight scope probe failed: {detail}")]
    Probe {
        detail: String,
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },
    #[error("trial cleanup could not be confirmed")]
    Containment(#[source] Arc<Containment>),
    #[error("cancelled")]
    Cancelled,
    #[error("trial supervision panicked: {what}")]
    Panicked { what: String },
    #[error("the samples file")]
    Storage(#[from] StorageFailure),
    #[error("backend output could not be read cleanly")]
    Output(#[source] OutputFailure),
    #[error("compress the trial files")]
    Compress(#[source] togi_trialfiles::Errors),
    #[error("{operation} is supported on Linux only")]
    Unsupported { operation: &'static str },
    #[error(transparent)]
    Joined(Joined),
}

impl Error {
    /// One error for several that happened, the first kept first; `None` for none.
    #[cfg(target_os = "linux")]
    pub(crate) fn join(mut errors: Vec<Self>) -> Option<Self> {
        match errors.len() {
            0 => None,
            1 => errors.pop(),
            _ => Some(Self::Joined(Joined { errors })),
        }
    }

    /// `first` and then what happened after it; `first` alone when nothing did.
    #[cfg(target_os = "linux")]
    pub(crate) fn join_after(first: Self, later: Vec<Self>) -> Self {
        if later.is_empty() {
            return first;
        }
        let mut errors = Vec::with_capacity(later.len() + 1);
        errors.push(first);
        errors.extend(later);
        Self::Joined(Joined { errors })
    }
}

/// Several errors of one operation, in the order they happened; each member reads with its
/// own causes, so the list has no source of its own.
#[derive(Debug, thiserror::Error)]
#[error("{}", .errors.iter().map(|member| togi_machine::error_chain(member)).collect::<Vec<_>>().join("; "))]
pub struct Joined {
    errors: Vec<Error>,
}

impl Joined {
    pub fn errors(&self) -> &[Error] {
        &self.errors
    }
}

/// Output I/O errors of a trial, kept whatever evidence was selected.
#[derive(Debug, thiserror::Error)]
#[error("{}", .diagnostics.join("; "))]
pub struct OutputFailure {
    pub(crate) diagnostics: Vec<String>,
}

impl OutputFailure {
    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }
}

/// Every failure a cleanup met, in the order it met them; the first never gets replaced. Each
/// failure reads with its own causes, so the list has no source of its own.
#[derive(Debug, thiserror::Error)]
#[error("{}", .failures.iter().map(|failure| togi_machine::error_chain(failure)).collect::<Vec<_>>().join("; "))]
pub struct Containment {
    failures: Vec<CleanupFailure>,
}

#[derive(Debug, thiserror::Error)]
pub enum CleanupFailure {
    /// The error that made the start roll back; it stays first.
    #[error("the start failed")]
    Start(#[source] Box<Error>),
    #[error("signal group {pgid} with {signal}")]
    SignalGroup {
        pgid: i32,
        signal: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("signal scope {scope} with {signal}")]
    KillScope {
        scope: String,
        signal: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("stop scope {scope}")]
    StopScope {
        scope: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("reap instance {instance}")]
    Reap {
        instance: String,
        #[source]
        source: io::Error,
    },
    #[error("read the {stream} of instance {instance}")]
    ReadPipe {
        instance: String,
        stream: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("watched output of instance {instance} lost before draining")]
    WatchedLost { instance: String },
    #[error("backend exit and output drain not confirmed before the teardown deadline")]
    DrainDeadline,
    #[error("a process remains in scope {scope} at the teardown deadline")]
    ScopeMembers { scope: String },
    #[error("leftover trial unit or process remains at the sweep deadline")]
    Leftover,
    #[error("observe {what}")]
    Observe {
        what: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

impl Containment {
    pub fn new(first: CleanupFailure) -> Self {
        Self {
            failures: vec![first],
        }
    }

    pub fn push(&mut self, failure: CleanupFailure) {
        self.failures.push(failure);
    }

    pub fn failures(&self) -> &[CleanupFailure] {
        &self.failures
    }

    /// Puts the error that made the start roll back in front of the cleanup's own failures.
    #[cfg(target_os = "linux")]
    pub(crate) fn caused_by(self, cause: Error) -> Self {
        let mut failures = vec![CleanupFailure::Start(Box::new(cause))];
        failures.extend(self.failures);
        Self { failures }
    }
}

/// How an error reads at the machine seam, which has fixed categories.
enum Seam {
    Cancelled,
    Containment,
    BackendMissing,
    Other,
}

impl Error {
    /// A cleanup that could not be confirmed outranks everything joined to it; otherwise the
    /// first error names the category, and a cancellation that carries other errors stays an
    /// ordinary error so none of them is lost.
    fn seam(&self) -> Seam {
        match self {
            Self::Containment(_) | Self::Stuck { .. } => Seam::Containment,
            Self::Cancelled => Seam::Cancelled,
            Self::BackendMissing(_) => Seam::BackendMissing,
            Self::Joined(joined) => {
                if joined
                    .errors
                    .iter()
                    .any(|error| matches!(error.seam(), Seam::Containment))
                {
                    Seam::Containment
                } else if matches!(joined.errors.first(), Some(Self::BackendMissing(_))) {
                    Seam::BackendMissing
                } else {
                    Seam::Other
                }
            }
            _ => Seam::Other,
        }
    }
}

impl From<Error> for togi_machine::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Containment(containment) => Self::Containment(Box::new(containment)),
            Error::Backend(backend) => backend.into(),
            other => match other.seam() {
                Seam::Cancelled => Self::Cancelled,
                Seam::BackendMissing => Self::BackendMissing,
                Seam::Containment => Self::Containment(Box::new(other)),
                Seam::Other => Self::Other(Box::new(other)),
            },
        }
    }
}

/// An error a trial keeps after reporting it, so every later report reads the same. It names
/// the operation that reported the kept error and keeps that error as its source, so the
/// chain renders it once and callers downcast to it.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(crate) struct Shared(Arc<Error>);

#[cfg(target_os = "linux")]
impl std::fmt::Display for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("wait for the trial")
    }
}

#[cfg(target_os = "linux")]
impl std::error::Error for Shared {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.0)
    }
}

#[cfg(target_os = "linux")]
impl Shared {
    /// The seam error for a kept error, the same each time it is asked for.
    pub(crate) fn seam(error: &Arc<Error>) -> togi_machine::Error {
        match &**error {
            Error::Containment(containment) => {
                togi_machine::Error::Containment(Box::new(Arc::clone(containment)))
            }
            other => match other.seam() {
                Seam::Cancelled => togi_machine::Error::Cancelled,
                Seam::BackendMissing => togi_machine::Error::BackendMissing,
                Seam::Containment => {
                    togi_machine::Error::Containment(Box::new(Self(Arc::clone(error))))
                }
                Seam::Other => togi_machine::Error::Other(Box::new(Self(Arc::clone(error)))),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InvalidId {
    #[error("the trial id is empty")]
    Empty,
    #[error("the trial id contains {0:?}; only A-Z a-z 0-9 _ . : - are allowed")]
    Character(char),
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum InvalidTrial {
    #[error(transparent)]
    Id(#[from] InvalidId),
    #[error("the trial duration is zero")]
    ZeroDuration,
    #[error("the trial names no core")]
    NoCores,
    #[error("core {0} is listed twice")]
    DuplicateCore(CoreId),
    #[error("core {0} is not in the topology")]
    UnknownCore(CoreId),
    #[error("core {0} has no logical CPU in the topology")]
    CoreWithoutCpu(CoreId),
    #[error("the trial names no logical CPU")]
    NoCpus,
    #[error("logical CPU {0} belongs to none of the trial's cores")]
    ForeignCpu(CpuId),
    #[error("the logical CPUs do not match the first CPU of each core")]
    CpusMismatch,
    #[error("a regime that runs one instance takes exactly one core")]
    CoresForOneInstance,
    #[error("the trial duration cannot be represented as a deadline")]
    UnrepresentableDuration,
}

#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    #[error("{operation} {}", path.display())]
    Directory {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} is not a directory", path.display())]
    NotDirectory { path: PathBuf },
    #[error("{} cannot hold trials: {reason}", path.display())]
    Untrusted { path: PathBuf, reason: &'static str },
    #[error("{} is not searchable by uid {uid} gid {gid}", path.display())]
    Inaccessible { path: PathBuf, uid: u32, gid: u32 },
    #[error("create {name} in instance {instance}")]
    Create {
        instance: String,
        name: String,
        #[source]
        source: io::Error,
    },
    #[error("hand {name} of instance {instance} to the backend account")]
    Chown {
        instance: String,
        name: String,
        #[source]
        source: io::Error,
    },
    #[error("plan the backend instance")]
    Backend(#[source] togi_backend::Error),
    #[error("the backend plan names no program")]
    EmptyProgram,
    #[error("preparing {what} did not finish within {bound:?}")]
    TimedOut { what: String, bound: Duration },
    #[error("an earlier preparation is still running")]
    Busy,
    #[error("start the preparation thread")]
    Spawn(#[source] io::Error),
    #[error("the preparation thread ended without a result")]
    Vanished,
}

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("create the samples file")]
    Create(#[source] io::Error),
    #[error("sync the trial samples directory")]
    SyncDirectory(#[source] io::Error),
    #[error("encode a conditions sample")]
    Encode(#[source] serde_json::Error),
    #[error("append a conditions sample")]
    Append(#[source] io::Error),
    #[error("close the samples file")]
    Close(#[source] io::Error),
    #[error("the samples writer did not finish within {0:?}")]
    JoinTimeout(Duration),
    #[error("the samples writer thread ended without a result")]
    Vanished,
    #[error("start the samples writer thread")]
    Spawn(#[source] io::Error),
}

/// Every storage failure of one trial, in the order they happened; each member reads with its
/// own causes, so the list has no source of its own.
#[derive(Debug, thiserror::Error)]
#[error("{}", .0.iter().map(|member| togi_machine::error_chain(member)).collect::<Vec<_>>().join("; "))]
pub struct StorageFailure(pub(crate) Vec<StorageError>);

impl StorageFailure {
    pub fn errors(&self) -> &[StorageError] {
        &self.0
    }
}

impl From<StorageError> for StorageFailure {
    fn from(error: StorageError) -> Self {
        Self(vec![error])
    }
}
