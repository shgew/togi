use std::collections::BTreeMap;
use std::io;
use std::ops::ControlFlow;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rustix::fs::{FileType, Mode, OFlags, fstat, openat};
use rustix::io::{Errno, pread};
use togi_backend::FileName;
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout_at};

use crate::output::{CHUNK, Frame, Framer};

/// The most one poll reads from one file.
pub(crate) const POLL_LIMIT: usize = 64 * 1024;

/// A file's identity: replacing it by another inode is detected, never silently resumed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileId {
    dev: rustix::fs::Dev,
    ino: u64,
}

/// Why a regular file the workload account does not exclusively own is never read.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum Refusal {
    #[error("the file belongs to another account")]
    Owner,
    #[error("the file has other hard links")]
    Links,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum WatchError {
    #[error("unsupported watched output file")]
    Unsupported,
    #[error("refused watched output file: {0}")]
    Refused(Refusal),
    /// Accepted bytes were not read before the file vanished, shrank, changed identity or kind.
    #[error("watched output lost before draining")]
    Lost,
    #[error("read watched output")]
    Read(#[source] io::Error),
    #[error("backend output line exceeds 64 KiB")]
    LineTooLong,
}

#[derive(Debug)]
pub(crate) enum Poll {
    /// The file does not exist yet.
    Idle,
    /// Read to the end of what existed when the poll began.
    Drained,
    /// The byte limit, the stop condition or the wait bound ended the poll with bytes unread or
    /// a read still in flight.
    Pending,
    /// The file will not be read again.
    Failed(WatchError),
}

#[derive(Debug)]
enum State {
    Unopened,
    Tailing {
        id: FileId,
        offset: u64,
        size: u64,
        drained: bool,
    },
    Stopped,
}

/// The descriptor-free adapter's read hook can fail, shrink or stall an accepted snapshot.
#[cfg(test)]
type ReadFn = dyn Fn(&MemoryRead, &mut [u8], u64) -> Result<usize, Errno> + Send + Sync;
#[cfg(test)]
type ReadAt = Arc<ReadFn>;

#[derive(Clone)]
enum Source {
    Disk {
        dir: Arc<OwnedFd>,
        name: FileName,
        owner: u32,
    },
    Memory {
        memory: Memory,
        path: PathBuf,
        owner: u32,
    },
}

/// What a tailed file's blocking owner needs to resume.
#[derive(Clone, Copy)]
struct Resume {
    id: FileId,
    offset: u64,
    size: u64,
    backlog: bool,
}

struct Request {
    source: Source,
    resume: Option<Resume>,
    buffer: Vec<u8>,
    #[cfg(test)]
    read_at: Option<ReadAt>,
}

enum Outcome {
    Missing,
    Other,
    Refused(Refusal),
    /// Accepted bytes remain unread but the file is another inode or shorter than accepted.
    Replaced,
    Failed(Errno),
    /// The file as accepted by this fetch and `got` bytes read from `start`; `error` ended the
    /// read early.
    Read {
        id: FileId,
        size: u64,
        start: u64,
        got: usize,
        error: Option<Errno>,
    },
}

struct Fetched {
    outcome: Outcome,
    buffer: Vec<u8>,
}

/// Every blocking call on a watched file: open, fstat and up to one poll's bytes. Runs on a
/// blocking thread, never on the runtime.
fn fetch(request: Request) -> Fetched {
    #[cfg(test)]
    let read_at = request.read_at;
    let Request {
        source,
        resume,
        mut buffer,
        ..
    } = request;
    let outcome = match source {
        Source::Disk { dir, name, owner } => open_and_read(&dir, &name, owner, resume, &mut buffer),
        Source::Memory {
            memory,
            path,
            owner,
        } => memory.fetch(&path, owner, resume, &mut buffer, |file, buffer, offset| {
            #[cfg(test)]
            if let Some(read_at) = &read_at {
                return read_at(file, buffer, offset);
            }
            file.read_at(buffer, offset)
        }),
    };
    Fetched { outcome, buffer }
}

fn open_and_read(
    dir: &OwnedFd,
    name: &FileName,
    owner: u32,
    resume: Option<Resume>,
    buffer: &mut Vec<u8>,
) -> Outcome {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let fd = match openat(dir.as_fd(), name.as_str(), flags, Mode::empty()) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Outcome::Missing,
        Err(Errno::LOOP | Errno::NXIO) => return Outcome::Other,
        Err(errno) => return Outcome::Failed(errno),
    };
    let stat = match fstat(&fd) {
        Ok(stat) => stat,
        Err(errno) => return Outcome::Failed(errno),
    };
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Outcome::Other;
    }
    if stat.st_uid != owner {
        return Outcome::Refused(Refusal::Owner);
    }
    if stat.st_nlink != 1 {
        return Outcome::Refused(Refusal::Links);
    }
    let id = FileId {
        dev: stat.st_dev,
        ino: stat.st_ino,
    };
    let size = u64::try_from(stat.st_size).unwrap_or(0);
    read_snapshot(id, size, resume, buffer, |buffer, offset| {
        pread(&fd, buffer, offset)
    })
}

/// Both adapters accept identity and size before reading any byte.
fn read_snapshot(
    id: FileId,
    size: u64,
    resume: Option<Resume>,
    buffer: &mut Vec<u8>,
    mut read_at: impl FnMut(&mut [u8], u64) -> Result<usize, Errno>,
) -> Outcome {
    let start = match resume {
        Some(known) if known.id == id && size >= known.size => known.offset,
        Some(known) if known.backlog => return Outcome::Replaced,
        Some(_) | None => 0,
    };
    let want = usize::try_from(size.saturating_sub(start).min(POLL_LIMIT as u64)).unwrap_or(0);
    buffer.resize(want, 0);
    let mut got = 0;
    let mut error = None;
    while got < want {
        match read_at(&mut buffer[got..], start + got as u64) {
            Ok(0) => break,
            Ok(count) => got += count,
            Err(Errno::INTR) => {}
            Err(errno) => {
                error = Some(errno);
                break;
            }
        }
    }
    Outcome::Read {
        id,
        size,
        start,
        got,
        error,
    }
}

