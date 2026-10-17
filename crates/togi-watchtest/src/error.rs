use std::num::NonZeroU32;
use std::path::PathBuf;

use togi_journal::{AppendError, CloseError, DirError, LockError, OpenError};
use togi_session::SessionError;
use togi_sim::{InvalidMachine, ScriptError};
use togi_simrun::{HistoryError, SimError};
use togi_tuner::FoldError;

use crate::Scenario;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("simulate: {}", togi_machine::error_chain(.0))]
    Simulate(#[from] SimError),
    #[error("the simulated run ended with errors: {}", listed(.0))]
    Session(Vec<SessionError>),
    #[error("read the simulated journal: {}", togi_machine::error_chain(.0))]
    History(#[from] HistoryError),
    #[error("the simulation did not stop within {0} boots")]
    Capped(NonZeroU32),
    #[error("build the simulated machine: {0}")]
    Machine(#[from] InvalidMachine),
    #[error("script a trial: {0}")]
    Script(#[from] ScriptError),
    #[error("configuration: {}", togi_machine::error_chain(.0))]
    Config(#[from] togi_config::ConfigError),
    #[error("fold the simulated journal: {}", togi_machine::error_chain(.0))]
    Fold(#[from] FoldError),
    #[error("no cut point in the simulated journal: {0}")]
    NoCut(&'static str),
    #[error("scenario {scenario}: {source}")]
    Scenario {
        scenario: Scenario,
        #[source]
        source: Box<Error>,
    },
    #[error("the shared simulation failed: {0}")]
    Shared(String),
    #[error("fixture: {0}")]
    Fixture(String),
    #[error("install the journal in {}: {source}", dir.display())]
    Install {
        dir: PathBuf,
        #[source]
        source: InstallError,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("lock the state directory: {}", togi_machine::error_chain(.0))]
    Lock(#[from] LockError),
    #[error("open the journal: {}", togi_machine::error_chain(.0))]
    Open(#[from] OpenError),
    #[error("append {kind} #{seq}: {}", togi_machine::error_chain(.source))]
    Append {
        kind: &'static str,
        seq: u64,
        #[source]
        source: AppendError,
    },
    #[error("close the journal: {}", togi_machine::error_chain(.0))]
    Close(#[from] CloseError),
    #[error("{}", togi_machine::error_chain(.0))]
    Dir(#[from] DirError),
    #[error("an smu.write at #{seq} follows no smu.intent")]
    OrphanWrite { seq: u64 },
    #[error("the directory already holds a journal")]
    NotEmpty,
    #[error("the journal does not open with session.start")]
    NoSession,
}

fn listed(errors: &[SessionError]) -> String {
    errors
        .iter()
        .map(|error| togi_machine::error_chain(error))
        .collect::<Vec<_>>()
        .join("; ")
}
