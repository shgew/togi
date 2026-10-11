use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::Duration;

type Slot = Arc<Mutex<Option<Box<dyn FnOnce() + Send>>>>;

#[derive(Default)]
struct Callbacks {
    next_id: u64,
    slots: Vec<(u64, Slot)>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn run(slot: &Slot) {
    let callback = locked(slot).take();
    if let Some(callback) = callback {
        callback();
    }
}

#[derive(Default)]
struct State {
    /// Keeps the ancestor chain alive, so a dropped intermediate token still forwards cancellation.
    _parent: Option<Arc<Self>>,
    cancelled: AtomicBool,
    children: Mutex<Vec<Weak<Self>>>,
    callbacks: Mutex<Callbacks>,
    changed: Condvar,
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State")
            .field("cancelled", &self.cancelled)
            .finish_non_exhaustive()
    }
}

/// Keeps a cancellation callback registered; dropping it deregisters the callback.
#[must_use = "dropping the guard deregisters the callback"]
pub struct OnCancel {
    registration: Option<(Arc<State>, u64, Slot)>,
}

impl std::fmt::Debug for OnCancel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OnCancel").finish_non_exhaustive()
    }
}

impl Drop for OnCancel {
    fn drop(&mut self) {
        if let Some((state, id, slot)) = self.registration.take() {
            drop(locked(&slot).take());
            locked(&state.callbacks).slots.retain(|(slot_id, _)| *slot_id != id);
        }
    }
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
        let slots = std::mem::take(&mut locked(&self.state.callbacks).slots);
        self.state.changed.notify_all();
        for (_, slot) in &slots {
            run(slot);
        }
        for child in children {
            if let Some(state) = child.upgrade() {
                Self { state }.cancel();
            }
        }
    }

    /// Runs `callback` exactly once when this token or an ancestor is cancelled: on the cancelling
    /// thread outside any lock, or at once on the calling thread if already cancelled. Dropping the
    /// returned guard before then keeps it from ever starting; a callback a concurrent cancel has
    /// already started still completes.
    pub fn on_cancel(&self, callback: impl FnOnce() + Send + 'static) -> OnCancel {
        let mut callbacks = locked(&self.state.callbacks);
        if self.is_cancelled() {
            drop(callbacks);
            callback();
            return OnCancel { registration: None };
        }
        let id = callbacks.next_id;
        callbacks.next_id += 1;
        let slot: Slot = Arc::new(Mutex::new(Some(Box::new(callback))));
        callbacks.slots.push((id, Arc::clone(&slot)));
        OnCancel {
            registration: Some((Arc::clone(&self.state), id, slot)),
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
