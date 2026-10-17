use std::future::pending;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::oneshot;
use tokio::time::Instant;

use crate::error::{Error, PrepareError};
use crate::files::Chown;
use crate::join::{Missing, report};
use crate::supervisor::TrialCommand;

type Entered = dyn Fn(&str) + Send + Sync;

/// Runs the file preparation of a start (the trial directory, then each instance's files) on a
/// std thread, because storage can block it and the runtime must keep handling cancellation and
/// deadlines.
///
/// One preparation runs at a time. Its thread is never replaced while it is stuck and never
/// joined: one that is abandoned at its deadline keeps the preparer busy until it returns, and a
/// preparation asked for meanwhile is refused with `PrepareError::Busy`.
#[derive(Clone, Default)]
pub(crate) struct Preparer {
    busy: Arc<AtomicBool>,
    /// Test construction: how created files change owner.
    pub(crate) chown: Chown,
    /// An explicitly injected filesystem for descriptor-free fixtures.
    pub(crate) fs: Option<Arc<dyn togi_journal::fs::Fs>>,
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
    #[cfg(test)]
    pub(crate) fn with_fs(fs: Arc<dyn togi_journal::fs::Fs>) -> Self {
        Self {
            fs: Some(fs),
            ..Self::default()
        }
    }

    /// Runs `work` and waits for what it reports until `until`; a cancellation of the start ends
    /// the wait first. `step` names the work in the error of a missed deadline and `bound` is the
    /// budget the deadline came from.
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
        let (finished, done) = oneshot::channel();
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
                let _ = finished.send(outcome);
            })
            .map_err(PrepareError::Spawn)?;
        let cancel = async {
            loop {
                match commands.recv().await {
                    Some(TrialCommand::Cancel) => return,
                    Some(TrialCommand::Stop | TrialCommand::Abandon) => {}
                    None => pending::<()>().await,
                }
            }
        };
        let remaining = until.saturating_duration_since(Instant::now());
        tokio::select! {
            biased;
            () = cancel => Err(Error::Cancelled),
            outcome = report(done, remaining) => match outcome {
                Ok(work) => work.map_err(Error::from),
                Err(Missing::TimedOut) => Err(PrepareError::TimedOut {
                    what: step.to_owned(),
                    bound,
                }
                .into()),
                Err(Missing::Vanished) => Err(PrepareError::Vanished.into()),
            },
        }
    }
}
