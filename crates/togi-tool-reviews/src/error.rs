use std::error::Error as StdError;
use std::io;
use std::path::Path;

type Source = Box<dyn StdError + Send + Sync + 'static>;

/// A failure of one of the tool's operations. Its text names the operation and never repeats the
/// cause, which is its source; [`Error::render`] prints the chain as the tool reports it.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A refusal the tool decides itself, with no cause below it.
    #[error("{0}")]
    Refused(String),
    /// An operation that failed because of its source.
    #[error("{operation}")]
    Failed {
        operation: String,
        #[source]
        source: Source,
    },
    /// A file system operation on a path.
    #[error("{operation} {path}")]
    Path {
        operation: &'static str,
        path: String,
        #[source]
        source: io::Error,
    },
    /// A cause reported as it is, with no operation above it.
    #[error(transparent)]
    Cause(Source),
}

impl Error {
    pub fn new(text: impl Into<String>) -> Self {
        Self::Refused(text.into())
    }

    pub fn wrap(operation: impl Into<String>, source: impl Into<Source>) -> Self {
        Self::Failed {
            operation: operation.into(),
            source: source.into(),
        }
    }

    /// A file system failure on `path`, rendered `operation path: reason`.
    pub fn path(operation: &'static str, path: &Path, source: io::Error) -> Self {
        Self::Path {
            operation,
            path: path.display().to_string(),
            source,
        }
    }

    pub fn cause(source: impl Into<Source>) -> Self {
        Self::Cause(source.into())
    }

    /// The chain from this error down, `operation: cause`, with an I/O cause worded by [`io_reason`].
    pub fn render(&self) -> String {
        let mut parts = Vec::new();
        for link in self.chain() {
            if let Some(error) = link.downcast_ref::<io::Error>() {
                parts.push(io_reason(error));
                break;
            }
            parts.push(link.to_string());
        }
        parts.join(": ")
    }

    /// The first error of type `T` in the chain.
    pub fn find<T: StdError + 'static>(&self) -> Option<&T> {
        self.chain().find_map(|link| link.downcast_ref::<T>())
    }

    /// The errors from this one down its sources, a transparent [`Error::Cause`] replaced by its cause.
    fn chain(&self) -> impl Iterator<Item = &(dyn StdError + 'static)> {
        std::iter::successors(Some(unwrap_cause(self)), |&link| {
            link.source().map(unwrap_cause)
        })
    }
}

/// A transparent [`Error::Cause`] stands for its cause in the chain.
fn unwrap_cause<'a>(link: &'a (dyn StdError + 'static)) -> &'a (dyn StdError + 'static) {
    match link.downcast_ref::<Error>() {
        Some(Error::Cause(cause)) => cause.as_ref(),
        _ => link,
    }
}

/// The system's reason for an I/O failure, lower-cased and without the error number.
pub fn io_reason(error: &io::Error) -> String {
    let Some(code) = error.raw_os_error() else {
        return error.to_string();
    };
    let text = io::Error::from_raw_os_error(code).to_string();
    let text = text
        .strip_suffix(&format!(" (os error {code})"))
        .unwrap_or(&text);
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    })
}
