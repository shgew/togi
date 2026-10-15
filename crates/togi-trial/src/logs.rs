use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::mpsc::{Receiver, Sender, SyncSender, TrySendError, channel, sync_channel};
use std::time::Duration;

use tokio::sync::oneshot;

use crate::evidence::Stream;
use crate::join::{self, Missing};

/// How many chunks may wait for the log thread before further output bytes are dropped.
const QUEUE: usize = 256;

pub(crate) type Sink = Box<dyn Write + Send>;
/// Decorates the retained output sink before the log thread writes to it.
pub(crate) type Wrap = Box<dyn Fn(Sink) -> Sink + Send>;

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
    /// The thread ended without reporting, so its failures are unknown.
    Vanished,
}

/// One std thread writes every output log of a trial, so a blocked filesystem write never stalls
/// the runtime thread; the queue is bounded and a full queue drops bytes instead of waiting.
pub(crate) struct LogWriter {
    queue: SyncSender<(Key, Vec<u8>)>,
    opens: Sender<(Key, Sink)>,
    wrap: Wrap,
    done: oneshot::Receiver<Vec<LogFailure>>,
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
        #[cfg(test)]
        if refusal::take() {
            return Err(io::Error::other("no threads left"));
        }
        let (queue, writes) = sync_channel(QUEUE);
        let (opens, registered) = channel();
        let (finished, done) = oneshot::channel();
        std::thread::Builder::new()
            .name("togi-logs".to_owned())
            .spawn(move || {
                let _ = finished.send(run(&writes, &registered));
            })?;
        Ok(Self {
            queue,
            opens,
            wrap: wrap.unwrap_or_else(|| Box::new(|sink| sink)),
            done,
        })
    }

    /// Hands `sink` to the log thread before any bytes for it can be queued.
    pub(crate) fn open(&self, instance: usize, stream: Stream, sink: Sink) -> LogHandle {
        let key = (instance, stream);
        let _ = self.opens.send((key, (self.wrap)(sink)));
        LogHandle {
            queue: self.queue.clone(),
            key,
            dropping: false,
        }
    }

    /// Closes the queue and waits until `bound` from now for queued bytes to be written; a thread
    /// stuck in a write is abandoned, and the wait holds no blocking-pool slot.
    pub(crate) async fn finish(self, bound: Duration) -> LogsEnd {
        let Self {
            queue, opens, done, ..
        } = self;
        drop((queue, opens));
        match join::report(done, bound).await {
            Ok(failures) => LogsEnd::Done(failures),
            Err(Missing::TimedOut) => LogsEnd::TimedOut(bound),
            Err(Missing::Vanished) => LogsEnd::Vanished,
        }
    }
}

/// Test seam: the next `LogWriter::spawn` on this thread fails as an exhausted OS would.
#[cfg(test)]
pub(crate) mod refusal {
    use std::cell::Cell;

    thread_local! {
        static NEXT: Cell<bool> = const { Cell::new(false) };
    }

    pub(crate) fn arm() {
        NEXT.with(|next| next.set(true));
    }

    pub(super) fn take() -> bool {
        NEXT.with(Cell::take)
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

    /// Signals `entered` each time a write starts, then blocks until `release` yields or closes.
    struct Stuck {
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::mpsc::Receiver<()>,
    }

    impl Write for Stuck {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let _ = self.entered.send(());
            let _ = self.release.recv();
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A log writer whose one sink is `Stuck`; returns the writer, the release gate and the
    /// receiver of the entered signals.
    fn stuck_writer() -> (
        LogWriter,
        std::sync::mpsc::Sender<()>,
        std::sync::mpsc::Receiver<()>,
    ) {
        let (gate, release) = std::sync::mpsc::channel();
        let (entered, entries) = std::sync::mpsc::channel();
        let stuck = Mutex::new(Some(Stuck { entered, release }));
        let writer = LogWriter::spawn(Some(Box::new(move |_| {
            Box::new(stuck.lock().unwrap().take().unwrap()) as Sink
        })))
        .unwrap();
        (writer, gate, entries)
    }

    fn await_entry(entries: &std::sync::mpsc::Receiver<()>) {
        entries
            .recv_timeout(Duration::from_secs(5))
            .expect("the log thread never entered its write");
    }

    #[test]
    fn bytes_reach_the_log_in_order_and_the_join_reports_success() {
        crate::testing::runtime(false).block_on(async {
            let written = Arc::new(Mutex::new(Vec::new()));
            let writer = LogWriter::spawn(None).unwrap();
            let mut log = writer.open(0, Stream::Stdout, Box::new(Shared(Arc::clone(&written))));
            log.append(b"one\n").unwrap();
            log.append(b"two\n").unwrap();
            drop(log);
            assert!(
                matches!(writer.finish(Duration::from_secs(5)).await, LogsEnd::Done(f) if f.is_empty())
            );
            assert_eq!(*written.lock().unwrap(), b"one\ntwo\n");
        });
    }

    #[test]
    fn a_write_error_is_reported_once_at_the_join() {
        crate::testing::runtime(false).block_on(async {
            let writer = LogWriter::spawn(None).unwrap();
            let mut log = writer.open(3, Stream::Stderr, Box::new(Failing));
            log.append(b"a").unwrap();
            log.append(b"b").unwrap();
            drop(log);
            match writer.finish(Duration::from_secs(5)).await {
                LogsEnd::Done(failures) => {
                    assert_eq!(failures.len(), 1);
                    assert_eq!(
                        (failures[0].instance, failures[0].stream),
                        (3, Stream::Stderr)
                    );
                }
                LogsEnd::TimedOut(_) | LogsEnd::Vanished => panic!("did not finish"),
            }
        });
    }

    #[test]
    fn a_stuck_write_drops_bytes_without_waiting_and_the_join_is_bounded() {
        crate::testing::runtime(false).block_on(async {
            let (writer, gate, entries) = stuck_writer();
            let mut log = writer.open(0, Stream::Stdout, Box::new(Vec::<u8>::new()));
            log.append(b"x").unwrap();
            await_entry(&entries);
            // The thread holds one chunk and cannot take another, so exactly QUEUE more fit.
            let started = std::time::Instant::now();
            let results: Vec<bool> = (0..(QUEUE + 10))
                .map(|_| log.append(b"x").is_ok())
                .collect();
            assert!(started.elapsed() < Duration::from_secs(1));
            assert!(
                results[..QUEUE].iter().all(|ok| *ok),
                "the queue holds QUEUE chunks"
            );
            assert!(!results[QUEUE], "the first drop is reported");
            assert!(
                results[QUEUE + 1..].iter().all(|ok| *ok),
                "only the first drop of a run is reported"
            );
            drop(log);
            assert!(matches!(
                writer.finish(Duration::from_millis(100)).await,
                LogsEnd::TimedOut(_)
            ));
            drop(gate);
        });
    }

    #[test]
    fn a_stuck_write_is_abandoned_at_the_join_bound_while_the_blocking_pool_is_saturated() {
        let (runtime, release) = crate::join::saturated_pool();
        let (gate, release) = runtime.block_on(async {
            let (writer, gate, _entries) = stuck_writer();
            let mut log = writer.open(0, Stream::Stdout, Box::new(Vec::<u8>::new()));
            log.append(b"x").unwrap();
            drop(log);
            let ended = tokio::time::timeout(
                Duration::from_secs(5),
                writer.finish(Duration::from_millis(100)),
            )
            .await
            .expect("the join outlived its bound behind the blocking pool");
            assert!(matches!(ended, LogsEnd::TimedOut(_)));
            (gate, release)
        });
        drop((gate, release));
    }
}
