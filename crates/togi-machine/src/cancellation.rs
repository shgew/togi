use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::Duration;

#[derive(Debug, Default)]
struct State {
    /// Keeps the ancestor chain alive, so a dropped intermediate token still forwards cancellation.
    _parent: Option<Arc<Self>>,
    cancelled: AtomicBool,
    children: Mutex<Vec<Weak<Self>>>,
    changed: Condvar,
}

#[derive(Clone, Debug, Default)]
pub struct CancellationToken {
    state: Arc<State>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.cancelled.load(Ordering::Acquire)
    }

    fn children(&self) -> MutexGuard<'_, Vec<Weak<State>>> {
        self.state
            .children
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn cancel(&self) {
        let mut registered_children = self.children();
        if self.state.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        let children = std::mem::take(&mut *registered_children);
        drop(registered_children);
        self.state.changed.notify_all();
        for child in children {
            if let Some(state) = child.upgrade() {
                Self { state }.cancel();
            }
        }
    }

    #[must_use]
    pub fn child_token(&self) -> Self {
        let child = Self {
            state: Arc::new(State {
                _parent: Some(Arc::clone(&self.state)),
                ..State::default()
            }),
        };
        let mut children = self.children();
        children.retain(|child| child.strong_count() != 0);
        if self.is_cancelled() {
            child.cancel();
        } else {
            children.push(Arc::downgrade(&child.state));
        }
        child
    }

    pub fn wait(&self) {
        let guard = self.children();
        let _guard = self
            .state
            .changed
            .wait_while(guard, |_| !self.is_cancelled())
            .unwrap_or_else(std::sync::PoisonError::into_inner);
    }

    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        let guard = self.children();
        let _guard = self
            .state
            .changed
            .wait_timeout_while(guard, timeout, |_| !self.is_cancelled())
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.is_cancelled()
    }
}
