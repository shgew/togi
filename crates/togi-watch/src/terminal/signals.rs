//! Signals: SIGINT, SIGTERM and SIGHUP cancel the dashboard, SIGWINCH redraws it.
//!
//! The listeners are tokio's, which stay registered for the rest of the process once a dashboard has made them:
//! afterwards none of these signals takes its default action, so a caller that shows the dashboard keeps its own
//! listener for SIGINT, SIGTERM and SIGHUP, as `togi run` does. SIGWINCH is ignored by default, so its listener
//! staying changes nothing.

use std::io;

use tokio::signal::unix::{Signal, SignalKind, signal};
use tokio::sync::mpsc::UnboundedSender;
use tokio::task::JoinHandle;

use super::Event;

/// Forwards SIGWINCH as a resize and, when `terminating`, SIGINT, SIGTERM and SIGHUP as a cancel, from a task its
/// owner aborts. It has to be called inside the runtime.
pub(super) fn listen(
    terminating: bool,
    events: UnboundedSender<Event>,
) -> io::Result<JoinHandle<()>> {
    let mut resize = signal(SignalKind::window_change())?;
    let mut cancel = if terminating {
        Some([
            signal(SignalKind::interrupt())?,
            signal(SignalKind::terminate())?,
            signal(SignalKind::hangup())?,
        ])
    } else {
        None
    };
    Ok(tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                Some(()) = resize.recv() => Event::Resize,
                () = cancelled(cancel.as_mut()) => Event::Cancel,
            };
            if events.send(event).is_err() {
                return;
            }
        }
    }))
}

/// Waits for any of the cancelling signals, or forever when there are none.
async fn cancelled(signals: Option<&mut [Signal; 3]>) {
    let Some([interrupt, terminate, hangup]) = signals else {
        return std::future::pending().await;
    };
    tokio::select! {
        Some(()) = interrupt.recv() => {}
        Some(()) = terminate.recv() => {}
        Some(()) = hangup.recv() => {}
        else => std::future::pending().await,
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::sync::{Mutex, MutexGuard, PoisonError};
    use std::time::Duration;

    use rustix::process::{Signal, getpid, kill_process};
    use tokio::signal::unix::{SignalKind, signal};
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
    use tokio::time::timeout;

    use super::listen;
    use crate::terminal::Event;

    static SERIAL: Mutex<()> = Mutex::new(());

    /// Held by every test that sends the process a signal or runs a dashboard that listens for them, so that under
    /// plain `cargo test`'s threads one test's signal never reaches another's dashboard. (`cargo nextest` runs each
    /// test in its own process.)
    pub(in crate::terminal) fn serial() -> MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A real signal is delivered within this, or the test fails.
    const BOUND: Duration = Duration::from_secs(20);

    async fn next(events: &mut UnboundedReceiver<Event>) -> Event {
        timeout(BOUND, events.recv())
            .await
            .expect("no event within the bound")
            .unwrap()
    }

    /// Runs a test's body on a fresh runtime while it holds [`serial`].
    fn serially(body: impl Future<Output = ()>) {
        let _serial = serial();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(body);
    }

    #[test]
    fn a_dashboard_that_terminates_turns_each_signal_into_its_event() {
        serially(async {
            let (events, mut queue) = unbounded_channel();
            let task = listen(true, events).unwrap();
            for (signal, resize) in [
                (Signal::WINCH, true),
                (Signal::INT, false),
                (Signal::TERM, false),
                (Signal::HUP, false),
            ] {
                kill_process(getpid(), signal).unwrap();
                let event = next(&mut queue).await;
                let expected = if resize {
                    matches!(event, Event::Resize)
                } else {
                    matches!(event, Event::Cancel)
                };
                assert!(expected, "{signal:?}: {event:?}");
            }
            task.abort();
        });
    }

    #[test]
    fn a_dashboard_that_does_not_terminate_leaves_sigint_sigterm_and_sighup_to_its_caller() {
        serially(async {
            let mut caller = [
                (Signal::INT, signal(SignalKind::interrupt()).unwrap()),
                (Signal::TERM, signal(SignalKind::terminate()).unwrap()),
                (Signal::HUP, signal(SignalKind::hangup()).unwrap()),
            ];
            let (events, mut queue) = unbounded_channel();
            let task = listen(false, events).unwrap();
            for (number, listener) in &mut caller {
                kill_process(getpid(), *number).unwrap();
                timeout(BOUND, listener.recv())
                    .await
                    .expect("the caller's listener missed its signal")
                    .unwrap();
                tokio::task::yield_now().await;
                tokio::task::yield_now().await;
                assert!(
                    queue.try_recv().is_err(),
                    "the dashboard took {number:?} from its caller"
                );
            }
            kill_process(getpid(), Signal::WINCH).unwrap();
            assert!(matches!(next(&mut queue).await, Event::Resize));
            task.abort();
        });
    }
}
