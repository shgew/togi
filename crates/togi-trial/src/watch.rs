use std::ops::ControlFlow;
use std::os::fd::OwnedFd;
use std::sync::Arc;

use togi_backend::FileName;

use crate::output::Frame;

/// A file's identity: replacing it by another inode is detected, never silently resumed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FileId {
    pub(crate) dev: u64,
    pub(crate) ino: u64,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum WatchError {
    #[error("unsupported watched output file")]
    Unsupported,
    #[error("watched output lost before draining")]
    Lost,
    #[error("read watched output")]
    Read(#[source] std::io::Error),
    #[error("backend output line exceeds 64 KiB")]
    LineTooLong,
}

#[derive(Debug)]
pub(crate) enum Poll {
    /// Nothing new, or the file does not exist yet.
    Idle,
    /// Read to the end of what existed at the start of the poll.
    Drained,
    /// The per-poll byte budget or the stop condition ended the poll with bytes still unread.
    Pending,
    Failed(WatchError),
}

/// One regular file the backend writes in its instance directory, tailed without blocking.
#[derive(Debug)]
pub(crate) struct Watched {
    dir: Arc<OwnedFd>,
    name: FileName,
}

/// Evidence that the processes able to write the file are gone, required before its final
/// unterminated line counts.
#[derive(Clone, Copy, Debug)]
pub(crate) struct WritersStopped(());

impl WritersStopped {
    pub(crate) const fn confirmed() -> Self {
        Self(())
    }
}

impl Watched {
    pub(crate) fn new(dir: Arc<OwnedFd>, name: FileName) -> Self {
        Self { dir, name }
    }

    pub(crate) fn name(&self) -> &FileName {
        &self.name
    }

    /// Reads at most 64 KiB in 4 KiB chunks from the saved offset. `sink` breaks to stop between
    /// lines; the offset then sits exactly after the last consumed byte.
    pub(crate) fn poll(&mut self, sink: &mut impl FnMut(Frame) -> ControlFlow<()>) -> Poll {
        let _ = sink;
        unimplemented!()
    }

    /// Whether bytes already accepted are still unread, so the file's disappearance loses evidence.
    pub(crate) fn backlog(&self) -> bool {
        unimplemented!()
    }

    /// The final unterminated line, once the file is fully drained and its writers are gone.
    pub(crate) fn finish(&mut self, writers: WritersStopped) -> Option<String> {
        let _ = writers;
        unimplemented!()
    }
}
