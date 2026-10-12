//! The state directory: one writer at a time, and every file the journal keeps in it.

mod archive;
mod identity;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::fs::{DiskFs, FileKind, Fs, FsLock, MemFs};
use crate::session_id::SessionId;

pub use archive::{ArchiveKind, Lifetime, Marker};

pub(crate) const LOCK_FILE: &str = "lock";
pub(crate) const EVENTS_FILE: &str = "events.jsonl";
pub(crate) const EVENTS_TMP: &str = ".events.jsonl.tmp";
pub(crate) const STATE_FILE: &str = "state.json";
pub(crate) const STATE_TMP: &str = ".state.json.tmp";
pub(crate) const ARCHIVE_DIR: &str = "archive";
pub(crate) const TRIALS_DIR: &str = "trials";
const STATE_DIR_MODE: u32 = 0o711;
const SIMULATED_ROOT: &str = "/state";

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("state directory {} is locked by another writer", path.display())]
    Held { path: PathBuf },
    #[error(transparent)]
    Dir(#[from] DirError),
}

#[derive(Debug, thiserror::Error)]
pub enum DirError {
    #[error("{op} {}: {source}", path.display())]
    Io {
        op: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{} is not a directory", path.display())]
    NotADirectory { path: PathBuf },
    #[error("{} is not a regular file", path.display())]
    NotRegular { path: PathBuf },
    #[error("session {session} is already archived")]
    ArchiveExists { session: SessionId },
    #[error("session {session} has no archived journal to complete its archive")]
    MissingArchive { session: SessionId },
    #[error("every session id after {after} is taken")]
    SessionIdsExhausted { after: SessionId },
    #[error("{} is not a journal: it does not start with session.start", path.display())]
    NotAJournal { path: PathBuf },
    #[error("{} holds an invalid session id: {source}", path.display())]
    BadSession {
        path: PathBuf,
        #[source]
        source: crate::session_id::InvalidSessionId,
    },
    #[error("{} holds an unreadable build stamp: {source}", path.display())]
    BadBuild {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{0} markers are permanent")]
    PermanentMarker(Marker),
}

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("encode state: {0}")]
    Encode(#[source] serde_json::Error),
    #[error("decode state: {0}")]
    Decode(#[source] serde_json::Error),
    #[error(transparent)]
    Dir(#[from] DirError),
}

pub(crate) fn io_at(op: &'static str, path: &Path) -> impl FnOnce(io::Error) -> DirError {
    let path = path.to_path_buf();
    move |source| DirError::Io { op, path, source }
}

pub(crate) fn not_found(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound
}

/// A state directory held under its writer lock. Dropping it releases the lock.
#[derive(Debug)]
pub struct StateDir {
    fs: Arc<dyn Fs>,
    path: PathBuf,
    buffered: bool,
    _lock: FsLock,
}

impl StateDir {
    /// Creates the directory and every missing ancestor durably, then takes the exclusive writer lock.
    pub fn lock(path: impl AsRef<Path>) -> Result<Self, LockError> {
        Self::lock_in(Arc::new(DiskFs::default()), path.as_ref(), false)
    }

    /// A state directory on disk whose files are never synced and whose journal buffers 64 KiB of events, flushed on
    /// read, archive and close. Fast simulations only: a crash loses events. An SMU intent receipt from such a journal
    /// proves the intent reached the file in order and survives the process exiting, not that it survives power loss.
    pub fn lock_unsynced(path: impl AsRef<Path>) -> Result<Self, LockError> {
        Self::lock_in(Arc::new(DiskFs::unsynced()), path.as_ref(), true)
    }

    /// A state directory inside an in-memory filesystem; the caller keeps the `MemFs` to survive simulated reboots.
    pub fn simulated(fs: MemFs) -> Result<Self, LockError> {
        Self::lock_in(Arc::new(fs), Path::new(SIMULATED_ROOT), false)
    }

    fn lock_in(fs: Arc<dyn Fs>, path: &Path, buffered: bool) -> Result<Self, LockError> {
        create_dirs(fs.as_ref(), path)?;
        fs.set_dir_mode(path, STATE_DIR_MODE)
            .map_err(io_at("set directory mode", path))?;
        let lock_path = path.join(LOCK_FILE);
        let lock = fs
            .try_lock(&lock_path)
            .map_err(io_at("lock", &lock_path))?
            .ok_or_else(|| LockError::Held {
                path: path.to_path_buf(),
            })?;
        Ok(Self {
            fs,
            path: path.to_path_buf(),
            buffered,
            _lock: lock,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn trials_dir(&self) -> PathBuf {
        self.path.join(TRIALS_DIR)
    }

    pub(crate) fn fs(&self) -> &dyn Fs {
        self.fs.as_ref()
    }

    pub(crate) const fn buffered(&self) -> bool {
        self.buffered
    }

    pub(crate) fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }

    pub(crate) fn archive_dir(&self) -> PathBuf {
        self.join(ARCHIVE_DIR)
    }

    pub(crate) fn sync_dir(&self, path: &Path) -> Result<(), DirError> {
        self.fs
            .sync_dir(path)
            .map_err(io_at("sync directory", path))
    }

    pub(crate) fn sync_dirs(&self) -> Result<(), DirError> {
        self.sync_dir(&self.archive_dir())?;
        self.sync_dir(&self.path)
    }

    /// Publishes the document as `state.json`: a reader sees the old document or the new one, never a mix.
    ///
    /// Publication takes `&mut self` because it shares one temporary file: two publications at once could truncate a
    /// document the other has already renamed into place.
    ///
    /// ```compile_fail,E0596
    /// fn publish(dir: &togi_journal::StateDir) {
    ///     let _ = dir.write_state(&1);
    /// }
    /// ```
    pub fn write_state<T: Serialize>(&mut self, state: &T) -> Result<(), StateError> {
        let mut bytes = serde_json::to_vec(state).map_err(StateError::Encode)?;
        bytes.push(b'\n');
        let tmp = self.join(STATE_TMP);
        let target = self.join(STATE_FILE);
        let publish = || -> io::Result<()> {
            match self.fs.remove_file(&tmp) {
                Err(error) if !not_found(&error) => return Err(error),
                _ => {}
            }
            let mut file = self.fs.create_new(&tmp)?;
            file.write_all(&bytes)?;
            file.sync()?;
            drop(file);
            self.fs.rename(&tmp, &target)
        };
        publish().map_err(io_at("publish state", &target))?;
        Ok(self.sync_dir(&self.path)?)
    }

    /// The published document; `None` when none was written, an error when it does not decode.
    pub fn read_state<T: DeserializeOwned>(&self) -> Result<Option<T>, StateError> {
        let target = self.join(STATE_FILE);
        let bytes = match self.fs.read(&target) {
            Ok(bytes) => bytes,
            Err(error) if not_found(&error) => return Ok(None),
            Err(error) => return Err(io_at("read state", &target)(error).into()),
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(StateError::Decode)
    }

    pub(crate) fn kind_of(&self, path: &Path) -> Result<Option<FileKind>, DirError> {
        self.fs.kind(path).map_err(io_at("stat", path))
    }
}

fn create_dirs(fs: &dyn Fs, path: &Path) -> Result<(), DirError> {
    let mut chain = Vec::new();
    for ancestor in path
        .ancestors()
        .take_while(|ancestor| !ancestor.as_os_str().is_empty())
    {
        let kind = fs.kind(ancestor).map_err(io_at("stat", ancestor))?;
        match kind {
            Some(FileKind::Dir | FileKind::Symlink) | None => chain.push((ancestor, kind)),
            Some(_) => {
                return Err(DirError::NotADirectory {
                    path: ancestor.to_path_buf(),
                });
            }
        }
        if kind == Some(FileKind::Symlink) {
            break;
        }
    }
    for (dir, kind) in chain.iter().rev() {
        if kind.is_none() {
            match fs.create_dir(dir) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(io_at("create directory", dir)(error)),
            }
        }
    }
    for (dir, _) in &chain {
        let Some(parent) = dir.parent() else { continue };
        let parent = if parent.as_os_str().is_empty() {
            Path::new(".")
        } else {
            parent
        };
        fs.sync_dir(parent)
            .map_err(io_at("sync directory", parent))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
