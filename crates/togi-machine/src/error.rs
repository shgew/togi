use std::fmt;

/// Why a machine seam call failed. The named variants are the conditions callers branch on.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the simulated machine crashed")]
    Crashed,
    #[error("backend binary missing")]
    BackendMissing,
    #[error("trial cleanup could not be confirmed")]
    Containment,
    #[error("boot missing from the system journal")]
    BootMissing,
    #[error("kernel log cursor cannot be resumed")]
    CursorMissing,
    #[error("cancelled")]
    Cancelled,
    #[error(transparent)]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

/// What a seam call accomplished before `error` ended it.
///
/// Displays as `error` and reports `error`'s source, so `?` and error chains read the same as for
/// the bare error.
#[derive(Debug)]
pub struct Partial<T> {
    pub value: T,
    pub error: Error,
}

impl<T> Partial<T> {
    pub fn into_parts(self) -> (T, Error) {
        (self.value, self.error)
    }
}

impl<T> fmt::Display for Partial<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

impl<T: fmt::Debug> std::error::Error for Partial<T> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        std::error::Error::source(&self.error)
    }
}