/// Watched-output metadata beside the shared memory filesystem. This adapter only models
/// entries a fake workload writes: it does not resolve links or emulate a filesystem.
#[derive(Clone, Debug)]
pub(crate) struct Memory {
    fs: Arc<dyn togi_journal::fs::Fs>,
    tree: Arc<Mutex<MemoryTree>>,
}

#[derive(Debug, Default)]
struct MemoryTree {
    #[cfg(test)]
    next_id: u64,
    files: BTreeMap<PathBuf, MemoryMetadata>,
}

#[derive(Clone, Copy, Debug)]
struct MemoryMetadata {
    id: u64,
    owner: u32,
    links: u64,
    regular: bool,
    revision: u64,
    open_error: Option<Errno>,
}

#[cfg(test)]
impl MemoryTree {
    fn written(&mut self, path: &Path, owner: Option<u32>) {
        if let Some(file) = self.files.get_mut(path) {
            file.revision += 1;
            if let Some(owner) = owner {
                file.owner = owner;
            }
        } else {
            let file = self.created(owner, true);
            self.files.insert(path.to_owned(), file);
        }
    }

    fn created(&mut self, owner: Option<u32>, regular: bool) -> MemoryMetadata {
        self.next_id += 1;
        MemoryMetadata {
            id: self.next_id,
            owner: owner.unwrap_or(0),
            links: 1,
            regular,
            revision: 0,
            open_error: None,
        }
    }

    fn unlinked(&mut self, path: &Path) {
        if let Some(removed) = self.files.remove(path) {
            for file in self.files.values_mut().filter(|file| file.id == removed.id) {
                file.links -= 1;
            }
        }
    }
}

impl Memory {
    #[cfg(test)]
    pub(crate) fn new(fs: Arc<dyn togi_journal::fs::Fs>) -> Self {
        Self {
            fs,
            tree: Arc::new(Mutex::new(MemoryTree::default())),
        }
    }

    fn tree(&self) -> MutexGuard<'_, MemoryTree> {
        self.tree.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn watched(&self, path: impl AsRef<Path>, name: &FileName, owner: u32) -> Watched {
        Watched::from_source(Source::Memory {
            memory: self.clone(),
            path: path.as_ref().join(name.as_str()),
            owner,
        })
    }

