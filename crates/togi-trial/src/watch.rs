use std::io;
use std::ops::ControlFlow;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;

use rustix::fs::{FileType, Mode, OFlags, fstat, openat};
use rustix::io::{Errno, pread};
use togi_backend::FileName;

use crate::output::{CHUNK, Frame, Framer};

/// The most one poll reads from one file.
pub(crate) const POLL_LIMIT: usize = 64 * 1024;

/// A file's identity: replacing it by another inode is detected, never silently resumed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileId {
    dev: rustix::fs::Dev,
    ino: u64,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum WatchError {
    #[error("unsupported watched output file")]
    Unsupported,
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
    /// The byte limit or the stop condition ended the poll with bytes still unread.
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

enum Opened {
    Missing,
    Regular { fd: OwnedFd, id: FileId, size: u64 },
    Other,
}

/// One regular file the backend writes in its instance directory, tailed without blocking.
#[derive(Debug)]
pub(crate) struct Watched {
    dir: Arc<OwnedFd>,
    name: FileName,
    state: State,
    framer: Framer,
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
        Self {
            dir,
            name,
            state: State::Unopened,
            framer: Framer::default(),
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

    /// Nothing more will be read now: fully read, never created, or rejected.
    pub(crate) const fn drained(&self) -> bool {
        match self.state {
            State::Tailing { drained, .. } => drained,
            State::Unopened | State::Stopped => true,
        }
    }

    fn open(&self) -> Result<Opened, Errno> {
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let fd = match openat(self.dir.as_fd(), self.name.as_str(), flags, Mode::empty()) {
            Ok(fd) => fd,
            Err(Errno::NOENT) => return Ok(Opened::Missing),
            Err(Errno::LOOP | Errno::NXIO) => return Ok(Opened::Other),
            Err(errno) => return Err(errno),
        };
        let stat = fstat(&fd)?;
        if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
            return Ok(Opened::Other);
        }
        Ok(Opened::Regular {
            id: FileId {
                dev: stat.st_dev,
                ino: stat.st_ino,
            },
            size: u64::try_from(stat.st_size).unwrap_or(0),
            fd,
        })
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

    /// Reads at most 64 KiB in 4 KiB chunks from the saved offset. `sink` breaks to stop between
    /// lines, `stop` is asked between chunks; the offset then sits exactly after the last
    /// consumed byte, so the next poll resumes without loss or duplication.
    pub(crate) fn poll(
        &mut self,
        sink: &mut impl FnMut(&str) -> ControlFlow<()>,
        stop: &mut impl FnMut() -> bool,
    ) -> Poll {
        if matches!(self.state, State::Stopped) {
            return Poll::Drained;
        }
        let (fd, id, size) = match self.open() {
            Err(errno) => return self.reject(WatchError::Read(errno.into())),
            Ok(Opened::Missing) => {
                return if self.backlog() {
                    self.reject(WatchError::Lost)
                } else {
                    Poll::Idle
                };
            }
            Ok(Opened::Other) => return self.reject(WatchError::Unsupported),
            Ok(Opened::Regular { fd, id, size }) => (fd, id, size),
        };
        let mut offset = match self.state {
            State::Tailing {
                id: known,
                offset,
                size: accepted,
                ..
            } if known == id && size >= accepted => offset,
            State::Tailing { .. } if self.backlog() => return self.reject(WatchError::Lost),
            State::Tailing { .. } | State::Unopened | State::Stopped => 0,
        };
        let limit = size.saturating_sub(offset).min(POLL_LIMIT as u64);
        let mut read = 0_u64;
        let mut interrupted = false;
        let mut buffer = [0_u8; CHUNK];
        while read < limit {
            if stop() {
                interrupted = true;
                break;
            }
            let want = usize::try_from(limit - read).unwrap_or(CHUNK).min(CHUNK);
            let count = match pread(&fd, &mut buffer[..want], offset) {
                Ok(0) => break,
                Ok(count) => count,
                Err(Errno::INTR) => continue,
                Err(errno) => return self.reject(WatchError::Read(errno.into())),
            };
            let mut exceeded = false;
            let consumed = self
                .framer
                .push(&buffer[..count], &mut |frame| match frame {
                    Frame::Line(line) => sink(&line),
                    Frame::Exceeded { .. } => {
                        exceeded = true;
                        ControlFlow::Break(())
                    }
                });
            if exceeded {
                return self.stop(WatchError::LineTooLong);
            }
            offset += consumed as u64;
            read += consumed as u64;
            if consumed < count {
                interrupted = true;
                break;
            }
        }
        let drained = !interrupted && offset >= size;
        self.state = State::Tailing {
            id,
            offset,
            size,
            drained,
        };
        if drained {
            Poll::Drained
        } else {
            Poll::Pending
        }
    }

    /// The final unterminated line, once the file is fully drained and its writers are gone.
    pub(crate) fn finish(&mut self, _writers: WritersStopped) -> Option<String> {
        match self.state {
            State::Tailing { drained: true, .. } => self.framer.finish(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, OpenOptions};
    use std::io::Write as _;
    use std::os::unix::fs::symlink;

    struct Fixture {
        dir: tempfile::TempDir,
        watched: Watched,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let fd = rustix::fs::open(
            dir.path(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        let watched = Watched::new(Arc::new(fd), FileName::from_static("results.txt"));
        Fixture { dir, watched }
    }

    impl Fixture {
        fn path(&self) -> std::path::PathBuf {
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

        fn poll(&mut self) -> (Poll, Vec<String>) {
            let mut lines = Vec::new();
            let poll = self.watched.poll(
                &mut |line| {
                    lines.push(line.to_owned());
                    ControlFlow::Continue(())
                },
                &mut || false,
            );
            (poll, lines)
        }
    }

    #[test]
    fn a_missing_file_is_idle_and_harmless() {
        let mut f = fixture();
        let (poll, lines) = f.poll();
        assert!(matches!(poll, Poll::Idle) && lines.is_empty());
        assert!(f.watched.drained() && !f.watched.backlog());
    }

    #[test]
    fn appended_lines_arrive_once_in_order() {
        let mut f = fixture();
        f.append(b"one\ntwo\n");
        let (poll, lines) = f.poll();
        assert!(matches!(poll, Poll::Drained));
        assert_eq!(lines, ["one", "two"]);
        f.append(b"three\n");
        assert_eq!(f.poll().1, ["three"]);
        assert!(f.poll().1.is_empty());
    }

    #[test]
    fn a_line_split_across_polls_is_one_line_and_the_tail_waits_for_writers() {
        let mut f = fixture();
        f.append(b"COMPUTE ");
        assert!(f.poll().1.is_empty());
        assert!(f.watched.backlog());
        f.append(b"ERROR\nFATAL: tail");
        assert_eq!(f.poll().1, ["COMPUTE ERROR"]);
        assert_eq!(
            f.watched.finish(WritersStopped::confirmed()).as_deref(),
            Some("FATAL: tail")
        );
        assert_eq!(f.watched.finish(WritersStopped::confirmed()), None);
    }

    #[test]
    fn a_large_append_is_read_in_bounded_polls() {
        let mut f = fixture();
        let line = b"ok ok ok ok ok ok ok ok\n";
        let total = 4 * 1024 * 1024 / line.len();
        f.append(&line.repeat(total));
        let mut polls = 0;
        let mut seen = 0;
        loop {
            let (poll, lines) = f.poll();
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

    #[test]
    fn stopping_between_lines_resumes_at_the_exact_offset() {
        let mut f = fixture();
        f.append(b"a\nb\nc\nd\n");
        let mut taken = Vec::new();
        let poll = f.watched.poll(
            &mut |line| {
                taken.push(line.to_owned());
                if taken.len() == 2 {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            },
            &mut || false,
        );
        assert!(matches!(poll, Poll::Pending));
        assert_eq!(taken, ["a", "b"]);
        assert!(!f.watched.drained() && f.watched.backlog());
        assert_eq!(f.poll().1, ["c", "d"]);
    }

    #[test]
    fn the_stop_condition_is_asked_before_each_chunk() {
        let mut f = fixture();
        f.append(&b"x\n".repeat(10_000));
        let mut asked = 0;
        let poll = f.watched.poll(&mut |_| ControlFlow::Continue(()), &mut || {
            asked += 1;
            asked > 2
        });
        assert!(matches!(poll, Poll::Pending));
        assert_eq!(asked, 3);
    }

    #[test]
    fn symlinks_directories_and_fifos_are_rejected_without_reading() {
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
            let (poll, lines) = f.poll();
            assert!(
                matches!(poll, Poll::Failed(WatchError::Unsupported)),
                "{poll:?}"
            );
            assert!(lines.is_empty());
            assert!(!f.watched.backlog());
            assert!(matches!(f.poll().0, Poll::Drained));
        }
    }

    #[test]
    fn removal_truncation_and_replacement_of_unread_bytes_are_lost() {
        let seed = |f: &mut Fixture| {
            f.append(&b"x\n".repeat(60_000));
            let mut count = 0;
            let poll = f.watched.poll(
                &mut |_| {
                    count += 1;
                    ControlFlow::Continue(())
                },
                &mut || false,
            );
            assert!(matches!(poll, Poll::Pending));
            assert!(f.watched.backlog());
        };
        let mut f = fixture();
        seed(&mut f);
        fs::remove_file(f.path()).unwrap();
        assert!(matches!(f.poll().0, Poll::Failed(WatchError::Lost)));

        let mut f = fixture();
        seed(&mut f);
        fs::write(f.path(), b"short\n").unwrap();
        assert!(matches!(f.poll().0, Poll::Failed(WatchError::Lost)));

        let mut f = fixture();
        seed(&mut f);
        let replacement = f.dir.path().join("other");
        fs::write(&replacement, b"y\n".repeat(60_000)).unwrap();
        fs::rename(&replacement, f.path()).unwrap();
        assert!(matches!(f.poll().0, Poll::Failed(WatchError::Lost)));

        let mut f = fixture();
        seed(&mut f);
        fs::remove_file(f.path()).unwrap();
        symlink("/dev/null", f.path()).unwrap();
        assert!(matches!(f.poll().0, Poll::Failed(WatchError::Lost)));
    }

    fn seed_backlog(f: &mut Fixture) {
        f.append(&b"ok line\n".repeat(128 * 1024 / 8));
        f.append(b"COMPUTE ERROR\n");
        assert!(matches!(f.poll().0, Poll::Pending));
        assert!(f.watched.backlog());
    }

    #[test]
    fn truncating_the_same_inode_below_the_accepted_size_loses_the_backlog() {
        let mut f = fixture();
        seed_backlog(&mut f);
        OpenOptions::new()
            .write(true)
            .open(f.path())
            .unwrap()
            .set_len(POLL_LIMIT as u64)
            .unwrap();
        assert!(matches!(f.poll().0, Poll::Failed(WatchError::Lost)));
    }

    #[test]
    fn an_open_error_with_an_unread_backlog_is_lost_output() {
        if rustix::process::geteuid().is_root() {
            return;
        }
        let mut f = fixture();
        seed_backlog(&mut f);
        std::fs::set_permissions(f.path(), std::os::unix::fs::PermissionsExt::from_mode(0)).unwrap();
        assert!(matches!(f.poll().0, Poll::Failed(WatchError::Lost)));
    }

    #[test]
    fn an_open_error_without_a_backlog_is_an_ordinary_read_error() {
        if rustix::process::geteuid().is_root() {
            return;
        }
        let mut f = fixture();
        f.append(b"fine\n");
        assert!(matches!(f.poll().0, Poll::Drained));
        std::fs::set_permissions(f.path(), std::os::unix::fs::PermissionsExt::from_mode(0)).unwrap();
        assert!(matches!(f.poll().0, Poll::Failed(WatchError::Read(_))));
    }

    #[test]
    fn a_same_size_replacement_is_detected_by_inode() {
        let mut f = fixture();
        f.append(b"aaaa\nbbbb\n");
        let mut first = true;
        let poll = f.watched.poll(
            &mut |_| {
                if std::mem::take(&mut first) {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            },
            &mut || false,
        );
        assert!(matches!(poll, Poll::Pending));
        let replacement = f.dir.path().join("other");
        fs::write(&replacement, b"cccc\ndddd\n").unwrap();
        fs::rename(&replacement, f.path()).unwrap();
        assert!(matches!(f.poll().0, Poll::Failed(WatchError::Lost)));
    }

    #[test]
    fn replacing_a_fully_drained_file_starts_the_new_one_from_the_top() {
        let mut f = fixture();
        f.append(b"old\n");
        assert_eq!(f.poll().1, ["old"]);
        let replacement = f.dir.path().join("other");
        fs::write(&replacement, b"new\nmore\n").unwrap();
        fs::rename(&replacement, f.path()).unwrap();
        assert_eq!(f.poll().1, ["new", "more"]);
        fs::write(f.path(), b"fresh\n").unwrap();
        assert_eq!(f.poll().1, ["fresh"]);
    }

    #[test]
    fn an_oversized_line_stops_the_file_for_good() {
        let mut f = fixture();
        f.append(&vec![b'x'; 1024 * 1024]);
        let mut total = 0;
        loop {
            let (poll, _) = f.poll();
            total += 1;
            match poll {
                Poll::Failed(WatchError::LineTooLong) => break,
                Poll::Pending => assert!(total < 100),
                other => panic!("{other:?}"),
            }
        }
        assert!(matches!(f.poll().0, Poll::Drained));
        assert_eq!(f.watched.finish(WritersStopped::confirmed()), None);
    }

    #[test]
    fn a_final_line_needs_a_drained_file() {
        let mut f = fixture();
        f.append(&b"x\n".repeat(60_000));
        f.append(b"tail");
        let mut seen = 0;
        let _ = f.watched.poll(
            &mut |_| {
                seen += 1;
                ControlFlow::Continue(())
            },
            &mut || false,
        );
        assert_eq!(f.watched.finish(WritersStopped::confirmed()), None);
        while !matches!(f.poll().0, Poll::Drained) {}
        assert_eq!(
            f.watched.finish(WritersStopped::confirmed()).as_deref(),
            Some("tail")
        );
    }
}
