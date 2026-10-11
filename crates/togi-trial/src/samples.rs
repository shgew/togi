use std::fs::File;
use std::io::{self, Write as _};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use rustix::fs::{Mode, OFlags, fsync, openat};
use togi_machine::TrialConditions;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::error::{StorageError, StorageFailure};
use crate::join::{self, Missing};

/// Where accepted samples go; the real one is a file, tests block and fail it on cue.
pub(crate) trait Storage: Send + 'static {
    /// Writes the whole line and makes it durable.
    fn append(&mut self, line: &[u8]) -> io::Result<()>;
    fn close(self: Box<Self>) -> io::Result<()>;
}

/// Opens the storage on the writer thread, so a slow open delays nothing else.
pub(crate) type Open = Box<dyn FnOnce() -> Result<Box<dyn Storage>, StorageError> + Send>;

/// Closes the descriptor and reports what `close(2)` reports, which a destructor would discard.
fn close_descriptor(file: File) -> io::Result<()> {
    nix::unistd::close(file).map_err(io::Error::from)
}

struct FileStorage {
    file: File,
    close: fn(File) -> io::Result<()>,
}

/// Both failures of a close that could neither sync nor close cleanly.
#[derive(Debug, thiserror::Error)]
#[error("sync: {sync}; close: {close}")]
struct SyncAndClose {
    sync: io::Error,
    close: io::Error,
}

impl Storage for FileStorage {
    fn append(&mut self, line: &[u8]) -> io::Result<()> {
        self.file.write_all(line)?;
        self.file.sync_all()
    }

    fn close(self: Box<Self>) -> io::Result<()> {
        let Self { file, close } = *self;
        let synced = file.sync_all();
        let closed = close(file);
        match (synced, closed) {
            (Ok(()), closed) => closed,
            (Err(sync), Ok(())) => Err(sync),
            (Err(sync), Err(close)) => Err(io::Error::other(SyncAndClose { sync, close })),
        }
    }
}

/// Creates `samples.jsonl` exclusively below `trial` and syncs the new entry's directories.
pub(crate) fn create_file(trial: &Arc<OwnedFd>, parent: &Arc<OwnedFd>) -> Open {
    let (trial, parent) = (Arc::clone(trial), Arc::clone(parent));
    Box::new(move || {
        let fd = openat(
            trial.as_fd(),
            togi_trialfiles::SAMPLES,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o644),
        )
        .map_err(|errno| StorageError::Create(errno.into()))?;
        let storage = FileStorage {
            file: File::from(fd),
            close: close_descriptor,
        };
        for directory in [&trial, &parent] {
            fsync(directory).map_err(|errno| StorageError::SyncDirectory(errno.into()))?;
        }
        Ok(Box::new(storage) as Box<dyn Storage>)
    })
}

/// Whether `offer` queued the sample.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Offered {
    Queued,
    /// The queue was full or the writer has ended; the sample is gone.
    Dropped,
}

/// One std thread appends samples: queue of one, the newest sample is dropped when it is full.
#[derive(Debug)]
pub(crate) struct SamplesWriter {
    queue: SyncSender<Vec<u8>>,
    done: oneshot::Receiver<()>,
    errors: Errors,
}

/// What the thread has failed at so far, in order; the thread records each failure before it goes
/// on, so a join that gives up on a stuck thread still reports what already happened.
type Errors = Arc<Mutex<Vec<StorageError>>>;

fn record(errors: &Errors, error: StorageError) {
    errors
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .push(error);
}

impl SamplesWriter {
    /// A storage failure wakes `failed` once and ends the writer; `finish` returns the errors.
    pub(crate) fn spawn(open: Open, failed: UnboundedSender<()>) -> Result<Self, StorageError> {
        let (queue, samples) = sync_channel::<Vec<u8>>(1);
        let (finished, done) = oneshot::channel();
        let errors = Errors::default();
        let thread_errors = Arc::clone(&errors);
        std::thread::Builder::new()
            .name("togi-samples".to_owned())
            .spawn(move || {
                run(open, &samples, &failed, &thread_errors);
                drop(samples);
                let _ = finished.send(());
            })
            .map_err(StorageError::Spawn)?;
        Ok(Self {
            queue,
            done,
            errors,
        })
    }