    /// Truncation preserves identity; removing and creating the path does not.
    #[cfg(test)]
    pub(crate) fn write_file(
        &self,
        path: impl AsRef<Path>,
        bytes: impl AsRef<[u8]>,
        owner: Option<u32>,
    ) -> io::Result<()> {
        let path = path.as_ref();
        let mut tree = self.tree();
        self.fs.create_truncate(path)?.write_all(bytes.as_ref())?;
        tree.written(path, owner);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn append_file(
        &self,
        path: impl AsRef<Path>,
        bytes: impl AsRef<[u8]>,
        owner: Option<u32>,
    ) -> io::Result<()> {
        let path = path.as_ref();
        let mut tree = self.tree();
        self.fs.open_append(path)?.write_all(bytes.as_ref())?;
        tree.written(path, owner);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn remove_file(&self, path: impl AsRef<Path>) -> io::Result<()> {
        let path = path.as_ref();
        let mut tree = self.tree();
        self.fs.remove_file(path)?;
        tree.unlinked(path);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn rename(&self, from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
        let (from, to) = (from.as_ref(), to.as_ref());
        let mut tree = self.tree();
        self.fs.rename(from, to)?;
        if from != to {
            let file = tree.files.remove(from);
            tree.unlinked(to);
            if let Some(file) = file {
                tree.files.insert(to.to_owned(), file);
            }
        }
        Ok(())
    }

    /// A second name records link refusal; no watched reader follows either name afterwards.
    #[cfg(test)]
    pub(crate) fn hard_link(&self, from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
        let (from, to) = (from.as_ref(), to.as_ref());
        let mut tree = self.tree();
        let mut linked = *tree
            .files
            .get(from)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let bytes = self.fs.read(from)?;
        self.fs.create_new(to)?.write_all(&bytes)?;
        for file in tree.files.values_mut().filter(|file| file.id == linked.id) {
            file.links += 1;
        }
        linked.links += 1;
        tree.files.insert(to.to_owned(), linked);
        Ok(())
    }

    /// An unsupported-entry marker, never a link traversal or a host filesystem operation.
    #[cfg(test)]
    pub(crate) fn symlink(
        &self,
        _target: impl AsRef<Path>,
        path: impl AsRef<Path>,
    ) -> io::Result<()> {
        self.nonregular(path)
    }

    #[cfg(test)]
    pub(crate) fn nonregular(&self, path: impl AsRef<Path>) -> io::Result<()> {
        let path = path.as_ref();
        let mut tree = self.tree();
        self.fs.create_new(path)?;
        let file = tree.created(None, false);
        tree.files.insert(path.to_owned(), file);
        Ok(())
    }

    #[cfg(test)]
    fn set_len(&self, path: impl AsRef<Path>, len: u64) -> io::Result<()> {
        let path = path.as_ref();
        let mut tree = self.tree();
        self.fs.open_append(path)?.set_len(len)?;
        tree.written(path, None);
        Ok(())
    }

    #[cfg(test)]
    fn fail_open(&self, path: impl AsRef<Path>, error: Errno) {
        self.tree().files.get_mut(path.as_ref()).unwrap().open_error = Some(error);
    }

    fn fetch(
        &self,
        path: &Path,
        owner: u32,
        resume: Option<Resume>,
        buffer: &mut Vec<u8>,
        mut read_at: impl FnMut(&MemoryRead, &mut [u8], u64) -> Result<usize, Errno>,
    ) -> Outcome {
        let tree = self.tree();
        let Some(file) = tree.files.get(path) else {
            return Outcome::Missing;
        };
        if let Some(error) = file.open_error {
            return Outcome::Failed(error);
        }
        if !file.regular {
            return Outcome::Other;
        }
        if file.owner != owner {
            return Outcome::Refused(Refusal::Owner);
        }
        if file.links != 1 {
            return Outcome::Refused(Refusal::Links);
        }
        let bytes = match self.fs.read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Outcome::Missing,
            Err(_) => return Outcome::Failed(Errno::IO),
        };
        let id = FileId {
            dev: 0,
            ino: file.id,
        };
        let size = bytes.len() as u64;
        let reader = MemoryRead {
            memory: self.clone(),
            path: path.to_owned(),
            id: file.id,
            snapshot: Mutex::new((file.revision, bytes)),
        };
        drop(tree);
        read_snapshot(id, size, resume, buffer, |buffer, offset| {
            read_at(&reader, buffer, offset)
        })
    }
}

/// An opened memory snapshot keeps its old inode after replacement, like an open descriptor.
/// In-place writes refresh the bytes, allowing a truncation between snapshot and read.
struct MemoryRead {
    memory: Memory,
    path: PathBuf,
    id: u64,
    snapshot: Mutex<(u64, Vec<u8>)>,
}

impl MemoryRead {
    fn read_at(&self, buffer: &mut [u8], offset: u64) -> Result<usize, Errno> {
        let tree = self.memory.tree();
        let mut snapshot = self.snapshot.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(file) = tree.files.get(&self.path)
            && file.id == self.id
            && file.revision != snapshot.0
        {
            snapshot.1 = self.memory.fs.read(&self.path).map_err(|_| Errno::IO)?;
            snapshot.0 = file.revision;
        }
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        let bytes = snapshot.1.get(start..).unwrap_or_default();
        let count = bytes.len().min(buffer.len());
        buffer[..count].copy_from_slice(&bytes[..count]);
        Ok(count)
    }
}

/// What waiting for the blocking owner came to.
enum Waited {
    Done(Fetched),
    /// Still running at the wait bound; it stays in flight.
    Late,
    Gone,
}

/// One regular file the backend writes in its instance directory, tailed by a single-flight
/// blocking owner: the runtime thread frames and classifies bytes but never opens, stats or
/// reads the file, and awaits the owner only up to an absolute deadline.
pub(crate) struct Watched {
    source: Source,
    state: State,
    framer: Framer,
    buffer: Vec<u8>,
    inflight: Option<JoinHandle<Fetched>>,
    final_read_pending: bool,
    #[cfg(test)]
    read_at: Option<ReadAt>,
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
    /// `owner` is the uid the workload runs as: a file of any other owner is refused.
    pub(crate) fn new(dir: Arc<OwnedFd>, name: FileName, owner: u32) -> Self {
        Self::from_source(Source::Disk { dir, name, owner })
    }

    fn from_source(source: Source) -> Self {
        Self {
            source,
            state: State::Unopened,
            framer: Framer::default(),
            buffer: Vec::new(),
            inflight: None,
            final_read_pending: false,
            #[cfg(test)]
            read_at: None,
        }
    }

    /// Whether bytes already seen are still unread, or a line is half read, so the file's
    /// disappearance would lose evidence.
    pub(crate) const fn backlog(&self) -> bool {
        match self.state {
            State::Tailing { offset, size, .. } => size > offset || self.framer.pending(),
            State::Unopened | State::Stopped => false,
        }
    }

    /// Nothing more will be read now: fully read, never created, or rejected. A read still in
    /// flight, or a final read that ended unfinished, is not drained, whatever the last poll saw.
    pub(crate) const fn drained(&self) -> bool {
        if self.final_read_pending || self.inflight.is_some() {
            return false;
        }
        match self.state {
            State::Tailing { drained, .. } => drained,
            State::Unopened | State::Stopped => true,
        }
    }

    /// The first 64 KiB of an oversized line, kept for diagnosis and never classified.
    #[cfg(test)]
    pub(crate) fn oversized(&self) -> Option<&[u8]> {
        self.framer.oversized()
    }

    fn stop(&mut self, error: WatchError) -> Poll {
        self.state = State::Stopped;
        Poll::Failed(error)
    }

    /// Ends the file: lost when accepted bytes were unread, otherwise the plain `error`.
    fn reject(&mut self, error: WatchError) -> Poll {
        let error = if self.backlog() {
            WatchError::Lost
        } else {
            error
        };
        self.stop(error)
    }

    fn resume(&self) -> Option<Resume> {
        match self.state {
            State::Tailing {
                id, offset, size, ..
            } => Some(Resume {
                id,
                offset,
                size,
                backlog: self.backlog(),
            }),
            State::Unopened | State::Stopped => None,
        }
    }

    /// Waits for the blocking owner, starting it unless one is already in flight, until `until`.
    /// An owner not done by then stays in flight: it is never replaced while stuck.
    async fn wait(&mut self, until: Instant) -> Waited {
        let mut task = if let Some(task) = self.inflight.take() {
            task
        } else {
            let request = Request {
                source: self.source.clone(),
                resume: self.resume(),
                buffer: std::mem::take(&mut self.buffer),
                #[cfg(test)]
                read_at: self.read_at.clone(),
            };
            tokio::task::spawn_blocking(move || fetch(request))
        };
        match timeout_at(until, &mut task).await {
            Ok(Ok(fetched)) => Waited::Done(fetched),
            Ok(Err(_)) => Waited::Gone,
            Err(_) => {
                self.inflight = Some(task);
                Waited::Late
            }
        }
    }

    /// Reads at most 64 KiB from the saved offset and frames it in 4 KiB chunks. `sink` breaks
    /// to stop after a line; `stop` is asked before each chunk and after each line, and a
    /// stop leaves the offset exactly after the last consumed byte, so the next poll resumes
    /// without loss or duplication. The wait for the blocking read ends at `until`, leaving the
    /// read in flight and the poll pending.
    pub(crate) async fn poll(
        &mut self,
        until: Instant,
        sink: &mut impl FnMut(&str) -> ControlFlow<()>,
        stop: &mut impl FnMut() -> bool,
    ) -> Poll {
        if matches!(self.state, State::Stopped) {
            return Poll::Drained;
        }
        if stop() {
            return Poll::Pending;
        }
        let fetched = match self.wait(until).await {
            Waited::Done(fetched) => fetched,
            Waited::Late => return Poll::Pending,
            Waited::Gone => {
                return self.reject(WatchError::Read(io::Error::other(
                    "the watched output reader ended without a result",
                )));
            }
        };
        let Fetched { outcome, buffer } = fetched;
        match outcome {
            Outcome::Missing if self.backlog() => self.reject(WatchError::Lost),
            Outcome::Missing => {
                self.buffer = buffer;
                Poll::Idle
            }
            Outcome::Other => self.reject(WatchError::Unsupported),
            Outcome::Refused(refusal) => self.reject(WatchError::Refused(refusal)),
            Outcome::Replaced => self.reject(WatchError::Lost),
            Outcome::Failed(errno) => self.reject(WatchError::Read(errno.into())),
            Outcome::Read {
                id,
                size,
                start,
                got,
                error,
            } => {
                self.state = State::Tailing {
                    id,
                    offset: start,
                    size,
                    drained: false,
                };
                let poll = self.consume(&buffer[..got], start, size, error, sink, stop);
                self.buffer = buffer;
                poll
            }
        }
    }

    /// Frames `bytes` read from `start`, the state already holding the accepted `size`, so any
    /// failure from here on sees the whole backlog.
    fn consume(
        &mut self,
        bytes: &[u8],
        start: u64,
        size: u64,
        error: Option<Errno>,
        sink: &mut impl FnMut(&str) -> ControlFlow<()>,
        stop: &mut impl FnMut() -> bool,
    ) -> Poll {
        let mut offset = start;
        let mut halted = false;
        let mut exceeded = false;
        for chunk in bytes.chunks(CHUNK) {
            if stop() {
                halted = true;
                break;
            }
            let consumed = self.framer.push(chunk, &mut |frame| match frame {
                Frame::Line(line) => {
                    if sink(&line).is_break() || stop() {
                        halted = true;
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                }
                Frame::Exceeded => {
                    exceeded = true;
                    ControlFlow::Break(())
                }
            });
            offset += consumed as u64;
            if exceeded {
                return self.stop(WatchError::LineTooLong);
            }
            if halted {
                break;
            }
        }
        let drained = !halted && error.is_none() && offset >= size;
        if let State::Tailing {
            offset: kept,
            drained: done,
            ..
        } = &mut self.state
        {
            *kept = offset;
            *done = drained;
        }
        if let Some(errno) = error {
            return self.reject(WatchError::Read(errno.into()));
        }
        if drained {
            Poll::Drained
        } else {
            Poll::Pending
        }
    }

    /// The final read, once the writers are confirmed stopped: polls to the end of the file,
    /// whatever an earlier poll saw, then classifies the unterminated last line exactly once.
    /// Anything but `Drained` or `Idle` leaves the file undrained or failed.
    pub(crate) async fn finish(
        &mut self,
        _writers: WritersStopped,
        until: Instant,
        sink: &mut impl FnMut(&str) -> ControlFlow<()>,
    ) -> Poll {
        let mut late = || Instant::now() >= until;
        let mut every = |line: &str| {
            let _ = sink(line);
            ControlFlow::Continue(())
        };
        let poll = loop {
            match self.poll(until, &mut every, &mut late).await {
                Poll::Pending if Instant::now() < until => {}
                other @ (Poll::Idle | Poll::Drained | Poll::Pending | Poll::Failed(_)) => {
                    break other;
                }
            }
        };
        self.final_read_pending = matches!(poll, Poll::Pending);
        if matches!(poll, Poll::Drained)
            && self.drained()
            && let Some(line) = self.framer.finish()
        {
            let _ = sink(&line);
        }
        poll
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::LINE_LIMIT;
    use std::cell::Cell;
    use std::fs::{self, OpenOptions};
    use std::io::Write as _;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    struct Fixture {
        dir: Option<tempfile::TempDir>,
        memory: Option<Memory>,
        watched: Watched,
    }

    fn owner() -> u32 {
        rustix::process::geteuid().as_raw()
    }

    fn fixture() -> Fixture {
        let fs = Arc::new(togi_journal::fs::MemFs::new());
        togi_journal::fs::Fs::create_dir(fs.as_ref(), Path::new("/watch")).unwrap();
        let memory = Memory::new(fs);
        let watched = memory.watched("/watch", &FileName::from_static("results.txt"), 0);
        Fixture {
            dir: None,
            memory: Some(memory),
            watched,
        }
    }

    fn real_fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let fd = rustix::fs::open(
            dir.path(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        let watched = Watched::new(Arc::new(fd), FileName::from_static("results.txt"), owner());
        Fixture {
            dir: Some(dir),
            memory: None,
            watched,
        }
    }

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(3600)
    }

    fn soon() -> Instant {
        Instant::now() + Duration::from_millis(100)
    }

    impl Fixture {
        fn directory(&self) -> &Path {
            self.dir
                .as_ref()
                .map_or(Path::new("/watch"), |dir| dir.path())
        }

        fn path(&self) -> PathBuf {
            self.directory().join("results.txt")
        }

        fn memory(&self) -> &Memory {
            self.memory.as_ref().expect("memory watched fixture")
        }

        fn append(&self, bytes: &[u8]) {
            if let Some(memory) = &self.memory {
                memory.append_file(self.path(), bytes, None).unwrap();
                return;
            }
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.path())
                .unwrap()
                .write_all(bytes)
                .unwrap();
        }

        fn read_with(
            &mut self,
            read: impl Fn(&MemoryRead, &mut [u8], u64) -> Result<usize, Errno> + Send + Sync + 'static,
        ) {
            self.watched.read_at = Some(Arc::new(read));
        }

        /// Makes every read wait at the returned gate until the test passes it too.
        fn stall_reads(&mut self) -> (Arc<Barrier>, Arc<AtomicUsize>) {
            let gate = Arc::new(Barrier::new(2));
            let calls = Arc::new(AtomicUsize::new(0));
            let (held, seen) = (Arc::clone(&gate), Arc::clone(&calls));
            self.read_with(move |file, buffer, offset| {
                seen.fetch_add(1, Ordering::SeqCst);
                held.wait();
                file.read_at(buffer, offset)
            });
            (gate, calls)
        }

        async fn poll(&mut self) -> (Poll, Vec<String>) {
            self.poll_by(far()).await
        }

        async fn poll_by(&mut self, until: Instant) -> (Poll, Vec<String>) {
            let mut lines = Vec::new();
            let poll = self
                .watched
                .poll(
                    until,
                    &mut |line| {
                        lines.push(line.to_owned());
                        ControlFlow::Continue(())
                    },
                    &mut || false,
                )
                .await;
            (poll, lines)
        }

        async fn finish(&mut self, until: Instant) -> (Poll, Vec<String>) {
            let mut lines = Vec::new();
            let poll = self
                .watched
                .finish(WritersStopped::confirmed(), until, &mut |line| {
                    lines.push(line.to_owned());
                    ControlFlow::Continue(())
                })
                .await;
            (poll, lines)
        }
    }

    #[test]
    fn a_missing_file_is_idle_and_harmless() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            let (poll, lines) = f.poll().await;
            assert!(matches!(poll, Poll::Idle) && lines.is_empty());
            assert!(f.watched.drained() && !f.watched.backlog());
        });
    }

