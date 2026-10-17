#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("hardware runs need Linux: unsupported operation")]
    Unsupported,
    #[error("open the SMU driver")]
    OpenSmu(#[source] togi_smu::Error),
    #[error("read the CPU topology")]
    Topology(#[source] togi_smu::Error),
    #[error("build the {backend} backend")]
    Backend {
        backend: togi_machine::Backend,
        #[source]
        source: togi_backend::Error,
    },
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
