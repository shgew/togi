//! The time the synchronous side bounds its waits on the runtime thread by.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub(crate) enum Clock {
    System,
    /// Test time that moves only when told to; the runtime thread then runs on tokio's paused
    /// clock.
    #[cfg(test)]
    Manual(std::sync::Arc<Manual>),
}

impl Clock {
    pub(crate) fn now(&self) -> Instant {
        match self {
            Self::System => Instant::now(),
            #[cfg(test)]
            Self::Manual(manual) => manual.now(),
        }
    }

    /// Receives from `from` unless `timeout` passes on this clock first.
    pub(crate) fn recv_timeout<T>(
        &self,
        from: &Receiver<T>,
        timeout: Duration,
    ) -> Result<T, RecvTimeoutError> {
        match self {
            Self::System => from.recv_timeout(timeout),
            #[cfg(test)]
            Self::Manual(manual) => manual.recv_timeout(from, timeout),
        }
    }

    /// Whether the runtime thread runs on tokio's paused clock.
    #[cfg(test)]
    pub(crate) fn paused(&self) -> bool {
        matches!(self, Self::Manual(_))
    }
}

/// A clock that moves only when a test moves it: a wait that finds nothing queued blocks until a
/// message arrives or the test moves the clock past its deadline.
#[cfg(test)]
pub(crate) struct Manual {
    start: Instant,
    state: std::sync::Mutex<State>,
    waiting: std::sync::Condvar,
}

#[cfg(test)]
#[derive(Default)]
struct State {
    elapsed: Duration,
    /// The deadline of the wait in progress.
    deadline: Option<Duration>,
}

#[cfg(test)]
impl Manual {
    pub(crate) fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            start: Instant::now(),
            state: std::sync::Mutex::default(),
            waiting: std::sync::Condvar::new(),
        })
    }

    pub(crate) fn elapsed(&self) -> Duration {
        self.state.lock().unwrap().elapsed
    }

    pub(crate) fn now(&self) -> Instant {
        self.start + self.elapsed()
    }

    pub(crate) fn advance(&self, by: Duration) {
        self.state.lock().unwrap().elapsed += by;
    }

    /// Waits until a wait is in progress, then moves the clock to its deadline, as when nothing
    /// answers it in time.
    pub(crate) fn expire_waiting(&self) {
        let state = self.state.lock().unwrap();
        let mut state = self
            .waiting
            .wait_while(state, |state| state.deadline.is_none())
            .unwrap();
        let deadline = state.deadline.unwrap();
        state.elapsed = state.elapsed.max(deadline);
    }

    fn recv_timeout<T>(
        &self,
        from: &Receiver<T>,
        timeout: Duration,
    ) -> Result<T, RecvTimeoutError> {
        {
            let mut state = self.state.lock().unwrap();
            state.deadline = Some(state.elapsed + timeout);
            self.waiting.notify_all();
        }
        let received = self.until_deadline(from);
        self.state.lock().unwrap().deadline = None;
        received
    }

    fn until_deadline<T>(&self, from: &Receiver<T>) -> Result<T, RecvTimeoutError> {
        loop {
            match from.recv_timeout(Duration::from_millis(1)) {
                Err(RecvTimeoutError::Timeout) => {}
                received @ (Ok(_) | Err(RecvTimeoutError::Disconnected)) => return received,
            }
            let state = self.state.lock().unwrap();
            if state
                .deadline
                .is_some_and(|deadline| state.elapsed >= deadline)
            {
                return Err(RecvTimeoutError::Timeout);
            }
        }
    }
}
