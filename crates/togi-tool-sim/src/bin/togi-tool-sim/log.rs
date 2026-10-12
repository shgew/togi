use std::cell::RefCell;
use std::io::{self, BufWriter, IsTerminal as _, Write};
use std::rc::Rc;

const BUFFER: usize = 64 << 10;

/// Standard error as the run's log. A terminal sees each line at once; anything else gets one write per 64 KiB, so
/// a run writing a line per event does not make a system call per event. The first write or flush error is kept
/// and fails the command even when the session went on without its log.
#[derive(Clone)]
pub(crate) struct Log(Rc<RefCell<Sink>>);

struct Sink {
    out: Box<dyn Write>,
    failed: Option<io::Error>,
}

impl Log {
    pub(crate) fn stderr() -> Self {
        let stderr = io::stderr();
        let terminal = stderr.is_terminal();
        Self::for_stream(stderr, terminal)
    }

    /// The log for a stream: a terminal unbuffered, anything else buffered.
    fn for_stream(out: impl Write + 'static, is_terminal: bool) -> Self {
        if is_terminal {
            Self::new(Box::new(out))
        } else {
            Self::buffered(out)
        }
    }

    pub(crate) fn buffered(out: impl Write + 'static) -> Self {
        Self::new(Box::new(BufWriter::with_capacity(BUFFER, out)))
    }

    fn new(out: Box<dyn Write>) -> Self {
        Self(Rc::new(RefCell::new(Sink { out, failed: None })))
    }

    /// Writes one line now, for a line a reader waits for before the run's own output.
    pub(crate) fn announce(&mut self, line: &str) {
        let _ = writeln!(self, "{line}");
        let _ = self.flush();
    }

    pub(crate) fn finish(self) -> io::Result<()> {
        let mut sink = self.0.borrow_mut();
        if let Some(error) = sink.failed.take() {
            return Err(error);
        }
        sink.out.flush()
    }
}

impl Sink {
    fn keep<T>(&mut self, result: io::Result<T>) -> io::Result<T> {
        if let Err(error) = &result
            && self.failed.is_none()
        {
            self.failed = Some(io::Error::new(error.kind(), error.to_string()));
        }
        result
    }
}

impl Write for Log {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut sink = self.0.borrow_mut();
        let result = sink.out.write(bytes);
        sink.keep(result)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut sink = self.0.borrow_mut();
        let result = sink.out.flush();
        sink.keep(result)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::io::{self, Write};
    use std::rc::Rc;

    use super::Log;

    #[derive(Clone, Default)]
    struct Shared(Rc<RefCell<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct Broken;

    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_buffered_log_reaches_its_stream_only_when_finished() {
        let out = Shared::default();
        let mut log = Log::buffered(out.clone());
        writeln!(log, "line").unwrap();
        assert!(out.0.borrow().is_empty());
        log.finish().unwrap();
        assert_eq!(*out.0.borrow(), b"line\n");
    }

    #[test]
    fn a_stream_log_is_buffered_unless_the_stream_is_a_terminal() {
        for terminal in [false, true] {
            let out = Shared::default();
            let mut log = Log::for_stream(out.clone(), terminal);
            writeln!(log, "line").unwrap();
            let before: &[u8] = if terminal { b"line\n" } else { b"" };
            assert_eq!(*out.0.borrow(), before, "terminal {terminal}");
            log.finish().unwrap();
            assert_eq!(*out.0.borrow(), b"line\n", "terminal {terminal}");
        }
    }

    #[test]
    fn an_announcement_is_written_at_once() {
        let out = Shared::default();
        let mut log = Log::buffered(out.clone());
        log.announce("sim: state directory /x");
        assert_eq!(*out.0.borrow(), b"sim: state directory /x\n");
    }

    #[test]
    fn a_failed_flush_fails_the_finish() {
        let mut log = Log::buffered(Broken);
        writeln!(log, "line").unwrap();
        assert_eq!(log.finish().unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn an_error_the_session_swallowed_still_fails_the_finish() {
        let mut log = Log::new(Box::new(Broken));
        assert!(writeln!(log, "line").is_err());
        assert_eq!(log.finish().unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    }
}
