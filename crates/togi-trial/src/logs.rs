use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Write};
use std::sync::mpsc::{Receiver, Sender, SyncSender, TrySendError, channel, sync_channel};
use std::time::Duration;

use crate::evidence::Stream;

/// How many chunks may wait for the log thread before further output bytes are dropped.
const QUEUE: usize = 256;

pub(crate) type Sink = Box<dyn Write + Send>;
/// Turns a retained log descriptor into what the log thread writes to; tests wrap it.
pub(crate) type Wrap = Box<dyn Fn(File) -> Sink + Send>;

type Key = (usize, Stream);

/// A log write that failed, for the instance and stream it belongs to.
#[derive(Debug)]
pub(crate) struct LogFailure {
    pub(crate) instance: usize,
    pub(crate) stream: Stream,
    pub(crate) error: io::Error,
}

/// How the log thread ended.
#[derive(Debug)]
pub(crate) enum LogsEnd {
    Done(Vec<LogFailure>),
    TimedOut(Duration),
}

/// One std thread writes every output log of a trial, so a blocked filesystem write never stalls
/// the runtime thread; the queue is bounded and a full queue drops bytes instead of waiting.
pub(crate) struct LogWriter {
    queue: SyncSender<(Key, Vec<u8>)>,
    opens: Sender<(Key, Sink)>,
    wrap: Wrap,
    done: tokio::sync::oneshot::Receiver<Vec<LogFailure>>,
}

/// What a pipe reader holds to append its consumed bytes to its log.
pub(crate) struct LogHandle {
    queue: SyncSender<(Key, Vec<u8>)>,
    key: Key,
    dropping: bool,
}

/// The log thread fell behind or ended; the bytes were not logged.
#[derive(Debug)]
pub(crate) struct Dropped;

impl LogHandle {
    /// Never waits. Reports only the first drop of a run of drops.
    pub(crate) fn append(&mut self, bytes: &[u8]) -> Result<(), Dropped> {
        match self.queue.try_send((self.key, bytes.to_vec())) {
            Ok(()) => {
                self.dropping = false;
                Ok(())
            }
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                if std::mem::replace(&mut self.dropping, true) {
                    Ok(())
                } else {
                    Err(Dropped)
                }
            }
        }
    }
}

impl LogWriter {
    pub(crate) fn spawn(wrap: Option<Wrap>) -> io::Result<Self> {
        let (queue, writes) = sync_channel(QUEUE);
        let (opens, registered) = channel();
        let (finished, done) = tokio::sync::oneshot::channel();
        std::thread::Builder::new()
            .name("togi-logs".to_owned())
            .spawn(move || {
                let _ = finished.send(run(&writes, &registered));
            })?;
        Ok(Self {
            queue,
            opens,
            wrap: wrap.unwrap_or_else(|| Box::new(|file| Box::new(file) as Sink)),
            done,
        })
    }

    /// Hands `file` to the log thread before any bytes for it can be queued.
    pub(crate) fn open(&self, instance: usize, stream: Stream, file: File) -> LogHandle {
        let key = (instance, stream);
        let _ = self.opens.send((key, (self.wrap)(file)));
        LogHandle {
            queue: self.queue.clone(),
            key,
            dropping: false,
        }
    }

    /// Closes the queue and waits at most `bound` for queued bytes to be written, without blocking
    /// the runtime; a thread stuck in a write is abandoned.
    pub(crate) async fn finish(self, bound: Duration) -> LogsEnd {
        let Self {
            queue, opens, done, ..
        } = self;
        drop((queue, opens));
        match tokio::time::timeout(bound, done).await {
            Ok(Ok(failures)) => LogsEnd::Done(failures),
            Ok(Err(_)) => LogsEnd::Done(Vec::new()),
            Err(_) => LogsEnd::TimedOut(bound),
        }
    }
}

fn run(writes: &Receiver<(Key, Vec<u8>)>, registered: &Receiver<(Key, Sink)>) -> Vec<LogFailure> {
    let mut sinks: HashMap<Key, Option<Sink>> = HashMap::new();
    let mut failures = Vec::new();
    while let Ok((key, bytes)) = writes.recv() {
        while let Ok((key, sink)) = registered.try_recv() {
            sinks.insert(key, Some(sink));
        }
        let Some(slot) = sinks.get_mut(&key) else {
            continue;
        };
        let Some(sink) = slot else {
            continue;
        };
        if let Err(error) = sink.write_all(&bytes) {
            *slot = None;
            failures.push(LogFailure {
                instance: key.0,
                stream: key.1,
                error,
            });
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct Failing;

    impl Write for Failing {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("disk full"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct Stuck(std::sync::mpsc::Receiver<()>);

    impl Write for Stuck {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let _ = self.0.recv();
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn file() -> File {
        tempfile::tempfile().unwrap()
    }

    #[tokio::test]
    async fn bytes_reach_the_log_in_order_and_the_join_reports_success() {
        let written = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&written);
        let writer = LogWriter::spawn(Some(Box::new(move |_| {
            Box::new(Shared(Arc::clone(&sink))) as Sink
        })))
        .unwrap();
        let mut log = writer.open(0, Stream::Stdout, file());
        log.append(b"one\n").unwrap();
        log.append(b"two\n").unwrap();
        drop(log);
        assert!(matches!(writer.finish(Duration::from_secs(5)).await, LogsEnd::Done(f) if f.is_empty()));
        assert_eq!(*written.lock().unwrap(), b"one\ntwo\n");
    }

    #[tokio::test]
    async fn a_write_error_is_reported_once_at_the_join() {
        let writer = LogWriter::spawn(Some(Box::new(|_| Box::new(Failing) as Sink))).unwrap();
        let mut log = writer.open(3, Stream::Stderr, file());
        log.append(b"a").unwrap();
        log.append(b"b").unwrap();
        drop(log);
        match writer.finish(Duration::from_secs(5)).await {
            LogsEnd::Done(failures) => {
                assert_eq!(failures.len(), 1);
                assert_eq!((failures[0].instance, failures[0].stream), (3, Stream::Stderr));
            }
            LogsEnd::TimedOut(_) => panic!("timed out"),
        }
    }

    #[tokio::test]
    async fn a_stuck_write_drops_bytes_without_waiting_and_the_join_is_bounded() {
        let (gate, stuck) = std::sync::mpsc::channel();
        let stuck = Mutex::new(Some(stuck));
        let writer = LogWriter::spawn(Some(Box::new(move |_| {
            Box::new(Stuck(stuck.lock().unwrap().take().unwrap())) as Sink
        })))
        .unwrap();
        let mut log = writer.open(0, Stream::Stdout, file());
        let started = std::time::Instant::now();
        let mut dropped = 0;
        for _ in 0..(QUEUE + 10) {
            if log.append(b"x").is_err() {
                dropped += 1;
            }
        }
        assert_eq!(dropped, 1, "only the first drop of a run is reported");
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(log);
        assert!(matches!(
            writer.finish(Duration::from_millis(100)).await,
            LogsEnd::TimedOut(_)
        ));
        drop(gate);
    }
}
