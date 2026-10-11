use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::{self, ThreadId};
use std::time::Duration;

type Callback = Box<dyn FnOnce() + Send>;

struct SlotState {
    callback: Option<Callback>,
    running_on: Option<ThreadId>,
}

struct SlotCell {
    state: Mutex<SlotState>,
    finished: Condvar,
}

type Slot = Arc<SlotCell>;

#[derive(Default)]
struct Callbacks {
    next_id: u64,
    slots: Vec<(u64, Slot)>,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Ends a running claim even when the callback panics, so a dropping guard never waits forever.
struct Claim<'a>(&'a SlotCell);

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        locked(&self.0.state).running_on = None;
        self.0.finished.notify_all();
    }
}

fn run(slot: &SlotCell) {
    let callback = {
        let mut state = locked(&slot.state);
        let callback = state.callback.take();
        if callback.is_some() {
            state.running_on = Some(thread::current().id());
        }
        callback
    };
    if let Some(callback) = callback {
        let _claim = Claim(slot);
        callback();
    }
}

type Payload = Box<dyn std::any::Any + Send>;

fn keep_first(first: &mut Option<Payload>, later: Payload) {
    if first.is_none() {
        *first = Some(later);
    } else {
        // A payload's Drop may panic, which would unwind out of the dispatch loop.
        std::mem::forget(later);
    }
}

#[cfg(test)]
thread_local! {
    /// Signalled under the slot lock right before a dropping guard waits on its callback.
    pub(crate) static BEFORE_WAIT: std::cell::RefCell<Option<std::sync::mpsc::Sender<()>>> =
        const { std::cell::RefCell::new(None) };
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
        let Some((state, id, slot)) = self.registration.take() else {
            return;
        };
        let unrun = locked(&slot.state).callback.take();
        locked(&state.callbacks)
            .slots
            .retain(|(slot_id, _)| *slot_id != id);
        // Captured values may cancel this token when dropped, so no lock is held here.
        drop(unrun);
        let this_thread = thread::current().id();
        let mut slot_state = locked(&slot.state);
        while slot_state
            .running_on
            .is_some_and(|running| running != this_thread)
        {
            #[cfg(test)]
            BEFORE_WAIT.with(|hook| {
                if let Some(waiting) = &*hook.borrow() {
                    let _ = waiting.send(());
                }
            });
            slot_state = slot
                .finished
                .wait(slot_state)
                .unwrap_or_else(PoisonError::into_inner);
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
        let mut panic = None;
        for (_, slot) in &slots {
            if let Err(payload) = catch_unwind(AssertUnwindSafe(|| run(slot))) {
                keep_first(&mut panic, payload);
            }
        }
        for child in children {
            if let Some(state) = child.upgrade() {
                if let Err(payload) = catch_unwind(AssertUnwindSafe(|| Self { state }.cancel())) {
                    keep_first(&mut panic, payload);
                }
            }
        }
        if let Some(payload) = panic {
            resume_unwind(payload);
        }
    }

    /// Runs `callback` exactly once when this token or an ancestor is cancelled: on the cancelling
    /// thread outside any lock, or at once on the calling thread if already cancelled. Dropping the
    /// returned guard before then keeps it from ever starting. Once the drop returns the callback
    /// is not running and never will start, unless the guard is dropped from inside the callback
    /// itself; a drop that races a callback a concurrent cancel already claimed waits for it to
    /// finish.
    pub fn on_cancel(&self, callback: impl FnOnce() + Send + 'static) -> OnCancel {
        let mut callbacks = locked(&self.state.callbacks);
        if self.is_cancelled() {
            drop(callbacks);
            callback();
            return OnCancel { registration: None };
        }
        let id = callbacks.next_id;
        callbacks.next_id += 1;
        let slot: Slot = Arc::new(SlotCell {
            state: Mutex::new(SlotState {
                callback: Some(Box::new(callback)),
                running_on: None,
            }),
            finished: Condvar::new(),
        });
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
