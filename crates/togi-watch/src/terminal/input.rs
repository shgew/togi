//! The keyboard reader: a thread that turns raw input into keys, and that every exit of the loop joins.

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::Errno;
use tokio::sync::mpsc::UnboundedSender;

use super::Event;
use super::keys::{Decoder, Key};

/// How long a lone escape waits for the rest of a sequence before it is the Esc key.
pub(super) const ESCAPE_TIMEOUT: Duration = Duration::from_millis(50);

/// What a wait for input ended with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Ready {
    Input,
    Timeout,
    Stop,
}

/// Where the reader gets bytes, and how it is told to stop.
pub(super) trait Source {
    /// Waits for input, for the stop signal, or for `timeout` to pass.
    fn wait(&mut self, timeout: Option<Duration>) -> io::Result<Ready>;
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
}

/// Reads keys from `source` into `send` until it ends, stops, fails or `send` has nobody to deliver to.
pub(super) fn read_keys(source: &mut impl Source, mut send: impl FnMut(Key) -> bool) {
    let mut decoder = Decoder::default();
    let mut buf = [0_u8; 128];
    loop {
        let timeout = decoder.is_pending().then_some(ESCAPE_TIMEOUT);
        match source.wait(timeout) {
            Ok(Ready::Input) => {}
            Ok(Ready::Timeout) => {
                if !decoder.flush().into_iter().all(&mut send) {
                    return;
                }
                continue;
            }
            Ok(Ready::Stop) | Err(_) => return,
        }
        match source.read(&mut buf) {
            Ok(0) => {
                decoder.flush().into_iter().all(&mut send);
                return;
            }
            Ok(read) => {
                if !decoder.push(&buf[..read]).into_iter().all(&mut send) {
                    return;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) => {}
            Err(_) => return,
        }
    }
}

/// A file descriptor and a private wake pipe, polled together so that stopping never closes or changes the
/// caller's file.
pub(super) struct FdSource<'a> {
    input: BorrowedFd<'a>,
    wake: BorrowedFd<'a>,
}

impl<'a> FdSource<'a> {
    pub(super) fn new(input: BorrowedFd<'a>, wake: BorrowedFd<'a>) -> Self {
        Self { input, wake }
    }
}

impl Source for FdSource<'_> {
    fn wait(&mut self, timeout: Option<Duration>) -> io::Result<Ready> {
        let timeout = timeout.map(|timeout| Timespec {
            tv_sec: crate::numbers::saturating_timeout_seconds(timeout.as_secs()),
            tv_nsec: i64::from(timeout.subsec_nanos()),
        });
        loop {
            let mut fds = [
                PollFd::new(&self.input, PollFlags::IN),
                PollFd::new(&self.wake, PollFlags::IN),
            ];
            match poll(&mut fds, timeout.as_ref()) {
                Ok(0) => return Ok(Ready::Timeout),
                Ok(_) => {}
                Err(Errno::INTR) => continue,
                Err(errno) => return Err(errno.into()),
            }
            let [input, wake] = fds.map(|fd| fd.revents());
            if !wake.is_empty() || input.contains(PollFlags::NVAL) {
                return Ok(Ready::Stop);
            }
            if input.intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR) {
                return Ok(Ready::Input);
            }
        }
    }

    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        rustix::io::read(self.input, buf).map_err(io::Error::from)
    }
}

/// A pipe that no child process inherits, made with the flag in one call so a fork on another thread cannot
/// catch it without.
#[cfg(not(target_vendor = "apple"))]
pub(super) fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    Ok(rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC)?)
}

/// A pipe that no child process inherits. Apple platforms have no `pipe2`, so the flag is set after.
#[cfg(target_vendor = "apple")]
pub(super) fn pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    let (read, write) = rustix::pipe::pipe()?;
    rustix::io::fcntl_setfd(&read, rustix::io::FdFlags::CLOEXEC)?;
    rustix::io::fcntl_setfd(&write, rustix::io::FdFlags::CLOEXEC)?;
    Ok((read, write))
}

/// A running keyboard reader. Dropping it wakes and joins the reader thread.
pub(super) struct Reader<'scope> {
    wake: OwnedFd,
    thread: Option<std::thread::ScopedJoinHandle<'scope, ()>>,
}

