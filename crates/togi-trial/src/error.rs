use std::io;

use crate::files::PrepareError;
use crate::identity::IdentityError;
use crate::plan::InvalidTrial;
use crate::samples::StorageFailure;

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
    #[error("prepare the trial files")]
    Prepare(#[from] PrepareError),
    #[error("launch instance {instance} in scope {scope}")]
    Launch {
        instance: String,
        scope: String,
        #[source]
        source: io::Error,
    },
    #[error("the preflight scope probe failed")]
    Probe(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("trial cleanup could not be confirmed")]
    Containment(#[source] Containment),
    #[error("cancelled")]
    Cancelled,
    #[error("the samples file")]
    Storage(#[from] StorageFailure),
    #[error("backend output could not be read cleanly")]
    Output(#[source] OutputFailure),
    #[error("compress the trial files")]
    Compress(#[source] togi_trialfiles::Errors),
    #[error("{operation} is supported on Linux only")]
    Unsupported { operation: &'static str },
}

/// Every failure a cleanup met, in the order it met them; the first never gets replaced.
#[derive(Debug, thiserror::Error)]
#[error("{}", .failures.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
pub struct Containment {
    failures: Vec<CleanupFailure>,
}

#[derive(Debug, thiserror::Error)]
pub enum CleanupFailure {
    #[error("signal group {pgid} with {signal}")]
    SignalGroup {
        pgid: i32,
        signal: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("kill scope {scope}")]
    KillScope {
        scope: String,
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
    #[error("leftover trial unit or process remains at the sweep deadline")]
    Leftover,
    #[error("observe {what}")]
    Observe {
        what: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

/// Output I/O errors of a trial, kept whatever evidence was selected.
#[derive(Debug, thiserror::Error)]
#[error("{}", .diagnostics.join("; "))]
pub struct OutputFailure {
    pub(crate) diagnostics: Vec<String>,
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
}

impl From<Error> for togi_machine::Error {
    fn from(error: Error) -> Self {
        match error {
            Error::Containment(_) => Self::Containment,
            Error::Cancelled => Self::Cancelled,
            Error::Backend(backend) => backend.into(),
            other => Self::Other(Box::new(other)),
        }
    }
}
