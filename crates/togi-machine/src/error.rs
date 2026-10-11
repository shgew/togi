use std::fmt;

/// Why a machine seam call failed. The named variants are the conditions callers branch on.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the simulated machine crashed")]
    Crashed,
    #[error("backend binary missing")]
    BackendMissing,
    #[error("trial cleanup could not be confirmed")]
    Containment(#[source] Box<dyn std::error::Error + Send + Sync>),
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
/// Displays as `error` and reports `error` as its source, so the named variant stays findable by
/// walking the chain of whatever boxed error `?` produced.
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
        Some(&self.error)
    }
}
