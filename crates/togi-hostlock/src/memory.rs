use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::{Error, HostLock};

/// An isolated lock domain for tests, with exclusion lasting until the guard is dropped.
/// Clones share the domain; separate instances do not coordinate with kernel locks.
#[derive(Clone, Debug, Default)]
pub struct MemHostLocks {
    held: Arc<Mutex<BTreeSet<PathBuf>>>,
}

impl MemHostLocks {
    pub fn acquire(&self, path: &Path) -> Result<HostLock, Error> {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !held.insert(path.to_owned()) {
            return Err(Error::Busy {
                path: path.to_owned(),
            });
        }
        Ok(HostLock {
            _file: None,
            _memory: Some(Held {
                locks: self.clone(),
                path: path.to_owned(),
            }),
            path: path.to_owned(),
        })
    }
}

#[derive(Debug)]
pub(crate) struct Held {
    locks: MemHostLocks,
    path: PathBuf,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.locks
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusion_is_shared_until_the_guard_drops() {
        let locks = MemHostLocks::default();
        let other = locks.clone();
        let path = Path::new("/locks/togi.lock");
        let held = locks.acquire(path).unwrap();
        assert_eq!(held.path(), path);
        assert!(matches!(other.acquire(path), Err(Error::Busy { path: named }) if named == path));
        drop(held);
        assert!(other.acquire(path).is_ok());
    }

    #[test]
    fn different_paths_and_domains_are_independent() {
        let locks = MemHostLocks::default();
        let path = Path::new("/locks/togi.lock");
        let _held = locks.acquire(path).unwrap();
        let _other_path = locks.acquire(Path::new("/locks/other.lock")).unwrap();
        let _other_domain = MemHostLocks::default().acquire(path).unwrap();
    }
}
