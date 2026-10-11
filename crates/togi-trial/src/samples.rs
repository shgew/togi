use std::io;
use std::time::Duration;

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
    #[error("the samples writer thread panicked")]
    Panicked,
}

/// Where accepted samples go; the real one is a file, tests block and fail it on cue.
pub(crate) trait Storage: Send + 'static {
    /// Writes the whole line and makes it durable.
    fn append(&mut self, line: &[u8]) -> io::Result<()>;
    fn close(self: Box<Self>) -> io::Result<()>;
}

/// Opens the storage on the writer thread, so a slow open delays nothing else.
pub(crate) type Open = Box<dyn FnOnce() -> Result<Box<dyn Storage>, StorageError> + Send>;

/// One std thread appends samples: queue of one, the newest sample is dropped when it is full.
#[derive(Debug)]
pub(crate) struct SamplesWriter {
    _private: (),
}

impl SamplesWriter {
    /// A storage failure is sent once on `failures` and ends the writer.
    pub(crate) fn spawn(open: Open, failures: UnboundedSender<StorageError>) -> io::Result<Self> {
        let _ = (open, failures);
        unimplemented!()
    }

    /// Never waits; returns whether the sample was queued.
    pub(crate) fn offer(&self, sample: &TrialConditions) -> bool {
        let _ = sample;
        unimplemented!()
    }

    /// Closes the queue and waits at most `bound` for the thread; a late thread is abandoned.
    pub(crate) fn finish(self, bound: Duration) -> Result<(), StorageError> {
        let _ = bound;
        unimplemented!()
    }
}
