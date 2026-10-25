//! Derivations memoized between queries. A cache is never state: it is skipped by serialization, equal
//! to every other cache, and rebuilt from the folded books when it is empty or its key no longer fits.

use std::sync::{Mutex, MutexGuard, PoisonError};

pub(super) struct Cache<T>(Mutex<T>);

impl<T> Cache<T> {
    pub(super) fn lock(&self) -> MutexGuard<'_, T> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<T: Default> Default for Cache<T> {
    fn default() -> Self {
        Self(Mutex::new(T::default()))
    }
}

impl<T: Clone> Clone for Cache<T> {
    fn clone(&self) -> Self {
        Self(Mutex::new(self.lock().clone()))
    }
}

impl<T> std::fmt::Debug for Cache<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Cache")
    }
}

impl<T> PartialEq for Cache<T> {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}
