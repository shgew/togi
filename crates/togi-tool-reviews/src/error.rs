use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::path::Path;

type Source = Box<dyn StdError + Send + Sync + 'static>;

/// A failure whose text holds its whole cause chain, `context: cause`, as the tool prints it.
#[derive(Debug)]
pub struct Error {
    text: String,
    source: Option<Source>,
}

impl Error {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            source: None,
        }
    }

    pub fn wrap(context: impl fmt::Display, source: impl Into<Source>) -> Self {
        let source = source.into();
        Self {
            text: format!("{context}: {source}"),
            source: Some(source),
        }
    }

    /// A file system failure on `path`, worded `op path: reason`.
    pub fn path(op: &str, path: &Path, error: io::Error) -> Self {
        let text = format!("{op} {}: {}", path.display(), io_reason(&error));
        Self {
            text,
            source: Some(Box::new(error)),
        }
    }

    /// The first cause of type `T` in the chain.
    pub fn find<T: StdError + 'static>(&self) -> Option<&T> {
        let mut cause: Option<&(dyn StdError + 'static)> = self.source.as_deref().map(|s| s as _);
        while let Some(next) = cause {
            if let Some(found) = next.downcast_ref::<T>() {
                return Some(found);
            }
            cause = next.source();
        }
        None
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source.as_deref().map(|s| s as _)
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
