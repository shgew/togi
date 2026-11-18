//! Live journal watching is unsupported without Linux inotify.

use std::path::Path;

use super::Watch;
use crate::terminal::WatchError;

#[derive(Debug)]
pub(in crate::terminal) enum Unsupported {}

pub(in crate::terminal) fn open(_directory: &Path) -> Result<Unsupported, WatchError> {
    Err(WatchError {
        operation: "live watching needs Linux",
        source: std::io::ErrorKind::Unsupported.into(),
    })
}

impl Watch for Unsupported {
    fn polling(&self) -> bool {
        match *self {}
    }

    fn arm(&mut self) -> Result<(), WatchError> {
        match *self {}
    }

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "an unsupported watcher cannot exist"
    )]
    async fn changed(&mut self) -> Result<(), WatchError> {
        match *self {}
    }
}

#[cfg(test)]
mod tests {
    use super::{Path, open};

    #[test]
    fn live_journal_notifications_require_linux() {
        let error = open(Path::new("state")).unwrap_err();
        assert_eq!(error.to_string(), "live watching needs Linux");
        assert_eq!(error.source.kind(), std::io::ErrorKind::Unsupported);
    }
}
