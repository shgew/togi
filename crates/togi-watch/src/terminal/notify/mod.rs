//! Telling the loop that the journal changed.
//!
//! On Linux the journal is watched with inotify; elsewhere it is polled once a second. Either way, a change is
//! delivered as [`Event::Changed`], and the next one only after the loop has acknowledged the reload for it, so
//! changes and reloads alternate one to one.

use std::time::Duration;

use tokio::sync::{mpsc, watch};

use super::{Event, WatchError};

#[cfg(target_os = "linux")]
pub(super) mod linux;
#[cfg(not(target_os = "linux"))]
pub(super) mod other;

#[cfg(target_os = "linux")]
pub(super) use linux::open;
#[cfg(not(target_os = "linux"))]
pub(super) use other::open;

/// How often the journal is looked for while nothing can be watched.
pub(super) const POLL: Duration = Duration::from_secs(1);

/// What is watched for a journal.
pub(super) trait Watch {
    /// Whether nothing watchable announces the journal's creation, so that it has to be looked for.
    fn polling(&self) -> bool;

    /// Arms the watches for the journal as it is now.
    fn arm(&mut self) -> Result<(), WatchError>;

    /// Waits for a change of the journal that matters, and arms the watches again for what it left.
    async fn changed(&mut self) -> Result<(), WatchError>;
}

/// The two ends of the loop's reload handshake, as the notifier sees them.
pub(super) struct Handshake {
    pub(super) changes: mpsc::UnboundedSender<Event>,
    pub(super) reloaded: mpsc::UnboundedReceiver<()>,
    /// Whether the frame on screen is frozen, in which case nothing may be polled for.
    pub(super) frozen: watch::Receiver<bool>,
}

impl Handshake {
    /// Tells the loop about a change and waits until it has reloaded for it. Reports whether to go on: a failure
    /// is delivered once and ends the notifier.
    async fn deliver(&mut self, change: Result<(), WatchError>) -> bool {
        match change {
            Ok(()) => {
                if self.changes.send(Event::Changed).is_err() {
                    return false;
                }
                self.reloaded.recv().await.is_some()
            }
            Err(error) => {
                let _ = self.changes.send(Event::Failed(error));
                false
            }
        }
    }

    /// Waits one polling interval, which never passes while a frozen frame is shown.
    async fn poll_interval(&mut self) {
        loop {
            if self.frozen.wait_for(|frozen| !*frozen).await.is_err() {
                return std::future::pending().await;
            }
            tokio::select! {
                () = tokio::time::sleep(POLL) => return,
                _ = self.frozen.changed() => {}
            }
        }
    }
}

/// Drives `watch` for the loop until the loop is gone or a failure has been delivered.
pub(super) async fn drive(watch: &mut impl Watch, handshake: &mut Handshake) {
    loop {
        let change = if watch.polling() {
            discover(watch, handshake).await
        } else {
            watch.changed().await
        };
        if !handshake.deliver(change).await {
            return;
        }
        if watch.polling() {
            // The change left no journal. Look again before the interval passes, so a journal that appeared
            // meanwhile is watched, not polled for, while a frame shows it.
            let looked = watch.arm().map(|()| watch.polling());
            match looked {
                Ok(true) => {}
                Ok(false) => {
                    if !handshake.deliver(Ok(())).await {
                        return;
                    }
                }
                Err(error) => {
                    handshake.deliver(Err(error)).await;
                    return;
                }
            }
        }
    }
}

