//! Journal notifications from inotify.
//!
//! The state directory is watched, so the journal's creation, replacement and removal are seen, and its parent
//! too, so the directory's own replacement is. Where the state directory cannot be watched (a root run leaves it
//! traverse-only) the journal itself is watched. While nothing at all can be watched for the journal, it is looked
//! for once an interval.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use rustix::fs::inotify::{self, CreateFlags, ReadFlags, WatchFlags};
use rustix::io::Errno;
use tokio::io::unix::AsyncFd;

use super::Watch;
use crate::terminal::WatchError;

const JOURNAL: &str = "events.jsonl";

const DIRECTORY: WatchFlags = WatchFlags::MODIFY
    .union(WatchFlags::CLOSE_WRITE)
    .union(WatchFlags::ATTRIB)
    .union(WatchFlags::CREATE)
    .union(WatchFlags::MOVED_FROM)
    .union(WatchFlags::MOVED_TO)
    .union(WatchFlags::DELETE)
    .union(WatchFlags::DELETE_SELF)
    .union(WatchFlags::MOVE_SELF)
    .union(WatchFlags::ONLYDIR);

const FILE: WatchFlags = WatchFlags::MODIFY
    .union(WatchFlags::CLOSE_WRITE)
    .union(WatchFlags::ATTRIB)
    .union(WatchFlags::DELETE_SELF)
    .union(WatchFlags::MOVE_SELF);

fn failed(operation: &'static str, errno: Errno) -> WatchError {
    WatchError {
        operation,
        source: io::Error::from(errno),
    }
}

/// The inotify watches for one journal.
pub(in crate::terminal) struct Inotify {
    fd: AsyncFd<OwnedFd>,
    directory: PathBuf,
    journal: PathBuf,
    watches: HashMap<i32, PathBuf>,
    polling: bool,
}

/// Starts watching the journal in `directory`, which has to be called inside the runtime.
pub(in crate::terminal) fn open(directory: &Path) -> Result<Inotify, WatchError> {
    let directory = std::path::absolute(directory).map_err(|source| WatchError {
        operation: "resolve state directory",
        source,
    })?;
    let fd = inotify::init(CreateFlags::CLOEXEC | CreateFlags::NONBLOCK)
        .map_err(|errno| failed("open journal notifications", errno))?;
    let fd = AsyncFd::new(fd).map_err(|source| WatchError {
        operation: "open journal notifications",
        source,
    })?;
    let mut watcher = Inotify {
        journal: directory.join(JOURNAL),
        directory,
        fd,
        watches: HashMap::new(),
        polling: false,
    };
    watcher.arm()?;
    Ok(watcher)
}

impl Inotify {
    /// Whether an event is about the journal: it, the state directory, or one of the directories above it that is
    /// watched for the state directory's sake.
    fn is_relevant(&self, watched: &Path, flags: ReadFlags, name: Option<&OsStr>) -> bool {
        if flags.contains(ReadFlags::QUEUE_OVERFLOW) {
            return true;
        }
        if flags.intersects(ReadFlags::IGNORED | ReadFlags::DELETE_SELF | ReadFlags::MOVE_SELF)
            || watched == self.journal
        {
            return true;
        }
        if watched == self.directory {
            return name == Some(OsStr::new(JOURNAL));
        }
        let first = self
            .directory
            .strip_prefix(watched)
            .ok()
            .and_then(|below| below.components().next());
        match (first, name) {
            (Some(first), Some(name)) => first.as_os_str() == name,
            _ => false,
        }
    }

    /// Reads every pending event and reports whether any of them matters.
    fn drain(&self) -> io::Result<bool> {
        let mut buf = [MaybeUninit::uninit(); 4096];
        let mut reader = inotify::Reader::new(self.fd.get_ref(), &mut buf);
        let mut relevant = false;
        loop {
            let event = match reader.next() {
                Ok(event) => event,
                Err(Errno::AGAIN) => return Ok(relevant),
                Err(errno) => return Err(errno.into()),
            };
            let name = event
                .file_name()
                .map(|name| OsStr::from_bytes(name.to_bytes()));
            if event.events().contains(ReadFlags::QUEUE_OVERFLOW) {
                relevant = true;
            } else if let Some(watched) = self.watches.get(&event.wd()) {
                relevant |= self.is_relevant(watched, event.events(), name);
            }
        }
    }
}

impl Watch for Inotify {
    fn polling(&self) -> bool {
        self.polling
    }

    fn arm(&mut self) -> Result<(), WatchError> {
        let fd = self.fd.get_ref();
        let mut watches = HashMap::with_capacity(2);
        let mut polling = false;
        let mut path = self.directory.clone();
        loop {
            match inotify::add_watch(fd, &path, DIRECTORY) {
                Ok(wd) => {
                    watches.insert(wd, path.clone());
                    if path != self.directory {
                        break;
                    }
                }
                Err(Errno::ACCESS) if path == self.directory => {
                    match inotify::add_watch(fd, &self.journal, FILE) {
                        Ok(wd) => {
                            watches.insert(wd, self.journal.clone());
                        }
                        Err(Errno::NOENT) => polling = true,
                        Err(errno) => return Err(failed("watch journal", errno)),
                    }
                }
                Err(Errno::ACCESS) => {
                    polling = polling || watches.is_empty();
                    break;
                }
                Err(Errno::NOENT | Errno::NOTDIR) => {}
                Err(errno) => return Err(failed("watch state directory", errno)),
            }
            let Some(parent) = path.parent() else {
                if polling || !watches.is_empty() {
                    break;
                }
                return Err(failed("watch state directory", Errno::NOENT));
            };
            path = parent.to_path_buf();
        }
        for wd in self.watches.keys() {
            if !watches.contains_key(wd) {
                let _ = inotify::remove_watch(fd, *wd);
            }
        }
        self.watches = watches;
        self.polling = polling;
        Ok(())
    }

    async fn changed(&mut self) -> Result<(), WatchError> {
        loop {
            let mut ready = self.fd.readable().await.map_err(|source| WatchError {
                operation: "read journal notifications",
                source,
            })?;
            let relevant = self.drain().map_err(|source| WatchError {
                operation: "read journal notifications",
                source,
            })?;
            ready.clear_ready();
            if relevant {
                return self.arm();
            }
        }
    }
}
