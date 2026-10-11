use std::fs::File;
use std::io::{self, Write as _};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

use rustix::fs::{Mode, OFlags, fsync, openat};
use tokio::sync::mpsc::UnboundedSender;
use togi_machine::TrialConditions;

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("create the samples file")]
    Create(#[source] io::Error),
    #[error("sync the trial samples directory")]
    SyncDirectory(#[source] io::Error),
    #[error("encode a conditions sample")]
    Encode(#[source] serde_json::Error),
    #[error("append a conditions sample")]
    Append(#[source] io::Error),
    #[error("close the samples file")]
    Close(#[source] io::Error),
    #[error("the samples writer did not finish within {0:?}")]
    JoinTimeout(Duration),
    #[error("the samples writer thread ended without a result")]
    Vanished,
    #[error("start the samples writer thread")]
    Spawn(#[source] io::Error),
}

/// Every storage failure of one trial, in the order they happened.
#[derive(Debug, thiserror::Error)]
#[error("{}", .0.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "))]
pub struct StorageFailure(Vec<StorageError>);

impl StorageFailure {
    pub fn errors(&self) -> &[StorageError] {
        &self.0
    }
}

impl From<StorageError> for StorageFailure {
    fn from(error: StorageError) -> Self {
        Self(vec![error])
    }
}

/// Where accepted samples go; the real one is a file, tests block and fail it on cue.
pub(crate) trait Storage: Send + 'static {
    /// Writes the whole line and makes it durable.
    fn append(&mut self, line: &[u8]) -> io::Result<()>;
    fn close(self: Box<Self>) -> io::Result<()>;
}

/// Opens the storage on the writer thread, so a slow open delays nothing else.
pub(crate) type Open = Box<dyn FnOnce() -> Result<Box<dyn Storage>, StorageError> + Send>;

struct FileStorage(File);

impl Storage for FileStorage {
    fn append(&mut self, line: &[u8]) -> io::Result<()> {
        self.0.write_all(line)?;
        self.0.sync_all()
    }

    fn close(self: Box<Self>) -> io::Result<()> {
        self.0.sync_all()
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
        let storage = FileStorage(File::from(fd));
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
    done: Receiver<Result<(), StorageFailure>>,
    thread: JoinHandle<()>,
}

impl SamplesWriter {
    /// A storage failure wakes `failed` once and ends the writer; `finish` returns the errors.
    pub(crate) fn spawn(open: Open, failed: UnboundedSender<()>) -> Result<Self, StorageError> {
        let (queue, samples) = sync_channel::<Vec<u8>>(1);
        let (finished, done) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("togi-samples".to_owned())
            .spawn(move || {
                let result = run(open, &samples, &failed);
                drop(samples);
                let _ = finished.send(result);
            })
            .map_err(StorageError::Spawn)?;
        Ok(Self {
            queue,
            done,
            thread,
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

    /// Closes the queue and waits at most `bound` for the thread to drain and close the file; a
    /// thread stuck in storage is abandoned and reported.
    pub(crate) fn finish(self, bound: Duration) -> Result<(), StorageFailure> {
        let Self {
            queue,
            done,
            thread,
        } = self;
        drop(queue);
        match done.recv_timeout(bound) {
            Ok(result) => {
                let _ = thread.join();
                result
            }
            Err(RecvTimeoutError::Timeout) => Err(StorageError::JoinTimeout(bound).into()),
            Err(RecvTimeoutError::Disconnected) => Err(StorageError::Vanished.into()),
        }
    }
}

fn run(
    open: Open,
    samples: &Receiver<Vec<u8>>,
    failed: &UnboundedSender<()>,
) -> Result<(), StorageFailure> {
    let mut storage = match open() {
        Ok(storage) => storage,
        Err(error) => {
            let _ = failed.send(());
            return Err(error.into());
        }
    };
    let mut errors = Vec::new();
    while let Ok(line) = samples.recv() {
        if let Err(error) = storage.append(&line) {
            errors.push(StorageError::Append(error));
            let _ = failed.send(());
            break;
        }
    }
    if let Err(error) = storage.close() {
        errors.push(StorageError::Close(error));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(StorageFailure(errors))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    #[derive(Default)]
    struct Shared {
        lines: Mutex<Vec<Vec<u8>>>,
        closed: Mutex<bool>,
    }

    struct Scripted {
        shared: Arc<Shared>,
        gate: Option<Receiver<()>>,
        fail_append: bool,
        fail_close: bool,
    }

    impl Storage for Scripted {
        fn append(&mut self, line: &[u8]) -> io::Result<()> {
            if let Some(gate) = &self.gate {
                let _ = gate.recv();
            }
            if self.fail_append {
                return Err(io::Error::other("disk full"));
            }
            self.shared.lines.lock().unwrap().push(line.to_vec());
            Ok(())
        }

        fn close(self: Box<Self>) -> io::Result<()> {
            *self.shared.closed.lock().unwrap() = true;
            if self.fail_close {
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

    fn scripted(
        gate: Option<Receiver<()>>,
        fail_append: bool,
        fail_close: bool,
    ) -> (Arc<Shared>, Open) {
        let shared = Arc::new(Shared::default());
        let storage = Scripted {
            shared: Arc::clone(&shared),
            gate,
            fail_append,
            fail_close,
        };
        (shared, Box::new(move || Ok(Box::new(storage) as Box<dyn Storage>)))
    }

    #[test]
    fn samples_are_appended_in_order_as_json_lines_and_joined_on_finish() {
        let (shared, open) = scripted(None, false, false);
        let (failed, _wake) = tokio::sync::mpsc::unbounded_channel();
        let writer = SamplesWriter::spawn(open, failed).unwrap();
        assert_eq!(writer.offer(&sample(1)).unwrap(), Offered::Queued);
        writer.finish(Duration::from_secs(5)).unwrap();
        let lines = shared.lines.lock().unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0], b"{\"elapsed_ms\":1}\n");
        assert!(*shared.closed.lock().unwrap());
    }

    #[test]
    fn a_slow_write_drops_the_newest_sample_without_waiting() {
        let (gate, gated) = std::sync::mpsc::channel();
        let (shared, open) = scripted(Some(gated), false, false);
        let (failed, _wake) = tokio::sync::mpsc::unbounded_channel();
        let writer = SamplesWriter::spawn(open, failed).unwrap();
        assert_eq!(writer.offer(&sample(1)).unwrap(), Offered::Queued);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(writer.offer(&sample(2)).unwrap(), Offered::Queued);
        let started = Instant::now();
        assert_eq!(writer.offer(&sample(3)).unwrap(), Offered::Dropped);
        assert!(started.elapsed() < Duration::from_millis(500));
        for _ in 0..3 {
            gate.send(()).unwrap();
        }
        writer.finish(Duration::from_secs(5)).unwrap();
        let elapsed: Vec<_> = shared
            .lines
            .lock()
            .unwrap()
            .iter()
            .map(|l| String::from_utf8(l.clone()).unwrap())
            .collect();
        assert_eq!(elapsed, ["{\"elapsed_ms\":1}\n", "{\"elapsed_ms\":2}\n"]);
    }

    #[test]
    fn an_append_failure_wakes_the_supervisor_ends_the_writer_and_keeps_the_close_error() {
        let (_shared, open) = scripted(None, true, true);
        let (failed, mut wake) = tokio::sync::mpsc::unbounded_channel();
        let writer = SamplesWriter::spawn(open, failed).unwrap();
        writer.offer(&sample(1)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while wake.try_recv().is_err() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(writer.offer(&sample(2)).unwrap(), Offered::Dropped);
        let failure = writer.finish(Duration::from_secs(5)).unwrap_err();
        assert!(matches!(
            failure.errors(),
            [StorageError::Append(_), StorageError::Close(_)]
        ));
    }

    #[test]
    fn an_open_failure_is_reported_and_never_delays_the_caller() {
        let open: Open = Box::new(|| Err(StorageError::Create(io::Error::other("denied"))));
        let (failed, mut wake) = tokio::sync::mpsc::unbounded_channel();
        let writer = SamplesWriter::spawn(open, failed).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while wake.try_recv().is_err() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(matches!(
            writer.finish(Duration::from_secs(5)).unwrap_err().errors(),
            [StorageError::Create(_)]
        ));
    }

    #[test]
    fn a_stuck_writer_is_abandoned_at_the_join_bound() {
        let (gate, gated) = std::sync::mpsc::channel::<()>();
        let (_shared, open) = scripted(Some(gated), false, false);
        let (failed, _wake) = tokio::sync::mpsc::unbounded_channel();
        let writer = SamplesWriter::spawn(open, failed).unwrap();
        writer.offer(&sample(1)).unwrap();
        let started = Instant::now();
        let failure = writer.finish(Duration::from_millis(200)).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(matches!(failure.errors(), [StorageError::JoinTimeout(_)]));
        drop(gate);
    }

    #[test]
    fn the_real_file_is_exclusive_durable_and_one_json_object_per_line() {
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
        writer.finish(Duration::from_secs(5)).unwrap();
        let text = std::fs::read_to_string(parent.path().join("t1/samples.jsonl")).unwrap();
        assert_eq!(text, "{\"elapsed_ms\":7}\n");
        let (failed, _wake) = tokio::sync::mpsc::unbounded_channel();
        let again = SamplesWriter::spawn(create_file(&trial, &parent_fd), failed).unwrap();
        let failure = again.finish(Duration::from_secs(5)).unwrap_err();
        assert!(matches!(failure.errors(), [StorageError::Create(_)]));
        assert_eq!(
            std::fs::read_to_string(parent.path().join("t1/samples.jsonl")).unwrap(),
            text
        );
    }
}
