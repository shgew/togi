use std::collections::HashMap;
use std::path::{Path, PathBuf};

use togi_machine::Backend;

/// The evidence identity of one backend package: the backend and the store path it ran from,
/// or no path when none was recorded. Restoring a path gives back the id it had.
#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct BackendId(usize);

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Backends {
    #[serde(with = "togi_machine::serde_pairs")]
    ids: HashMap<(Backend, Option<PathBuf>), BackendId>,
    active: [BackendId; 2],
}

const fn slot(backend: Backend) -> usize {
    match backend {
        Backend::Mprime => 0,
        Backend::Ycruncher => 1,
    }
}

impl Backends {
    pub(super) fn new() -> Self {
        let mut backends = Self {
            ids: HashMap::new(),
            active: [BackendId(0); 2],
        };
        backends.active = [
            backends.intern(Backend::Mprime, None),
            backends.intern(Backend::Ycruncher, None),
        ];
        backends
    }

    pub(super) fn intern(&mut self, backend: Backend, path: Option<&Path>) -> BackendId {
        let next = BackendId(self.ids.len());
        *self
            .ids
            .entry((backend, path.map(Path::to_path_buf)))
            .or_insert(next)
    }

    pub(super) fn set(&mut self, backend: Backend, path: Option<&Path>) {
        self.active[slot(backend)] = self.intern(backend, path);
    }

    pub(super) fn active(&self, backend: Backend) -> BackendId {
        self.active[slot(backend)]
    }
}
