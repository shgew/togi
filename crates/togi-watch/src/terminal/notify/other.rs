//! Journal notifications where inotify is not available: the journal is polled once a second, and the loop's
//! reload decides whether anything changed.

use std::path::Path;

use super::{POLL, Watch};
use crate::terminal::WatchError;

pub(in crate::terminal) struct Poller;

pub(in crate::terminal) fn open(_directory: &Path) -> Result<Poller, WatchError> {
    Ok(Poller)
}

impl Watch for Poller {
    fn polling(&self) -> bool {
        false
    }

    fn arm(&mut self) -> Result<(), WatchError> {
        Ok(())
    }

    async fn changed(&mut self) -> Result<(), WatchError> {
        tokio::time::sleep(POLL).await;
        Ok(())
    }
}
