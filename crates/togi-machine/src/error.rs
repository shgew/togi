use std::fmt::{self, Write as _};

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

/// Renders an operation followed by every source, in order, without comparing their text.
#[must_use]
pub fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        let _ = write!(text, ": {cause}");
        source = cause.source();
    }
    text
}

/// What a seam call accomplished before `error` ended it.
///
/// The source keeps the machine error findable even when partial progress is boxed.
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
        f.write_str("machine operation left partial progress")
    }
}

impl<T: fmt::Debug> std::error::Error for Partial<T> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

#[cfg(test)]
mod tests;