    #[test]
    fn appended_lines_arrive_once_in_order() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            f.append(b"one\ntwo\n");
            let (poll, lines) = f.poll().await;
            assert!(matches!(poll, Poll::Drained));
            assert_eq!(lines, ["one", "two"]);
            f.append(b"three\n");
            assert_eq!(f.poll().await.1, ["three"]);
            assert!(f.poll().await.1.is_empty());
        });
    }

    #[test]
    fn a_line_split_across_polls_is_one_line_and_the_tail_waits_for_writers() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            f.append(b"COMPUTE ");
            assert!(f.poll().await.1.is_empty());
            assert!(f.watched.backlog());
            f.append(b"ERROR\nFATAL: tail");
            assert_eq!(f.poll().await.1, ["COMPUTE ERROR"]);
            let (poll, lines) = f.finish(far()).await;
            assert!(matches!(poll, Poll::Drained));
            assert_eq!(lines, ["FATAL: tail"]);
            assert!(f.finish(far()).await.1.is_empty());
        });
    }

    #[test]
    fn the_final_read_sees_what_a_late_writer_appended_after_the_last_poll() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            f.append(b"COMPUTE");
            assert!(matches!(f.poll().await.0, Poll::Drained));
            f.append(b" ERROR\nFATAL: tail");
            let (poll, lines) = f.finish(far()).await;
            assert!(matches!(poll, Poll::Drained));
            assert_eq!(lines, ["COMPUTE ERROR", "FATAL: tail"]);
            assert!(f.watched.drained());
        });
    }

    #[test]
    fn an_expired_final_read_invalidates_the_earlier_drained_snapshot() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            f.append(b"old\nCOMPUTE");
            let (poll, lines) = f.poll().await;
            assert!(matches!(poll, Poll::Drained));
            assert_eq!(lines, ["old"]);
            assert!(f.watched.drained());
            f.append(b" ERROR\nFATAL: tail");

            let (poll, lines) = f.finish(Instant::now()).await;
            assert!(matches!(poll, Poll::Pending) && lines.is_empty());
            assert!(!f.watched.drained(), "the final snapshot was not read");

            let (poll, lines) = f.finish(far()).await;
            assert!(matches!(poll, Poll::Drained));
            assert_eq!(lines, ["COMPUTE ERROR", "FATAL: tail"]);
            assert!(f.watched.drained());
            assert!(f.finish(far()).await.1.is_empty());
        });
    }

    #[test]
    fn an_expired_final_read_cannot_confirm_a_previously_missing_file() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            assert!(matches!(f.poll().await.0, Poll::Idle));
            f.append(b"COMPUTE ERROR");
            let (poll, lines) = f.finish(Instant::now()).await;
            assert!(matches!(poll, Poll::Pending) && lines.is_empty());
            assert!(!f.watched.drained(), "the final snapshot was not read");
        });
    }

    #[test]
    fn a_final_read_that_cannot_finish_leaves_the_file_undrained() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            f.append(b"COMPUTE");
            assert!(matches!(f.poll().await.0, Poll::Drained));
            f.append(b" ERROR\n");
            let (gate, _) = f.stall_reads();
            let (poll, lines) = f.finish(soon()).await;
            assert!(matches!(poll, Poll::Pending) && lines.is_empty());
            assert!(!f.watched.drained());
            gate.wait();
        });
    }

    #[test]
    fn a_large_append_is_read_in_bounded_polls() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            let line = b"ok ok ok ok ok ok ok ok\n";
            let total = 4 * 1024 * 1024 / line.len();
            f.append(&line.repeat(total));
            let mut polls = 0;
            let mut seen = 0;
            loop {
                let (poll, lines) = f.poll().await;
                polls += 1;
                seen += lines.len();
                assert!(lines.len() * line.len() <= POLL_LIMIT + line.len());
                if matches!(poll, Poll::Drained) {
                    break;
                }
            }
            assert_eq!(seen, total);
            assert!(polls >= 4 * 1024 * 1024 / POLL_LIMIT);
        });
    }

    #[test]
    fn stopping_between_lines_resumes_at_the_exact_offset() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            f.append(b"a\nb\nc\nd\n");
            let mut taken = Vec::new();
            let poll = f
                .watched
                .poll(
                    far(),
                    &mut |line| {
                        taken.push(line.to_owned());
                        if taken.len() == 2 {
                            ControlFlow::Break(())
                        } else {
                            ControlFlow::Continue(())
                        }
                    },
                    &mut || false,
                )
                .await;
            assert!(matches!(poll, Poll::Pending));
            assert_eq!(taken, ["a", "b"]);
            assert!(!f.watched.drained() && f.watched.backlog());
            assert_eq!(f.poll().await.1, ["c", "d"]);
        });
    }

    #[test]
    fn the_stop_condition_is_asked_before_each_chunk() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            f.append(&vec![b'y'; 10_000]);
            let mut asked = 0;
            let poll = f
                .watched
                .poll(far(), &mut |_| ControlFlow::Continue(()), &mut || {
                    asked += 1;
                    asked > 2
                })
                .await;
            assert!(matches!(poll, Poll::Pending));
            assert_eq!(asked, 3);
            assert!(f.watched.backlog());
        });
    }

    #[test]
    fn a_stop_requested_while_a_line_is_classified_leaves_the_rest_of_the_chunk_unread() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            f.append(b"a\nb\nc\n");
            let stopping = Cell::new(false);
            let mut taken = Vec::new();
            let poll = f
                .watched
                .poll(
                    far(),
                    &mut |line| {
                        taken.push(line.to_owned());
                        stopping.set(true);
                        ControlFlow::Continue(())
                    },
                    &mut || stopping.get(),
                )
                .await;
            assert!(matches!(poll, Poll::Pending));
            assert_eq!(taken, ["a"]);
            assert_eq!(f.poll().await.1, ["b", "c"]);
        });
    }

    #[test]
    fn a_sink_break_on_the_last_byte_of_a_chunk_still_ends_the_poll() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            let mut first = vec![b'a'; CHUNK - 1];
            first.push(b'\n');
            f.append(&first);
            f.append(b"next\n");
            let mut taken = Vec::new();
            let poll = f
                .watched
                .poll(
                    far(),
                    &mut |line| {
                        taken.push(line.len());
                        ControlFlow::Break(())
                    },
                    &mut || false,
                )
                .await;
            assert!(matches!(poll, Poll::Pending));
            assert_eq!(taken, [CHUNK - 1]);
            assert!(!f.watched.drained());
            assert_eq!(f.poll().await.1, ["next"]);
        });
    }

    #[test]
    fn real_fs_symlinks_directories_and_fifos_are_rejected_without_reading() {
        crate::testing::runtime(false).block_on(async {
            for make in [
                |p: &std::path::Path, target: &std::path::Path| {
                    fs::write(target, "secret\n").unwrap();
                    symlink(target, p).unwrap();
                },
                |p: &std::path::Path, _: &std::path::Path| fs::create_dir(p).unwrap(),
                |p: &std::path::Path, _: &std::path::Path| {
                    rustix::fs::mknodat(
                        rustix::fs::CWD,
                        p,
                        FileType::Fifo,
                        Mode::from_raw_mode(0o600),
                        0,
                    )
                    .unwrap();
                },
            ] {
                let mut f = real_fixture();
                let target = f.directory().join("target");
                make(&f.path(), &target);
                let (poll, lines) = f.poll().await;
                assert!(
                    matches!(poll, Poll::Failed(WatchError::Unsupported)),
                    "{poll:?}"
                );
                assert!(lines.is_empty());
                assert!(!f.watched.backlog());
                assert!(matches!(f.poll().await.0, Poll::Drained));
            }
        });
    }

    #[test]
    fn real_fs_a_hard_linked_file_is_refused_without_reading() {
        crate::testing::runtime(false).block_on(async {
            let mut f = real_fixture();
            let elsewhere = f.directory().join("elsewhere");
            fs::write(&elsewhere, "COMPUTE ERROR\n").unwrap();
            fs::hard_link(&elsewhere, f.path()).unwrap();
            let (poll, lines) = f.poll().await;
            assert!(
                matches!(poll, Poll::Failed(WatchError::Refused(Refusal::Links))),
                "{poll:?}"
            );
            assert!(lines.is_empty());
            assert!(!f.watched.backlog());
            let (poll, lines) = f.finish(Instant::now()).await;
            assert!(matches!(poll, Poll::Drained) && lines.is_empty());
            assert!(f.watched.drained());
        });
    }

    #[test]
    fn real_fs_a_file_of_another_account_is_refused_without_reading() {
        crate::testing::runtime(false).block_on(async {
            let mut f = real_fixture();
            let Source::Disk {
                owner: expected, ..
            } = &mut f.watched.source
            else {
                unreachable!("real watched fixture");
            };
            *expected = owner().wrapping_add(1);
            f.append(b"COMPUTE ERROR\n");
            let (poll, lines) = f.poll().await;
            assert!(
                matches!(poll, Poll::Failed(WatchError::Refused(Refusal::Owner))),
                "{poll:?}"
            );
            assert!(lines.is_empty());
        });
    }

    #[test]
    fn real_fs_a_hard_link_added_to_a_file_with_unread_backlog_loses_the_backlog() {
        crate::testing::runtime(false).block_on(async {
            let mut f = real_fixture();
            seed_backlog(&mut f).await;
            fs::hard_link(f.path(), f.directory().join("alias")).unwrap();
            assert!(matches!(f.poll().await.0, Poll::Failed(WatchError::Lost)));
        });
    }

    async fn seed(f: &mut Fixture) {
        f.append(&b"x\n".repeat(60_000));
        let poll = f
            .watched
            .poll(far(), &mut |_| ControlFlow::Continue(()), &mut || false)
            .await;
        assert!(matches!(poll, Poll::Pending));
        assert!(f.watched.backlog());
    }

    #[test]
    fn removal_truncation_and_replacement_of_unread_bytes_are_lost() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            seed(&mut f).await;
            f.memory().remove_file(f.path()).unwrap();
            assert!(matches!(f.poll().await.0, Poll::Failed(WatchError::Lost)));

            let mut f = fixture();
            seed(&mut f).await;
            f.memory().write_file(f.path(), b"short\n", None).unwrap();
            assert!(matches!(f.poll().await.0, Poll::Failed(WatchError::Lost)));

            let mut f = fixture();
            seed(&mut f).await;
            let replacement = f.directory().join("other");
            f.memory()
                .write_file(&replacement, b"y\n".repeat(60_000), None)
                .unwrap();
            f.memory().rename(&replacement, f.path()).unwrap();
            assert!(matches!(f.poll().await.0, Poll::Failed(WatchError::Lost)));

            let mut f = fixture();
            seed(&mut f).await;
            f.memory().remove_file(f.path()).unwrap();
            f.memory().symlink("/dev/null", f.path()).unwrap();
            assert!(matches!(f.poll().await.0, Poll::Failed(WatchError::Lost)));
        });
    }

    async fn seed_backlog(f: &mut Fixture) {
        f.append(&b"ok line\n".repeat(128 * 1024 / 8));
        f.append(b"COMPUTE ERROR\n");
        assert!(matches!(f.poll().await.0, Poll::Pending));
        assert!(f.watched.backlog());
    }

    #[test]
    fn truncating_the_same_inode_below_the_accepted_size_loses_the_backlog() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            seed_backlog(&mut f).await;
            f.memory().set_len(f.path(), POLL_LIMIT as u64).unwrap();
            assert!(matches!(f.poll().await.0, Poll::Failed(WatchError::Lost)));
        });
    }

    #[test]
    fn an_open_error_with_an_unread_backlog_is_lost_output() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            seed_backlog(&mut f).await;
            f.memory().fail_open(f.path(), Errno::ACCESS);
            assert!(matches!(f.poll().await.0, Poll::Failed(WatchError::Lost)));
        });
    }

    #[test]
    fn an_open_error_without_a_backlog_is_an_ordinary_read_error() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            f.append(b"fine\n");
            assert!(matches!(f.poll().await.0, Poll::Drained));
            f.memory().fail_open(f.path(), Errno::ACCESS);
            assert!(matches!(
                f.poll().await.0,
                Poll::Failed(WatchError::Read(_))
            ));
        });
    }

    #[test]
    fn a_read_failing_on_a_backlog_accepted_by_the_same_poll_is_lost_output() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            f.append(&b"ok line\n".repeat(1000));
            f.read_with(|_, _, _| Err(Errno::IO));
            assert!(matches!(f.poll().await.0, Poll::Failed(WatchError::Lost)));
        });
    }

    #[test]
    fn a_read_failing_after_a_chunk_of_a_new_append_is_lost_output() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            f.append(b"first\n");
            assert!(matches!(f.poll().await.0, Poll::Drained));
            f.append(&b"ok line\n".repeat(2000));
            let calls = AtomicUsize::new(0);
            f.read_with(move |file, buffer, offset| {
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    let limit = buffer.len().min(CHUNK);
                    file.read_at(&mut buffer[..limit], offset)
                } else {
                    Err(Errno::IO)
                }
            });
            let (poll, lines) = f.poll().await;
            assert!(matches!(poll, Poll::Failed(WatchError::Lost)), "{poll:?}");
            assert_eq!(lines.len(), CHUNK / 8);
        });
    }

    #[test]
    fn end_of_file_before_the_accepted_end_keeps_the_backlog_pending_and_never_drains() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            f.append(&b"ok line\n".repeat(1000));
            let path = f.path();
            let memory = f.memory().clone();
            let calls = AtomicUsize::new(0);
            f.read_with(move |file, buffer, offset| {
                let limit = buffer.len().min(CHUNK);
                let count = file.read_at(&mut buffer[..limit], offset)?;
                if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    memory.set_len(&path, 0).unwrap();
                }
                Ok(count)
            });
            let (poll, lines) = f.poll().await;
            assert!(matches!(poll, Poll::Pending), "{poll:?}");
            assert_eq!(lines.len(), CHUNK / 8);
            assert!(f.watched.backlog() && !f.watched.drained());
            let (poll, lines) = f.finish(far()).await;
            assert!(
                matches!(poll, Poll::Failed(WatchError::Lost)),
                "the shrunk file's unread backlog is lost, never a drained file: {poll:?}"
            );
            assert!(lines.is_empty());
        });
    }

    #[test]
    fn a_stuck_read_never_blocks_the_runtime_and_is_never_replaced() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            f.append(b"one\n");
            assert_eq!(f.poll().await.1, ["one"]);
            f.append(b"two\n");
            let (gate, calls) = f.stall_reads();
            let (poll, lines) = f.poll_by(soon()).await;
            assert!(matches!(poll, Poll::Pending) && lines.is_empty());
            assert!(
                !f.watched.drained(),
                "a read in flight is not a drained file"
            );
            let (poll, _) = f.poll_by(soon()).await;
            assert!(matches!(poll, Poll::Pending));
            assert_eq!(calls.load(Ordering::SeqCst), 1, "single flight");
            gate.wait();
            let (poll, lines) = f.poll().await;
            assert!(matches!(poll, Poll::Drained));
            assert_eq!(lines, ["two"]);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert!(f.watched.drained());
        });
    }

    #[test]
    fn a_same_size_replacement_is_detected_by_inode() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            f.append(b"aaaa\nbbbb\n");
            let mut first = true;
            let poll = f
                .watched
                .poll(
                    far(),
                    &mut |_| {
                        if std::mem::take(&mut first) {
                            ControlFlow::Break(())
                        } else {
                            ControlFlow::Continue(())
                        }
                    },
                    &mut || false,
                )
                .await;
            assert!(matches!(poll, Poll::Pending));
            let replacement = f.directory().join("other");
            f.memory()
                .write_file(&replacement, b"cccc\ndddd\n", None)
                .unwrap();
            f.memory().rename(&replacement, f.path()).unwrap();
            assert!(matches!(f.poll().await.0, Poll::Failed(WatchError::Lost)));
        });
    }

    #[test]
    fn replacing_a_fully_drained_file_starts_the_new_one_from_the_top() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            f.append(b"old\n");
            assert_eq!(f.poll().await.1, ["old"]);
            let replacement = f.directory().join("other");
            f.memory()
                .write_file(&replacement, b"new\nmore\n", None)
                .unwrap();
            f.memory().rename(&replacement, f.path()).unwrap();
            assert_eq!(f.poll().await.1, ["new", "more"]);
            f.memory().write_file(f.path(), b"fresh\n", None).unwrap();
            assert_eq!(f.poll().await.1, ["fresh"]);
        });
    }

    #[test]
    fn an_oversized_line_stops_the_file_for_good_and_keeps_its_prefix_unclassified() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            f.append(&vec![b'x'; 1024 * 1024]);
            let mut total = 0;
            loop {
                let (poll, lines) = f.poll().await;
                assert!(lines.is_empty());
                total += 1;
                match poll {
                    Poll::Failed(WatchError::LineTooLong) => break,
                    Poll::Pending => assert!(total < 100),
                    other @ (Poll::Idle | Poll::Drained | Poll::Failed(_)) => panic!("{other:?}"),
                }
            }
            assert_eq!(f.watched.oversized(), Some(&vec![b'x'; LINE_LIMIT][..]));
            assert!(!f.watched.backlog() && f.watched.drained());
            assert!(matches!(f.poll().await.0, Poll::Drained));
            let (poll, lines) = f.finish(Instant::now()).await;
            assert!(matches!(poll, Poll::Drained) && f.watched.drained());
            assert!(lines.is_empty());
            assert_eq!(f.watched.oversized().map(<[u8]>::len), Some(LINE_LIMIT));
        });
    }

    #[test]
    fn a_final_line_needs_a_drained_file() {
        crate::testing::runtime(false).block_on(async {
            let mut f = fixture();
            f.append(&b"x\n".repeat(60_000));
            f.append(b"tail");
            let mut seen = 0;
            let poll = f
                .watched
                .poll(
                    far(),
                    &mut |_| {
                        seen += 1;
                        ControlFlow::Continue(())
                    },
                    &mut || false,
                )
                .await;
            assert!(matches!(poll, Poll::Pending));
            let (poll, lines) = f.finish(Instant::now()).await;
            assert!(matches!(poll, Poll::Pending) && lines.is_empty());
            let (poll, lines) = f.finish(far()).await;
            assert!(matches!(poll, Poll::Drained));
            assert_eq!(lines.len(), 60_000 - seen + 1);
            assert_eq!(lines.last().map(String::as_str), Some("tail"));
        });
    }

    #[test]
    fn real_fs_a_fifo_replacing_a_file_with_unread_backlog_loses_the_backlog() {
        crate::testing::runtime(false).block_on(async {
            let mut f = real_fixture();
            seed(&mut f).await;
            fs::remove_file(f.path()).unwrap();
            rustix::fs::mknodat(
                rustix::fs::CWD,
                f.path(),
                FileType::Fifo,
                Mode::from_raw_mode(0o600),
                0,
            )
            .unwrap();
            assert!(matches!(f.poll().await.0, Poll::Failed(WatchError::Lost)));
        });
    }
}
