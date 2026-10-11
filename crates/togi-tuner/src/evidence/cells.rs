use std::collections::HashMap;
use std::sync::Arc;

use togi_journal::Seq;
use togi_machine::CoreId;

use crate::Profile;

use super::{BackendId, SessionRef};

#[cfg(test)]
thread_local! {
    static COPIED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The elements copied on this thread, because a shared list was written, since the last call.
#[cfg(test)]
pub(crate) fn take_copied() -> u64 {
    COPIED.with(|copied| copied.replace(0))
}

/// The list behind `shared`, copied first when another clone still holds it.
pub(crate) fn unique<T: Clone>(shared: &mut Arc<Vec<T>>) -> &mut Vec<T> {
    #[cfg(test)]
    if Arc::strong_count(shared) > 1 {
        let length = u64::try_from(shared.len()).unwrap_or(u64::MAX);
        COPIED.with(|copied| copied.set(copied.get() + length));
    }
    Arc::make_mut(shared)
}

/// Seqs in ascending order; each lookup is a binary search. A clone shares the storage until
/// either side changes it, so cloning a state costs the cells, not the seqs they hold.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct Seqs(Arc<Vec<Seq>>);

impl Seqs {
    pub(super) fn push(&mut self, seq: Seq) {
        debug_assert!(self.0.last().is_none_or(|&last| last < seq));
        unique(&mut self.0).push(seq);
    }

    /// Adds `seq` at its place; the seqs it moves past are the few recorded after it.
    pub(super) fn insert(&mut self, seq: Seq) {
        if let Err(at) = self.0.binary_search(&seq) {
            unique(&mut self.0).insert(at, seq);
        }
    }

    pub(super) fn last(&self) -> Option<Seq> {
        self.0.last().copied()
    }

    /// The seqs above `floor`.
    pub(super) fn after(&self, floor: Option<Seq>) -> &[Seq] {
        let start = self.0.partition_point(|&seq| Some(seq) <= floor);
        &self.0[start..]
    }

    /// Removes `seq`, saying whether it was here.
    pub(super) fn remove(&mut self, seq: Seq) -> bool {
        match self.0.binary_search(&seq) {
            Ok(at) => {
                unique(&mut self.0).remove(at);
                true
            }
            Err(_) => false,
        }
    }

    #[cfg(test)]
    pub(super) fn shares_storage(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Carried seqs in ascending order, each with the session it came from. The provenance lives in
/// the cell, so discarding the cell discards it too.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct Carried {
    seqs: Seqs,
    sources: Arc<Vec<SessionRef>>,
}

impl Carried {
    pub(super) fn push(&mut self, seq: Seq, source: SessionRef) {
        self.seqs.push(seq);
        unique(&mut self.sources).push(source);
    }

    /// Adds `seq` at its place unless it is here.
    pub(super) fn insert(&mut self, seq: Seq, source: SessionRef) {
        if let Err(at) = self.seqs.0.binary_search(&seq) {
            unique(&mut self.seqs.0).insert(at, seq);
            unique(&mut self.sources).insert(at, source);
        }
    }

    /// Removes `seq`, returning the session it came from.
    pub(super) fn remove(&mut self, seq: Seq) -> Option<SessionRef> {
        let at = self.seqs.0.binary_search(&seq).ok()?;
        unique(&mut self.seqs.0).remove(at);
        Some(unique(&mut self.sources).remove(at))
    }

    pub(super) fn source(&self, seq: Seq) -> Option<&SessionRef> {
        let at = self.seqs.0.binary_search(&seq).ok()?;
        self.sources.get(at)
    }

    pub(super) fn last(&self) -> Option<Seq> {
        self.seqs.last()
    }

    /// The seqs above `floor`.
    pub(super) fn after(&self, floor: Option<Seq>) -> &[Seq] {
        self.seqs.after(floor)
    }
}

/// The failures of one class at one profile, under one named core for multi-core R7.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct FailureCell {
    pub(super) named: Option<CoreId>,
    pub(super) profile: Profile,
    pub(super) live: Seqs,
    pub(super) carried: Carried,
}

/// The passes of one class at one profile under one backend package and named core.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PassCell {
    pub(super) backend: BackendId,
    pub(super) named: Option<CoreId>,
    pub(super) profile: Profile,
    pub(super) live: Seqs,
    pub(super) live_non_alone: Seqs,
    pub(super) carried: Carried,
}

pub(super) trait Cell {
    type Key: Copy + Eq + std::hash::Hash;
    fn key(&self) -> Self::Key;
    fn named(&self) -> Option<CoreId>;
}

impl Cell for FailureCell {
    type Key = (Option<CoreId>, Profile);
    fn key(&self) -> Self::Key {
        (self.named, self.profile)
    }
    fn named(&self) -> Option<CoreId> {
        self.named
    }
}

impl Cell for PassCell {
    type Key = (BackendId, Option<CoreId>, Profile);
    fn key(&self) -> Self::Key {
        (self.backend, self.named, self.profile)
    }
    fn named(&self) -> Option<CoreId> {
        self.named
    }
}

/// Cells in the order they were first filled, with a point lookup by key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Cells<C: Cell> {
    cells: Vec<C>,
    index: HashMap<C::Key, usize>,
}

impl<C: Cell> Default for Cells<C> {
    fn default() -> Self {
        Self {
            cells: Vec::new(),
            index: HashMap::new(),
        }
    }
}

impl<C: Cell> Cells<C> {
    fn cell_with(&mut self, key: C::Key, make: impl FnOnce() -> C) -> &mut C {
        let at = *self.index.entry(key).or_insert_with(|| {
            self.cells.push(make());
            self.cells.len() - 1
        });
        &mut self.cells[at]
    }

    pub(super) fn cell_at_mut(&mut self, key: C::Key) -> Option<&mut C> {
        let at = *self.index.get(&key)?;
        self.cells.get_mut(at)
    }

    pub(super) fn cells(&self) -> &[C] {
        &self.cells
    }

    /// Drops the cells naming `core`, with the carried provenance they hold.
    pub(super) fn remove_named(&mut self, core: CoreId) {
        self.cells.retain(|cell| cell.named() != Some(core));
        self.index = self
            .cells
            .iter()
            .enumerate()
            .map(|(at, cell)| (cell.key(), at))
            .collect();
    }
}

pub(super) type FailureCells = Cells<FailureCell>;
pub(super) type PassCells = Cells<PassCell>;

impl FailureCells {
    pub(super) fn cell_mut(&mut self, named: Option<CoreId>, profile: Profile) -> &mut FailureCell {
        self.cell_with((named, profile), || FailureCell {
            named,
            profile,
            live: Seqs::default(),
            carried: Carried::default(),
        })
    }
}

impl PassCells {
    pub(super) fn cell_mut(
        &mut self,
        backend: BackendId,
        named: Option<CoreId>,
        profile: Profile,
    ) -> &mut PassCell {
        self.cell_with((backend, named, profile), || PassCell {
            backend,
            named,
            profile,
            live: Seqs::default(),
            live_non_alone: Seqs::default(),
            carried: Carried::default(),
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct ClassCells {
    pub(super) failures: FailureCells,
    pub(super) passes: PassCells,
    pub(super) exposure: u32,
}