    /// Never waits.
    pub(crate) fn offer(&self, sample: &TrialConditions) -> Result<Offered, StorageError> {
        let mut line = serde_json::to_vec(sample).map_err(StorageError::Encode)?;
        line.push(b'\n');
        Ok(match self.queue.try_send(line) {
            Ok(()) => Offered::Queued,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => Offered::Dropped,
        })
    }

    /// Closes the queue and waits until `bound` from now for the thread to drain and close the
    /// file. A thread stuck in storage is abandoned; every failure it recorded before then is
    /// returned first, then the join's own.
    pub(crate) async fn finish(self, bound: Duration) -> Result<(), StorageFailure> {
        let Self {
            queue,
            done,
            errors,
        } = self;
        drop(queue);
        let ended = join::report(done, bound).await;
        let mut errors =
            std::mem::take(&mut *errors.lock().unwrap_or_else(PoisonError::into_inner));
        match ended {
            Ok(()) => {}
            Err(Missing::TimedOut) => errors.push(StorageError::JoinTimeout(bound)),
            Err(Missing::Vanished) => errors.push(StorageError::Vanished),
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(StorageFailure(errors))
        }
    }
}

fn run(open: Open, samples: &Receiver<Vec<u8>>, failed: &UnboundedSender<()>, errors: &Errors) {
    let mut storage = match open() {
        Ok(storage) => storage,
        Err(error) => {
            record(errors, error);
            let _ = failed.send(());
            return;
        }
    };
    while let Ok(line) = samples.recv() {
        if let Err(error) = storage.append(&line) {
            record(errors, StorageError::Append(error));
            let _ = failed.send(());
            break;
        }
    }
    if let Err(error) = storage.close() {
        record(errors, StorageError::Close(error));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{Sender, channel};
    use std::time::Instant;
    use tokio::sync::mpsc::UnboundedReceiver;

    #[derive(Default)]
    struct Shared {
        lines: Mutex<Vec<Vec<u8>>>,
        closed: Mutex<bool>,
    }

    #[derive(Default)]
    struct Script {
        /// Each append waits for a message here before it goes on.
        gate: Option<Receiver<()>>,
        /// Told when an append begins.
        entered: Option<Sender<()>>,
        /// `close` waits for a message here before it returns.
        close_gate: Option<Receiver<()>>,
        fail_append: bool,
        fail_close: bool,
    }

    struct Scripted {
        shared: Arc<Shared>,
        script: Script,
    }

    impl Storage for Scripted {
        fn append(&mut self, line: &[u8]) -> io::Result<()> {
            if let Some(entered) = &self.script.entered {
                let _ = entered.send(());
            }
            if let Some(gate) = &self.script.gate {
                let _ = gate.recv();
            }
            if self.script.fail_append {
                return Err(io::Error::other("disk full"));
            }
            self.shared.lines.lock().unwrap().push(line.to_vec());
            Ok(())
        }

        fn close(self: Box<Self>) -> io::Result<()> {
            *self.shared.closed.lock().unwrap() = true;
            if let Some(gate) = &self.script.close_gate {
                let _ = gate.recv();
            }
            if self.script.fail_close {
                Err(io::Error::other("close failed"))
            } else {
                Ok(())
            }
        }
    }

    fn sample(ms: i64) -> TrialConditions {
        TrialConditions {
            elapsed_ms: ms,
            ..TrialConditions::default()
        }
    }

    fn scripted(script: Script) -> (Arc<Shared>, Open) {
        let shared = Arc::new(Shared::default());
        let storage = Scripted {
            shared: Arc::clone(&shared),
            script,
        };
        (
            shared,
            Box::new(move || Ok(Box::new(storage) as Box<dyn Storage>)),
        )
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    async fn woken(wake: &mut UnboundedReceiver<()>) {
        tokio::time::timeout(secs(5), wake.recv())
            .await
            .expect("the writer never reported its failure")
            .unwrap();
    }

    fn wait_until_the_writer_has_ended(writer: &SamplesWriter) {
        let deadline = Instant::now() + secs(5);
        while writer.offer(&sample(99)).unwrap() != Offered::Dropped {
            assert!(Instant::now() < deadline, "the writer never ended");
            std::thread::yield_now();
        }
    }

    #[tokio::test]
    async fn samples_are_appended_in_order_as_json_lines_and_joined_on_finish() {
        let (shared, open) = scripted(Script::default());
        let (failed, _wake) = tokio::sync::mpsc::unbounded_channel();
        let writer = SamplesWriter::spawn(open, failed).unwrap();
        assert_eq!(writer.offer(&sample(1)).unwrap(), Offered::Queued);
        writer.finish(secs(5)).await.unwrap();
        let lines = shared.lines.lock().unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"{\"elapsed_ms\":1}\n");
        assert!(*shared.closed.lock().unwrap());
    }

    #[tokio::test]
    async fn a_slow_write_drops_the_newest_sample_without_waiting() {
        let (gate, gated) = channel();
        let (entered, first_append) = channel();
        let (shared, open) = scripted(Script {
            gate: Some(gated),
            entered: Some(entered),
            ..Script::default()
        });
        let (failed, _wake) = tokio::sync::mpsc::unbounded_channel();
        let writer = SamplesWriter::spawn(open, failed).unwrap();
        assert_eq!(writer.offer(&sample(1)).unwrap(), Offered::Queued);
        first_append.recv().unwrap();
        assert_eq!(writer.offer(&sample(2)).unwrap(), Offered::Queued);
        let started = Instant::now();
        assert_eq!(writer.offer(&sample(3)).unwrap(), Offered::Dropped);
        assert!(started.elapsed() < Duration::from_millis(500));
        for _ in 0..3 {
            gate.send(()).unwrap();
        }
        writer.finish(secs(5)).await.unwrap();
        let elapsed: Vec<_> = shared
            .lines
            .lock()
            .unwrap()
            .iter()
            .map(|l| String::from_utf8(l.clone()).unwrap())
            .collect();
        assert_eq!(elapsed, ["{\"elapsed_ms\":1}\n", "{\"elapsed_ms\":2}\n"]);
    }

    #[tokio::test]
    async fn an_append_failure_wakes_the_supervisor_ends_the_writer_and_keeps_the_close_error() {
        let (_shared, open) = scripted(Script {
            fail_append: true,
            fail_close: true,
            ..Script::default()
        });
        let (failed, mut wake) = tokio::sync::mpsc::unbounded_channel();
        let writer = SamplesWriter::spawn(open, failed).unwrap();
        writer.offer(&sample(1)).unwrap();
        woken(&mut wake).await;
        wait_until_the_writer_has_ended(&writer);
        let failure = writer.finish(secs(5)).await.unwrap_err();
        assert!(matches!(
            failure.errors(),
            [StorageError::Append(_), StorageError::Close(_)]
        ));
    }

    #[tokio::test]
    async fn an_append_failure_survives_a_close_that_never_returns() {
        let (hold, close_gate) = channel::<()>();
        let (_shared, open) = scripted(Script {
            close_gate: Some(close_gate),
            fail_append: true,
            ..Script::default()
        });
        let (failed, mut wake) = tokio::sync::mpsc::unbounded_channel();
        let writer = SamplesWriter::spawn(open, failed).unwrap();
        writer.offer(&sample(1)).unwrap();
        woken(&mut wake).await;
        let failure = writer.finish(Duration::from_millis(200)).await.unwrap_err();
        assert!(
            matches!(
                failure.errors(),
                [StorageError::Append(_), StorageError::JoinTimeout(_)]
            ),
            "{:?}",
            failure.errors()
        );
        drop(hold);
    }

    #[tokio::test]
    async fn an_open_failure_is_reported_and_never_delays_the_caller() {
        let open: Open = Box::new(|| Err(StorageError::Create(io::Error::other("denied"))));
        let (failed, mut wake) = tokio::sync::mpsc::unbounded_channel();
        let writer = SamplesWriter::spawn(open, failed).unwrap();
        woken(&mut wake).await;
        assert!(matches!(
            writer.finish(secs(5)).await.unwrap_err().errors(),
            [StorageError::Create(_)]
        ));
    }

    #[tokio::test]
    async fn a_stuck_writer_is_abandoned_at_the_join_bound() {
        let (gate, gated) = channel::<()>();
        let (_shared, open) = scripted(Script {
            gate: Some(gated),
            ..Script::default()
        });
        let (failed, _wake) = tokio::sync::mpsc::unbounded_channel();
        let writer = SamplesWriter::spawn(open, failed).unwrap();
        writer.offer(&sample(1)).unwrap();
        let started = Instant::now();
        let failure = writer.finish(Duration::from_millis(200)).await.unwrap_err();
        assert!(started.elapsed() < secs(2));
        assert!(matches!(failure.errors(), [StorageError::JoinTimeout(_)]));
        drop(gate);
    }

    #[test]
    fn a_stuck_writer_is_abandoned_at_the_join_bound_while_the_blocking_pool_is_saturated() {
        let (runtime, release) = crate::join::saturated_pool();
        let (gate, gated) = channel::<()>();
        runtime.block_on(async {
            let (_shared, open) = scripted(Script {
                gate: Some(gated),
                ..Script::default()
            });
            let (failed, _wake) = tokio::sync::mpsc::unbounded_channel();
            let writer = SamplesWriter::spawn(open, failed).unwrap();
            writer.offer(&sample(1)).unwrap();
            let started = Instant::now();
            let failure = tokio::time::timeout(secs(5), writer.finish(Duration::from_millis(200)))
                .await
                .expect("the join outlived its bound behind the blocking pool")
                .unwrap_err();
            assert!(started.elapsed() < secs(2));
            assert!(matches!(failure.errors(), [StorageError::JoinTimeout(_)]));
        });
        drop((gate, release));
    }

    fn failing_close(file: File) -> io::Result<()> {
        drop(file);
        Err(io::Error::other("close failed"))
    }

    fn storage(file: File, close: fn(File) -> io::Result<()>) -> FileStorage {
        FileStorage { file, close }
    }

    fn dev_null() -> File {
        File::options().write(true).open("/dev/null").unwrap()
    }

    #[test]
    fn a_clean_sync_and_close_of_a_real_file_succeeds() {
        Box::new(storage(tempfile::tempfile().unwrap(), close_descriptor))
            .close()
            .unwrap();
    }

    #[test]
    fn an_error_from_closing_the_descriptor_is_returned_after_a_clean_sync() {
        let error = Box::new(storage(tempfile::tempfile().unwrap(), failing_close))
            .close()
            .unwrap_err();
        assert_eq!(error.to_string(), "close failed");
    }

    #[test]
    fn a_failed_sync_is_returned_and_the_descriptor_is_still_closed() {
        let error = Box::new(storage(dev_null(), close_descriptor))
            .close()
            .unwrap_err();
        assert!(!error.to_string().contains("close:"), "{error}");
    }

    #[test]
    fn a_failed_sync_and_a_failed_close_are_both_returned() {
        let error = Box::new(storage(dev_null(), failing_close))
            .close()
            .unwrap_err();
        let text = error.to_string();
        assert!(
            text.starts_with("sync: ") && text.ends_with("close: close failed"),
            "{text}"
        );
    }

    #[tokio::test]
    async fn a_descriptor_close_error_becomes_the_writers_close_error() {
        let open: Open = Box::new(|| {
            Ok(Box::new(storage(tempfile::tempfile().unwrap(), failing_close)) as Box<dyn Storage>)
        });
        let (failed, _wake) = tokio::sync::mpsc::unbounded_channel();
        let writer = SamplesWriter::spawn(open, failed).unwrap();
        writer.offer(&sample(1)).unwrap();
        let failure = writer.finish(secs(5)).await.unwrap_err();
        assert!(matches!(failure.errors(), [StorageError::Close(_)]));
    }

    #[tokio::test]
    async fn the_real_file_is_exclusive_durable_and_one_json_object_per_line() {
        let parent = tempfile::tempdir().unwrap();
        std::fs::create_dir(parent.path().join("t1")).unwrap();
        let fd = |path: &std::path::Path| {
            Arc::new(
                rustix::fs::open(path, OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty()).unwrap(),
            )
        };
        let trial = fd(&parent.path().join("t1"));
        let parent_fd = fd(parent.path());
        let (failed, _wake) = tokio::sync::mpsc::unbounded_channel();
        let writer = SamplesWriter::spawn(create_file(&trial, &parent_fd), failed).unwrap();
        writer.offer(&sample(7)).unwrap();
        writer.finish(secs(5)).await.unwrap();
        let text = std::fs::read_to_string(parent.path().join("t1/samples.jsonl")).unwrap();
        assert_eq!(text, "{\"elapsed_ms\":7}\n");
        let (failed, _wake) = tokio::sync::mpsc::unbounded_channel();
        let again = SamplesWriter::spawn(create_file(&trial, &parent_fd), failed).unwrap();
        let failure = again.finish(secs(5)).await.unwrap_err();
        assert!(matches!(failure.errors(), [StorageError::Create(_)]));
        assert_eq!(
            std::fs::read_to_string(parent.path().join("t1/samples.jsonl")).unwrap(),
            text
        );
    }
}
