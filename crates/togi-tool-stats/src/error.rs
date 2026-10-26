use togi_journal::FileError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Journal(#[from] FileError),
    #[error("the journal has no events")]
    Empty,
    #[error("the journal does not open with session.start")]
    NotStarted,
    #[error("convert recorded boot uptime")]
    BootDuration(#[source] jiff::Error),
    #[error("reconstruct recorded boot start")]
    BootStart(#[source] jiff::Error),
    #[error("recorded voltage request {0} cannot be represented as a 4 mV bin")]
    VoltageBin(f64),
}
