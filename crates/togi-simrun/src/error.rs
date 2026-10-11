use std::io;

use togi_journal::{CloseError, LockError, LookupError, OpenError};
use togi_session::Divergence;

#[derive(Debug, thiserror::Error)]
pub enum SimError {
    #[error("lock the state directory: {0}")]
    Lock(#[from] LockError),
    #[error("open the journal for boot {boot}: {source}")]
    Open {
        boot: u32,
        #[source]
        source: OpenError,
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
    #[error("close the journal of boot {boot}: {source}")]
    Close {
        boot: u32,
        #[source]
        source: CloseError,
    },
    #[error("dump the simulated state directory: {0}")]
    Dump(#[source] io::Error),
    #[error("samples are written under a state directory on disk; an in-memory one has none")]
    SamplesInMemory,
    #[error(
        "an observer reads events from the in-memory journal, which a state directory on disk does not give"
    )]
    ObserverOnDisk,
}

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error(transparent)]
    Lock(#[from] LockError),
    #[error(transparent)]
    Read(#[from] LookupError),
}
