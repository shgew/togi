use std::future::pending;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, channel};
use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{Instant, timeout_at};

use crate::error::{Error, PrepareError};
use crate::files::Chown;
use crate::supervisor::TrialCommand;

type Entered = dyn Fn(&str) + Send + Sync;

/// Runs a start's file preparation, which can block on storage, on its own thread so the runtime
/// keeps handling cancellation and deadlines; the runtime waits on the blocking pool for at most
/// the time left, under an absolute deadline of its own, as the log and samples writers are joined. One preparation runs at a time and its thread is
/// never replaced while stuck or joined: a preparation abandoned at its deadline leaves the
/// preparer busy until it returns, and the next one is refused rather than stacked on it.
#[derive(Clone, Default)]
pub(crate) struct Preparer {
    busy: Arc<AtomicBool>,
    /// Test construction: how created files change owner.
    pub(crate) chown: Chown,
    /// Test construction: called on the preparation thread with the step's name before it works.
    pub(crate) entered: Option<Arc<Entered>>,
}

struct Release(Arc<AtomicBool>);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Preparer {
    /// Runs `work` and waits for it until `until`; a cancellation of the start ends the wait too.
    /// `step` names the work in the error of a missed deadline, `bound` is the budget it was
    /// part of.
    pub(crate) async fn run<T: Send + 'static>(
        &self,
        step: &str,
        until: Instant,
        bound: Duration,
        commands: &mut UnboundedReceiver<TrialCommand>,
        cancelled: &AtomicBool,
        work: impl FnOnce() -> Result<T, PrepareError> + Send + 'static,
    ) -> Result<T, Error> {
        if cancelled.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        if self.busy.swap(true, Ordering::AcqRel) {
            return Err(PrepareError::Busy.into());
        }
        let release = Release(Arc::clone(&self.busy));
        let (done, result) = channel();
        let entered = self.entered.clone();
        let name = step.to_owned();
        std::thread::Builder::new()
            .name("togi-trial-prepare".to_owned())
            .spawn(move || {
                if let Some(entered) = entered {
                    entered(&name);
                }
                let outcome = work();
                drop(release);
                let _ = done.send(outcome);
            })
            .map_err(PrepareError::Spawn)?;
        let cancel = async {
            loop {
                match commands.recv().await {
                    Some(TrialCommand::Cancel) => return,
                    Some(_) => {}
                    None => pending::<()>().await,
                }
            }
        };
        let remaining = until.saturating_duration_since(Instant::now());
        let wait = tokio::task::spawn_blocking(move || result.recv_timeout(remaining));
        let timed_out = || PrepareError::TimedOut {
            what: step.to_owned(),
            bound,
        };
        tokio::select! {
            biased;
            () = cancel => Err(Error::Cancelled),
            outcome = timeout_at(until, wait) => match outcome {
                Ok(Ok(Ok(work))) => work.map_err(Error::from),
                Ok(Ok(Err(RecvTimeoutError::Timeout))) | Err(_) => Err(timed_out().into()),
                Ok(Ok(Err(RecvTimeoutError::Disconnected)) | Err(_)) => {
                    Err(PrepareError::Vanished.into())
                }
            },
        }
    }
}
