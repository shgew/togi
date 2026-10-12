use std::time::Duration;

use tokio::sync::oneshot;
use tokio::time::{Instant, timeout_at};

/// Why a worker thread's report never came.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Missing {
    /// The deadline passed with the thread still working; it is abandoned.
    TimedOut,
    /// The thread ended without reporting.
    Vanished,
}

/// Waits for what a std worker thread reports on `done`, until `bound` from now. The wait is a
/// timer on the runtime and a channel the thread completes, so neither a stuck thread nor a busy
/// blocking pool can hold it past the deadline, and nothing joins the thread.
pub(crate) async fn report<T>(done: oneshot::Receiver<T>, bound: Duration) -> Result<T, Missing> {
    let _hold = ClockHold::new(bound);
    match timeout_at(Instant::now() + bound, done).await {
        Ok(Ok(report)) => Ok(report),
        Ok(Err(_)) => Err(Missing::Vanished),
        Err(_) => Err(Missing::TimedOut),
    }
}

/// While alive, a paused test clock does not jump over a worker thread that runs in real time:
/// tokio never auto-advances while a blocking task exists. The production clock never pauses, so
/// production holds nothing.
pub(crate) struct ClockHold {
    #[cfg(test)]
    _release: std::sync::mpsc::Sender<()>,
}

impl ClockHold {
    #[cfg(test)]
    pub(crate) fn new(bound: Duration) -> Self {
        let (release, held) = std::sync::mpsc::channel::<()>();
        drop(tokio::task::spawn_blocking(move || {
            let _ = held.recv_timeout(bound);
        }));
        Self { _release: release }
    }

    #[cfg(not(test))]
    pub(crate) fn new(_bound: Duration) -> Self {
        Self {}
    }
}

/// A runtime whose only blocking-pool thread is occupied until the returned sender is dropped, so
/// whatever is submitted to the pool queues behind it.
#[cfg(test)]
pub(crate) fn saturated_pool() -> (tokio::runtime::Runtime, std::sync::mpsc::Sender<()>) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    let (release, held) = std::sync::mpsc::channel::<()>();
    let (occupied, running) = std::sync::mpsc::channel::<()>();
    drop(runtime.spawn_blocking(move || {
        let _ = occupied.send(());
        let _ = held.recv();
    }));
    running.recv().unwrap();
    (runtime, release)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn what_the_thread_reports_is_returned() {
        let (finished, done) = oneshot::channel();
        std::thread::spawn(move || {
            let _ = finished.send(7);
        });
        assert_eq!(report(done, Duration::from_secs(5)).await, Ok(7));
    }

    #[tokio::test]
    async fn a_thread_that_ends_without_reporting_vanished() {
        let (finished, done) = oneshot::channel::<()>();
        drop(finished);
        assert_eq!(
            report(done, Duration::from_secs(5)).await,
            Err(Missing::Vanished)
        );
    }

    #[test]
    fn a_silent_thread_times_out_at_the_bound_while_the_blocking_pool_is_saturated() {
        let (runtime, release) = saturated_pool();
        runtime.block_on(async {
            let (finished, done) = oneshot::channel::<()>();
            let started = std::time::Instant::now();
            let outcome = tokio::time::timeout(
                Duration::from_secs(5),
                report(done, Duration::from_millis(100)),
            )
            .await
            .expect("the wait outlived its bound behind the blocking pool");
            assert_eq!(outcome, Err(Missing::TimedOut));
            assert!(started.elapsed() < Duration::from_secs(2));
            drop(finished);
        });
        drop(release);
    }
}
