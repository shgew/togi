use std::error::Error as StdError;
use std::fmt::Write as _;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("hardware runs need Linux: unsupported operation")]
    Unsupported,
    #[error("open the SMU driver")]
    OpenSmu(#[source] togi_smu::Error),
    #[error("read the CPU topology")]
    Topology(#[source] togi_smu::Error),
    #[error("start the trial runner")]
    TrialRunner(#[source] togi_trial::Error),
    #[error("index the logical CPUs of each core for the kernel log")]
    KernelLog(#[source] togi_detect::DuplicateCpu),
    #[error("{operation}: running as uid {uid}; this needs root")]
    NeedsRoot { operation: &'static str, uid: u32 },
}

impl From<Error> for togi_machine::Error {
    fn from(error: Error) -> Self {
        Self::Other(Box::new(error))
    }
}

pub(crate) fn describe(error: &dyn StdError) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let _ = write!(text, ": {cause}");
        source = cause.source();
    }
    text
}
