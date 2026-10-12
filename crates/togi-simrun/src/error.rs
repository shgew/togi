use std::io;

use togi_journal::{CloseError, LockError, LookupError, StateError};
use togi_session::{Divergence, PrepareError, SessionError};

#[derive(Debug, thiserror::Error)]
pub enum SimError {
    #[error("lock the state directory: {0}")]
    Lock(#[from] LockError),
    #[error("prepare the journal for boot {boot}: {source}")]
    Prepare {
        boot: u32,
        #[source]
        source: PrepareError,
    },
    #[error("read the boot id or BIOS context of boot {boot}: {source}")]
    Host {
        boot: u32,
        #[source]
        source: togi_machine::Error,
    },
    #[error(
        "the resume state after boot {boot} differs from a fresh fold of the journal: {source}"
    )]
    Diverged {
        boot: u32,
        #[source]
        source: Divergence,
    },
    #[error("boot {boot} left no resume state to verify: the run latched a journal failure")]
    NoCapsule { boot: u32 },
    #[error("publish the state document boot {boot} left unwritten: {source}")]
    Publish {
        boot: u32,
        #[source]
        source: StateError,
    },
    #[error("close the journal of boot {boot}: {source}")]
    Close {
        boot: u32,
        #[source]
        source: CloseError,
    },
    #[error("dump the simulated state directory: {0}")]
    Dump(#[source] io::Error),
    #[error("write the simulated machine to {}: {source}", path.display())]
    Machine {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("encode the simulated machine: {0}")]
    EncodeMachine(#[source] serde_json::Error),
    #[error("boot {boot} crashed and also failed: {}", listed(errors))]
    Failed {
        boot: u32,
        errors: Vec<SessionError>,
    },
    #[error("flush the log: {0}")]
    FlushLog(#[source] io::Error),
    #[error("samples are written under a state directory on disk; an in-memory one has none")]
    SamplesInMemory,
}

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error(transparent)]
    Lock(#[from] LockError),
    #[error(transparent)]
    Read(#[from] LookupError),
    #[error("the configuration the journal recorded: {0}")]
    Config(#[from] togi_config::ConfigError),
}

fn listed(errors: &[SessionError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}
