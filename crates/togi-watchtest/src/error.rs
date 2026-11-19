use std::num::{NonZeroU32, TryFromIntError};
use std::path::PathBuf;
use std::sync::Arc;

use togi_journal::{AppendError, CloseError, DirError, InvalidSessionId, LockError, OpenError};
use togi_machine::{CoreId, InvalidCcd, InvalidCore, InvalidTarget, ObservedOffset};
use togi_session::SessionError;
use togi_simrun::{HistoryError, SimError};
use togi_tuner::policy::FoldError;

use crate::Scenario;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("simulate")]
    Simulate(#[from] SimError),
    #[error("the simulated run ended with errors: {}", listed(.0))]
    Session(Vec<SessionError>),
    #[error("read the simulated journal")]
    History(#[from] HistoryError),
    #[error("the simulation did not stop within {0} boots")]
    Capped(NonZeroU32),
    #[error("the simulation did not stop within {0} observed events")]
    EventCap(usize),
    #[error("read core {core} after shutdown")]
    StockRead {
        core: CoreId,
        #[source]
        source: togi_machine::Error,
    },
    #[error("core {core} remained at CO {} after shutdown", offset.0)]
    NotStock {
        core: CoreId,
        offset: ObservedOffset,
    },
    #[error("construct the simulated machine")]
    Construct(#[from] togi_sim::ArithmeticError),
    #[error("configuration")]
    Config(#[from] togi_config::ConfigError),
    #[error("fold the simulated journal")]
    Fold(#[from] FoldError),
    #[error("lock the staged day journal")]
    DayLock(#[from] LockError),
    #[error("open the staged day journal")]
    DayOpen(#[from] OpenError),
    #[error("append to the staged day journal")]
    DayAppend(#[from] AppendError),
    #[error("close the staged day journal")]
    DayClose(#[from] CloseError),
    #[error("access the staged day machine")]
    DayMachine(#[from] togi_machine::Error),
    #[error("query the staged day policy")]
    DayPolicy(#[from] togi_tuner::policy::NotReady),
    #[error("build the staged CCD profile")]
    DayProfile(#[from] togi_machine::CcdProfileError),
    #[error("apply the staged day profile")]
    DayWrite(#[from] togi_session::DayWriteError),
    #[error("no cut point in the simulated journal: {0}")]
    NoCut(&'static str),
    #[error("scenario {scenario}")]
    Scenario {
        scenario: Scenario,
        #[source]
        source: Box<Error>,
    },
    /// The simulation every caller of one base shares failed; each caller gets the same failure.
    #[error("run the shared simulation")]
    Shared(#[source] Arc<Error>),
    #[error("fixture: {0}")]
    Fixture(String),
    #[error("pick a fixture core")]
    Core(#[from] InvalidCore),
    #[error("pick a fixture CCD")]
    Ccd(#[from] InvalidCcd),
    #[error("pick a fixture offset")]
    Offset(#[from] InvalidTarget),
    #[error("name the fixture session")]
    SessionId(#[from] InvalidSessionId),
    #[error("stamp a fixture event")]
    Stamp(#[from] jiff::Error),
    #[error("index the fixture journal")]
    Index(#[from] TryFromIntError),
    #[error("install the journal in {}", dir.display())]
    Install {
        dir: PathBuf,
        #[source]
        source: InstallError,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("lock the state directory")]
    Lock(#[from] LockError),
    #[error("open the journal")]
    Open(#[from] OpenError),
    #[error("append {kind} #{seq}")]
    Append {
        kind: &'static str,
        seq: u64,
        #[source]
        source: AppendError,
    },
    #[error("reboot the journal into the boot at #{seq}")]
    Reboot {
        seq: u64,
        #[source]
        source: AppendError,
    },
    #[error("close the journal")]
    Close(#[from] CloseError),
    #[error(transparent)]
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
