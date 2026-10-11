use std::io;
use std::ops::ControlFlow;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;

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

/// Reads at most `buffer.len()` bytes at an offset of a descriptor; replaceable so tests can
/// fail, shrink or stall the read.
type ReadFn = dyn Fn(&OwnedFd, &mut [u8], u64) -> Result<usize, Errno> + Send + Sync;
type ReadAt = Arc<ReadFn>;

/// What a tailed file's blocking owner needs to resume.
#[derive(Clone, Copy)]
struct Resume {
    id: FileId,
    offset: u64,
    size: u64,
    backlog: bool,
}

struct Request {
    dir: Arc<OwnedFd>,
    name: FileName,
    owner: u32,
    resume: Option<Resume>,
    buffer: Vec<u8>,
    read_at: ReadAt,
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
    let Request {
        dir,
        name,
        owner,
        resume,
        mut buffer,
        read_at,
    } = request;
    let outcome = open_and_read(&dir, &name, owner, resume, &mut buffer, &*read_at);
    Fetched { outcome, buffer }
}

fn open_and_read(
    dir: &OwnedFd,
    name: &FileName,
    owner: u32,
    resume: Option<Resume>,
    buffer: &mut Vec<u8>,
    read_at: &ReadFn,
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
        match read_at(&fd, &mut buffer[got..], start + got as u64) {
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
    dir: Arc<OwnedFd>,
    name: FileName,
    owner: u32,
    state: State,
    framer: Framer,
    buffer: Vec<u8>,
    inflight: Option<JoinHandle<Fetched>>,
    read_at: ReadAt,
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
        Self {
            dir,
            name,
            owner,
            state: State::Unopened,
            framer: Framer::default(),
            buffer: Vec::new(),
            inflight: None,
            read_at: Arc::new(|fd, buffer, offset| pread(fd, buffer, offset)),
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
    /// flight is not drained, whatever the last poll saw.
    pub(crate) const fn drained(&self) -> bool {
        if self.inflight.is_some() {
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
        let mut task = match self.inflight.take() {
            Some(task) => task,
            None => {
                let request = Request {
                    dir: Arc::clone(&self.dir),
                    name: self.name.clone(),
                    owner: self.owner,
                    resume: self.resume(),
                    buffer: std::mem::take(&mut self.buffer),
                    read_at: Arc::clone(&self.read_at),
                };
                tokio::task::spawn_blocking(move || fetch(request))
            }
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
                other => break other,
            }
        };
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
        dir: tempfile::TempDir,
        watched: Watched,
    }

    fn owner() -> u32 {
        rustix::process::geteuid().as_raw()
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let fd = rustix::fs::open(
            dir.path(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        let watched = Watched::new(Arc::new(fd), FileName::from_static("results.txt"), owner());
        Fixture { dir, watched }
    }

    fn far() -> Instant {
        Instant::now() + Duration::from_secs(3600)
    }

    fn soon() -> Instant {
        Instant::now() + Duration::from_millis(100)
    }

    impl Fixture {
        fn path(&self) -> PathBuf {
            self.dir.path().join("results.txt")
        }

        fn append(&self, bytes: &[u8]) {
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
            read: impl Fn(&OwnedFd, &mut [u8], u64) -> Result<usize, Errno> + Send + Sync + 'static,
        ) {
            self.watched.read_at = Arc::new(read);
        }

        /// Makes every read wait at the returned gate until the test passes it too.
        fn stall_reads(&mut self) -> (Arc<Barrier>, Arc<AtomicUsize>) {
            let gate = Arc::new(Barrier::new(2));
            let calls = Arc::new(AtomicUsize::new(0));
            let (held, seen) = (Arc::clone(&gate), Arc::clone(&calls));
            self.read_with(move |fd, buffer, offset| {
                seen.fetch_add(1, Ordering::SeqCst);
                held.wait();
                pread(fd, buffer, offset)
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

    #[tokio::test]
    async fn a_missing_file_is_idle_and_harmless() {
        let mut f = fixture();
        let (poll, lines) = f.poll().await;
        assert!(matches!(poll, Poll::Idle) && lines.is_empty());
        assert!(f.watched.drained() && !f.watched.backlog());
    }

    #[tokio::test]
    async fn appended_lines_arrive_once_in_order() {
        let mut f = fixture();
        f.append(b"one\ntwo\n");
        let (poll, lines) = f.poll().await;
        assert!(matches!(poll, Poll::Drained));
        assert_eq!(lines, ["one", "two"]);
        f.append(b"three\n");
        assert_eq!(f.poll().await.1, ["three"]);
        assert!(f.poll().await.1.is_empty());
    }

    #[tokio::test]
    async fn a_line_split_across_polls_is_one_line_and_the_tail_waits_for_writers() {
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
    }

    #[tokio::test]
    async fn the_final_read_sees_what_a_late_writer_appended_after_the_last_poll() {
        let mut f = fixture();
        f.append(b"COMPUTE");
        assert!(matches!(f.poll().await.0, Poll::Drained));
        f.append(b" ERROR\nFATAL: tail");
        let (poll, lines) = f.finish(far()).await;
        assert!(matches!(poll, Poll::Drained));
        assert_eq!(lines, ["COMPUTE ERROR", "FATAL: tail"]);
        assert!(f.watched.drained());
    }

    #[tokio::test]
    async fn a_final_read_that_cannot_finish_leaves_the_file_undrained() {
        let mut f = fixture();
        f.append(b"COMPUTE");
        assert!(matches!(f.poll().await.0, Poll::Drained));
        f.append(b" ERROR\n");
        let (gate, _) = f.stall_reads();
        let (poll, lines) = f.finish(soon()).await;
        assert!(matches!(poll, Poll::Pending) && lines.is_empty());
        assert!(!f.watched.drained());
        gate.wait();
    }

    #[tokio::test]
    async fn a_large_append_is_read_in_bounded_polls() {
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
    }

    #[tokio::test]
    async fn stopping_between_lines_resumes_at_the_exact_offset() {
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
    }

    #[tokio::test]
    async fn the_stop_condition_is_asked_before_each_chunk() {
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
    }

    #[tokio::test]
    async fn a_stop_requested_while_a_line_is_classified_leaves_the_rest_of_the_chunk_unread() {
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
    }

    #[tokio::test]
    async fn a_sink_break_on_the_last_byte_of_a_chunk_still_ends_the_poll() {
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
    }

    #[tokio::test]
    async fn symlinks_directories_and_fifos_are_rejected_without_reading() {
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
            let mut f = fixture();
            let target = f.dir.path().join("target");
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
    }

    #[tokio::test]
    async fn a_hard_linked_file_is_refused_without_reading() {
        let mut f = fixture();
        let elsewhere = f.dir.path().join("elsewhere");
        fs::write(&elsewhere, "COMPUTE ERROR\n").unwrap();
        fs::hard_link(&elsewhere, f.path()).unwrap();
        let (poll, lines) = f.poll().await;
        assert!(
            matches!(poll, Poll::Failed(WatchError::Refused(Refusal::Links))),
            "{poll:?}"
        );
        assert!(lines.is_empty());
        assert!(!f.watched.backlog());
    }

    #[tokio::test]
    async fn a_file_of_another_account_is_refused_without_reading() {
        let mut f = fixture();
        f.watched.owner = owner().wrapping_add(1);
        f.append(b"COMPUTE ERROR\n");
        let (poll, lines) = f.poll().await;
        assert!(
            matches!(poll, Poll::Failed(WatchError::Refused(Refusal::Owner))),
            "{poll:?}"
        );
        assert!(lines.is_empty());
    }

    #[tokio::test]
    async fn a_hard_link_added_to_a_file_with_unread_backlog_loses_the_backlog() {
        let mut f = fixture();
        seed_backlog(&mut f).await;
        fs::hard_link(f.path(), f.dir.path().join("alias")).unwrap();
        assert!(matches!(f.poll().await.0, Poll::Failed(WatchError::Lost)));
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

    #[tokio::test]
    async fn removal_truncation_and_replacement_of_unread_bytes_are_lost() {
        let mut f = fixture();
        seed(&mut f).await;
        fs::remove_file(f.path()).unwrap();
        assert!(matches!(f.poll().await.0, Poll::Failed(WatchError::Lost)));

        let mut f = fixture();
        seed(&mut f).await;
        fs::write(f.path(), b"short\n").unwrap();
        assert!(matches!(f.poll().await.0, Poll::Failed(WatchError::Lost)));

        let mut f = fixture();
        seed(&mut f).await;
        let replacement = f.dir.path().join("other");
        fs::write(&replacement, b"y\n".repeat(60_000)).unwrap();
        fs::rename(&replacement, f.path()).unwrap();
        assert!(matches!(f.poll().await.0, Poll::Failed(WatchError::Lost)));

        let mut f = fixture();
        seed(&mut f).await;
        fs::remove_file(f.path()).unwrap();
        symlink("/dev/null", f.path()).unwrap();
        assert!(matches!(f.poll().await.0, Poll::Failed(WatchError::Lost)));
    }

    async fn seed_backlog(f: &mut Fixture) {
        f.append(&b"ok line\n".repeat(128 * 1024 / 8));
        f.append(b"COMPUTE ERROR\n");
        assert!(matches!(f.poll().await.0, Poll::Pending));
        assert!(f.watched.backlog());
    }

    #[tokio::test]
    async fn truncating_the_same_inode_below_the_accepted_size_loses_the_backlog() {
        let mut f = fixture();
        seed_backlog(&mut f).await;
        OpenOptions::new()
            .write(true)
            .open(f.path())
            .unwrap()
            .set_len(POLL_LIMIT as u64)
            .unwrap();
        assert!(matches!(f.poll().await.0, Poll::Failed(WatchError::Lost)));
    }

    #[tokio::test]
    async fn an_open_error_with_an_unread_backlog_is_lost_output() {
        if rustix::process::geteuid().is_root() {
            return;
        }
        let mut f = fixture();
        seed_backlog(&mut f).await;
        fs::set_permissions(
            f.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o000),
        )
        .unwrap();
        assert!(matches!(f.poll().await.0, Poll::Failed(WatchError::Lost)));
    }

    #[tokio::test]
    async fn an_open_error_without_a_backlog_is_an_ordinary_read_error() {
        if rustix::process::geteuid().is_root() {
            return;
        }
        let mut f = fixture();
        f.append(b"fine\n");
        assert!(matches!(f.poll().await.0, Poll::Drained));
        fs::set_permissions(
            f.path(),
            std::os::unix::fs::PermissionsExt::from_mode(0o000),
        )
        .unwrap();
        assert!(matches!(
            f.poll().await.0,
            Poll::Failed(WatchError::Read(_))
        ));
    }

    #[tokio::test]
    async fn a_read_failing_on_a_backlog_accepted_by_the_same_poll_is_lost_output() {
        let mut f = fixture();
        f.append(&b"ok line\n".repeat(1000));
        f.read_with(|_, _, _| Err(Errno::IO));
        assert!(matches!(f.poll().await.0, Poll::Failed(WatchError::Lost)));
    }

    #[tokio::test]
    async fn a_read_failing_after_a_chunk_of_a_new_append_is_lost_output() {
        let mut f = fixture();
        f.append(b"first\n");
        assert!(matches!(f.poll().await.0, Poll::Drained));
        f.append(&b"ok line\n".repeat(2000));
        let calls = AtomicUsize::new(0);
        f.read_with(move |fd, buffer, offset| {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                let limit = buffer.len().min(CHUNK);
                pread(fd, &mut buffer[..limit], offset)
            } else {
                Err(Errno::IO)
            }
        });
        let (poll, lines) = f.poll().await;
        assert!(matches!(poll, Poll::Failed(WatchError::Lost)), "{poll:?}");
        assert_eq!(lines.len(), CHUNK / 8);
    }

    #[tokio::test]
    async fn end_of_file_before_the_accepted_end_keeps_the_backlog_pending_and_never_drains() {
        let mut f = fixture();
        f.append(&b"ok line\n".repeat(1000));
        let path = f.path();
        let calls = AtomicUsize::new(0);
        f.read_with(move |fd, buffer, offset| {
            let limit = buffer.len().min(CHUNK);
            let count = pread(fd, &mut buffer[..limit], offset)?;
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_len(0)
                    .unwrap();
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
    }

    #[tokio::test]
    async fn a_stuck_read_never_blocks_the_runtime_and_is_never_replaced() {
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
    }

    #[tokio::test]
    async fn a_same_size_replacement_is_detected_by_inode() {
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
        let replacement = f.dir.path().join("other");
        fs::write(&replacement, b"cccc\ndddd\n").unwrap();
        fs::rename(&replacement, f.path()).unwrap();
        assert!(matches!(f.poll().await.0, Poll::Failed(WatchError::Lost)));
    }

    #[tokio::test]
    async fn replacing_a_fully_drained_file_starts_the_new_one_from_the_top() {
        let mut f = fixture();
        f.append(b"old\n");
        assert_eq!(f.poll().await.1, ["old"]);
        let replacement = f.dir.path().join("other");
        fs::write(&replacement, b"new\nmore\n").unwrap();
        fs::rename(&replacement, f.path()).unwrap();
        assert_eq!(f.poll().await.1, ["new", "more"]);
        fs::write(f.path(), b"fresh\n").unwrap();
        assert_eq!(f.poll().await.1, ["fresh"]);
    }

    #[tokio::test]
    async fn an_oversized_line_stops_the_file_for_good_and_keeps_its_prefix_unclassified() {
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
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(f.watched.oversized(), Some(&vec![b'x'; LINE_LIMIT][..]));
        assert!(!f.watched.backlog() && f.watched.drained());
        assert!(matches!(f.poll().await.0, Poll::Drained));
        let (_, lines) = f.finish(far()).await;
        assert!(lines.is_empty());
        assert_eq!(f.watched.oversized().map(<[u8]>::len), Some(LINE_LIMIT));
    }

    #[tokio::test]
    async fn a_final_line_needs_a_drained_file() {
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
    }
}