/// Looks for the journal once an interval until it can be watched. Nothing is delivered while it is still
/// missing: it is only reported once it is watched, so no frame shows it while discovery still ticks.
async fn discover(watch: &mut impl Watch, handshake: &mut Handshake) -> Result<(), WatchError> {
    loop {
        handshake.poll_interval().await;
        watch.arm()?;
        if !watch.polling() {
            return Ok(());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::time::Duration;

    use tokio::sync::{mpsc, watch};
    use tokio::time::timeout;

    use super::{Handshake, POLL, Watch, drive};
    use crate::terminal::{Event, WatchError};

    /// A watch that plays a script: how many arms until the journal can be watched, and the changes it reports.
    struct Fake {
        polling: bool,
        appears_after: usize,
        arms: usize,
        changes: VecDeque<Result<(), WatchError>>,
        left_missing: bool,
    }

    impl Fake {
        fn polling(appears_after: usize) -> Self {
            Self {
                polling: true,
                appears_after,
                arms: 0,
                changes: VecDeque::new(),
                left_missing: false,
            }
        }

        fn watching(changes: impl IntoIterator<Item = Result<(), WatchError>>) -> Self {
            Self {
                polling: false,
                appears_after: 0,
                arms: 0,
                changes: changes.into_iter().collect(),
                left_missing: false,
            }
        }
    }

    impl Watch for Fake {
        fn polling(&self) -> bool {
            self.polling
        }

        fn arm(&mut self) -> Result<(), WatchError> {
            self.arms += 1;
            self.polling = self.arms < self.appears_after;
            Ok(())
        }

        async fn changed(&mut self) -> Result<(), WatchError> {
            match self.changes.pop_front() {
                Some(change) => {
                    if self.left_missing {
                        self.polling = true;
                    }
                    change
                }
                None => std::future::pending().await,
            }
        }
    }

    struct Loop {
        events: mpsc::UnboundedReceiver<Event>,
        reloaded: mpsc::UnboundedSender<()>,
        frozen: watch::Sender<bool>,
        handshake: Option<Handshake>,
    }

    fn handshake() -> Loop {
        let (changes, events) = mpsc::unbounded_channel();
        let (reloaded, acks) = mpsc::unbounded_channel();
        let (frozen, frozen_rx) = watch::channel(false);
        Loop {
            events,
            reloaded,
            frozen,
            handshake: Some(Handshake {
                changes,
                reloaded: acks,
                frozen: frozen_rx,
            }),
        }
    }

    fn io_error() -> WatchError {
        WatchError {
            operation: "watch state directory",
            source: std::io::Error::other("gone"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_change_waits_for_its_reload_before_the_notifier_goes_on() {
        let mut l = handshake();
        let mut handshake = l.handshake.take().unwrap();
        let mut watch = Fake::watching([Ok(()), Ok(())]);
        let task = tokio::spawn(async move {
            drive(&mut watch, &mut handshake).await;
            watch.changes.len()
        });
        assert!(matches!(l.events.recv().await, Some(Event::Changed)));
        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::task::yield_now().await;
        assert!(l.events.try_recv().is_err(), "the second change came before the reload was acknowledged");
        l.reloaded.send(()).unwrap();
        assert!(matches!(l.events.recv().await, Some(Event::Changed)));
        drop(l.reloaded);
        assert_eq!(task.await.unwrap(), 0, "the notifier ended once the loop was gone");
    }

    #[tokio::test(start_paused = true)]
    async fn polling_looks_once_a_second_and_reports_the_journal_once_it_is_watched() {
        let mut l = handshake();
        let mut handshake = l.handshake.take().unwrap();
        let mut watch = Fake::polling(4);
        let task = tokio::spawn(async move {
            drive(&mut watch, &mut handshake).await;
            watch.arms
        });
        tokio::time::advance(POLL * 4 - Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert!(l.events.try_recv().is_err(), "reported before the journal could be watched");
        tokio::time::advance(Duration::from_millis(1)).await;
        assert!(matches!(l.events.recv().await, Some(Event::Changed)));
        drop(l.reloaded);
        assert_eq!(task.await.unwrap(), 4, "three looks while missing, one that found it");
    }

    #[tokio::test(start_paused = true)]
    async fn a_frozen_frame_is_never_polled_behind() {
        let mut l = handshake();
        l.frozen.send_replace(true);
        let mut handshake = l.handshake.take().unwrap();
        let mut watch = Fake::polling(1);
        let task = tokio::spawn(async move {
            drive(&mut watch, &mut handshake).await;
            watch.arms
        });
        tokio::time::advance(Duration::from_secs(6 * 3600)).await;
        tokio::task::yield_now().await;
        assert!(l.events.try_recv().is_err());
        l.frozen.send_replace(false);
        tokio::time::advance(POLL).await;
        assert!(matches!(l.events.recv().await, Some(Event::Changed)));
        drop(l.reloaded);
        assert_eq!(task.await.unwrap(), 1, "nothing was looked at while frozen");
    }

    #[tokio::test(start_paused = true)]
    async fn a_change_that_leaves_no_journal_looks_again_before_the_interval() {
        let mut l = handshake();
        let mut handshake = l.handshake.take().unwrap();
        let mut watch = Fake::watching([Ok(())]);
        watch.left_missing = true;
        watch.appears_after = 1;
        let task = tokio::spawn(async move {
            drive(&mut watch, &mut handshake).await;
            watch.arms
        });
        assert!(matches!(l.events.recv().await, Some(Event::Changed)));
        l.reloaded.send(()).unwrap();
        assert!(matches!(l.events.recv().await, Some(Event::Changed)), "the journal that appeared meanwhile is reported at once");
        drop(l.reloaded);
        assert_eq!(task.await.unwrap(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failure_is_delivered_once_and_ends_the_notifier() {
        let mut l = handshake();
        let mut handshake = l.handshake.take().unwrap();
        let mut watch = Fake::watching([Err(io_error()), Ok(())]);
        let task = tokio::spawn(async move { drive(&mut watch, &mut handshake).await });
        assert!(matches!(l.events.recv().await, Some(Event::Failed(_))));
        timeout(Duration::from_secs(1), task).await.unwrap().unwrap();
        assert!(l.events.recv().await.is_none());
    }
}