impl<'scope> Reader<'scope> {
    /// Starts reading `input` on a thread of `scope`. Every key is sent as [`Event::Key`] and the end of the
    /// input as [`Event::InputClosed`].
    pub(super) fn start<'env>(
        scope: &'scope std::thread::Scope<'scope, 'env>,
        input: BorrowedFd<'env>,
        events: UnboundedSender<Event>,
    ) -> io::Result<Self> {
        let (wake_read, wake) = pipe()?;
        let thread = scope.spawn(move || {
            let mut source = FdSource::new(input, wake_read.as_fd());
            read_keys(&mut source, |key| events.send(Event::Key(key)).is_ok());
            let _ = events.send(Event::InputClosed);
        });
        Ok(Self {
            wake,
            thread: Some(thread),
        })
    }
}

impl Drop for Reader<'_> {
    fn drop(&mut self) {
        let _ = rustix::io::write(&self.wake, &[1]);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::collections::VecDeque;
    use std::io;
    use std::time::Duration;
    use test_case::test_case;

    use super::{ESCAPE_TIMEOUT, Ready, Source, read_keys};
    use crate::terminal::keys::Key;

    /// One step of a scripted input.
    pub(in crate::terminal) enum Step {
        Data(&'static [u8]),
        Timeout,
        Eof,
        Stop,
        Fail,
    }

    /// An input that plays a script and records the timeout every wait asked for.
    pub(in crate::terminal) struct Script {
        steps: VecDeque<Step>,
        pub(in crate::terminal) timeouts: Vec<Option<Duration>>,
        data: Option<&'static [u8]>,
    }

    impl Script {
        pub(in crate::terminal) fn new(steps: impl IntoIterator<Item = Step>) -> Self {
            Self {
                steps: steps.into_iter().collect(),
                timeouts: Vec::new(),
                data: None,
            }
        }
    }

    impl Source for Script {
        fn wait(&mut self, timeout: Option<Duration>) -> io::Result<Ready> {
            self.timeouts.push(timeout);
            match self.steps.pop_front() {
                Some(Step::Data(bytes)) => {
                    self.data = Some(bytes);
                    Ok(Ready::Input)
                }
                Some(Step::Timeout) => Ok(Ready::Timeout),
                Some(Step::Eof) => {
                    self.data = Some(b"");
                    Ok(Ready::Input)
                }
                Some(Step::Stop) | None => Ok(Ready::Stop),
                Some(Step::Fail) => Err(io::Error::other("poll failed")),
            }
        }

        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let data = self.data.take().unwrap_or_default();
            buf[..data.len()].copy_from_slice(data);
            Ok(data.len())
        }
    }

    fn keys(steps: impl IntoIterator<Item = Step>) -> (Vec<Key>, Vec<Option<Duration>>) {
        let mut script = Script::new(steps);
        let mut keys = Vec::new();
        read_keys(&mut script, |key| {
            keys.push(key);
            true
        });
        (keys, script.timeouts)
    }

    #[test]
    fn a_split_sequence_waits_a_bounded_time_and_is_one_key() {
        let (keys, timeouts) = keys([Step::Data(b"\x1b["), Step::Data(b"A"), Step::Eof]);
        assert_eq!(keys, [Key::Up]);
        assert_eq!(timeouts, [None, Some(ESCAPE_TIMEOUT), None]);
    }

    #[test]
    fn an_ss3_arrow_byte_by_byte_waits_after_each_byte_and_is_one_key() {
        let (keys, timeouts) = keys([
            Step::Data(b"\x1b"),
            Step::Data(b"O"),
            Step::Data(b"A"),
            Step::Eof,
        ]);
        assert_eq!(keys, [Key::Up]);
        assert_eq!(
            timeouts,
            [None, Some(ESCAPE_TIMEOUT), Some(ESCAPE_TIMEOUT), None],
            "each fragment waits the whole timeout again"
        );
    }

    #[test_case(b"\x1b", vec![Key::Esc]; "escape")]
    #[test_case(b"\x1b[", vec![]; "csi prefix")]
    #[test_case(b"\x1bO", vec![]; "ss3 prefix")]
    fn a_quiet_escape_prefix_times_out_into_esc_or_nothing_and_keeps_the_next_key(
        prefix: &'static [u8],
        mut want: Vec<Key>,
    ) {
        let (keys, timeouts) = keys([
            Step::Data(prefix),
            Step::Timeout,
            Step::Data(b"j"),
            Step::Eof,
        ]);
        want.push(Key::Char('j'));
        assert_eq!(keys, want, "{prefix:?}");
        assert_eq!(timeouts[1], Some(ESCAPE_TIMEOUT));
        assert_eq!(timeouts[2], None, "the timeout released the prefix");
    }

    #[test]
    fn end_of_input_flushes_a_lone_escape_and_ends() {
        let (keys, _) = keys([Step::Data(b"\x1b"), Step::Eof]);
        assert_eq!(keys, [Key::Esc]);
    }

    #[test_case(Step::Stop; "a stop")]
    #[test_case(Step::Fail; "a failed wait")]
    fn a_stop_or_a_failed_wait_ends_without_flushing(end: Step) {
        let (keys, _) = keys([Step::Data(b"\x1b"), end]);
        assert_eq!(keys, []);
    }

    #[test]
    fn a_receiver_that_is_gone_ends_the_reader() {
        let mut script = Script::new([Step::Data(b"ab"), Step::Data(b"c")]);
        let mut sent = 0;
        read_keys(&mut script, |_| {
            sent += 1;
            false
        });
        assert_eq!(sent, 1);
        assert_eq!(script.timeouts.len(), 1);
    }

    mod pipes {
        use std::os::fd::{AsFd as _, OwnedFd};
        use std::time::Duration;

        use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

        use crate::terminal::Event;
        use crate::terminal::input::Reader;
        use crate::terminal::keys::Key;

        fn pipe() -> (OwnedFd, OwnedFd) {
            crate::terminal::input::pipe().unwrap()
        }

        /// The next event, or a failure if none comes within five seconds.
        fn receive(queue: &mut UnboundedReceiver<Event>) -> Option<Event> {
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap()
                .block_on(async {
                    tokio::time::timeout(Duration::from_secs(5), queue.recv()).await
                })
                .unwrap_or_else(|_| panic!("the reader sent nothing within five seconds"))
        }

        fn assert_closed_on_exec(end: &OwnedFd) {
            assert!(
                rustix::io::fcntl_getfd(end)
                    .unwrap()
                    .contains(rustix::io::FdFlags::CLOEXEC)
            );
        }

        #[test]
        fn real_fs_the_read_end_of_a_pipe_is_closed_on_exec() {
            assert_closed_on_exec(&pipe().0);
        }

        #[test]
        fn real_fs_the_write_end_of_a_pipe_is_closed_on_exec() {
            assert_closed_on_exec(&pipe().1);
        }

        #[test]
        fn real_fs_a_reader_on_a_real_pipe_decodes_a_sequence_and_reports_the_end() {
            let (read, write) = pipe();
            let (events, mut queue) = unbounded_channel();
            std::thread::scope(|scope| {
                let reader = Reader::start(scope, read.as_fd(), events).unwrap();
                rustix::io::write(&write, b"\x1b[Aj").unwrap();
                drop(write);
                let mut keys = Vec::new();
                loop {
                    match receive(&mut queue) {
                        Some(Event::Key(key)) => keys.push(key),
                        Some(Event::InputClosed) => break,
                        other => panic!("{other:?}"),
                    }
                }
                assert_eq!(keys, [Key::Up, Key::Char('j')]);
                drop(reader);
            });
        }

        #[test]
        fn real_fs_stopping_an_idle_reader_joins_it_and_leaves_the_callers_pipe_alone() {
            let (read, write) = pipe();
            let (events, mut queue) = unbounded_channel();
            std::thread::scope(|scope| {
                let reader = Reader::start(scope, read.as_fd(), events).unwrap();
                drop(reader);
            });
            assert!(matches!(
                receive(&mut queue),
                Some(Event::InputClosed) | None
            ));
            rustix::io::write(&write, b"remaining input").unwrap();
            let mut buf = [0_u8; 15];
            assert_eq!(rustix::io::read(&read, &mut buf).unwrap(), 15);
            assert_eq!(&buf, b"remaining input");
        }

        #[test]
        fn real_fs_a_lone_escape_becomes_the_esc_key_after_the_timeout() {
            let (read, write) = pipe();
            let (events, mut queue) = unbounded_channel();
            std::thread::scope(|scope| {
                let reader = Reader::start(scope, read.as_fd(), events).unwrap();
                rustix::io::write(&write, b"\x1b").unwrap();
                assert!(matches!(receive(&mut queue), Some(Event::Key(Key::Esc))));
                drop(reader);
            });
        }
    }
}
