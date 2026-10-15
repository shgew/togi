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
    let resize = signal(SignalKind::window_change())?;
    let cancel = if terminating {
        Some([
            signal(SignalKind::interrupt())?,
            signal(SignalKind::terminate())?,
            signal(SignalKind::hangup())?,
        ])
    } else {
        None
    };
    Ok(forward(resize, cancel, events))
}

trait Receive: Send + 'static {
    fn recv(&mut self) -> impl Future<Output = Option<()>> + Send;
}

impl Receive for Signal {
    async fn recv(&mut self) -> Option<()> {
        Signal::recv(self).await
    }
}

fn forward(
    mut resize: impl Receive,
    mut cancel: Option<[impl Receive; 3]>,
    events: UnboundedSender<Event>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                Some(()) = resize.recv() => Event::Resize,
                () = cancelled(cancel.as_mut()) => Event::Cancel,
            };
            if events.send(event).is_err() {
                return;
            }
        }
    })
}

/// Waits for any of the cancelling signals, or forever when there are none.
async fn cancelled(signals: Option<&mut [impl Receive; 3]>) {
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
mod tests {
    use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

    use super::forward;
    use crate::terminal::Event;
    use crate::terminal::testing::paused;

    impl super::Receive for UnboundedReceiver<()> {
        async fn recv(&mut self) -> Option<()> {
            self.recv().await
        }
    }

    fn listener() -> (UnboundedSender<()>, UnboundedReceiver<()>) {
        unbounded_channel()
    }

    async fn next(queue: &mut UnboundedReceiver<Event>) -> Event {
        tokio::time::timeout(std::time::Duration::from_secs(20), queue.recv())
            .await
            .expect("no event within the paused-time bound")
            .expect("signal forwarding stopped")
    }

    #[test]
    fn a_dashboard_that_terminates_turns_each_signal_into_its_event() {
        paused(async {
            let (events, mut queue) = unbounded_channel();
            let (resize, resize_listener) = listener();
            let terminating = [listener(), listener(), listener()];
            let [interrupt, terminate, hangup] = terminating;
            let task = forward(
                resize_listener,
                Some([interrupt.1, terminate.1, hangup.1]),
                events,
            );
            resize.send(()).unwrap();
            assert!(matches!(next(&mut queue).await, Event::Resize));
            for signal in [interrupt.0, terminate.0, hangup.0] {
                signal.send(()).unwrap();
                assert!(matches!(next(&mut queue).await, Event::Cancel));
            }
            task.abort();
        });
    }

    #[test]
    fn a_dashboard_that_does_not_terminate_listens_only_for_resize() {
        paused(async {
            let (events, mut queue) = unbounded_channel();
            let (resize, resize_listener) = listener();
            let task = forward(resize_listener, None::<[UnboundedReceiver<()>; 3]>, events);
            tokio::task::yield_now().await;
            assert!(queue.try_recv().is_err());
            resize.send(()).unwrap();
            assert!(matches!(next(&mut queue).await, Event::Resize));
            task.abort();
        });
    }
}
