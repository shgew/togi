use std::collections::HashMap;

use togi_journal::Seq;
use togi_machine::CoreId;

use crate::Profile;

use super::BackendId;

/// Seqs in ascending order; each lookup is a binary search.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct Seqs(Vec<Seq>);

impl Seqs {
    pub(super) fn push(&mut self, seq: Seq) {
        debug_assert!(self.0.last().is_none_or(|&last| last < seq));
        self.0.push(seq);
    }

    /// Adds `seq` at its place; the seqs it moves past are the few recorded after it.
    pub(super) fn insert(&mut self, seq: Seq) {
        if let Err(at) = self.0.binary_search(&seq) {
            self.0.insert(at, seq);
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
                self.0.remove(at);
                true
            }
            Err(_) => false,
        }
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = Seq> + '_ {
        self.0.iter().copied()
    }
}

/// The failures of one class at one profile, under one named core for multi-core R7.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct FailureCell {
    pub(super) named: Option<CoreId>,
    pub(super) profile: Profile,
    pub(super) live: Seqs,
    pub(super) carried: Seqs,
}

/// The passes of one class at one profile under one backend package and named core.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PassCell {
    pub(super) backend: BackendId,
    pub(super) named: Option<CoreId>,
    pub(super) profile: Profile,
    pub(super) live: Seqs,
    pub(super) live_non_alone: Seqs,
    pub(super) carried: Seqs,
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

    /// Drops the cells naming `core`, returning them.
    pub(super) fn remove_named(&mut self, core: CoreId) -> Vec<C> {
        let (gone, kept): (Vec<C>, Vec<C>) = std::mem::take(&mut self.cells)
            .into_iter()
            .partition(|cell| cell.named() == Some(core));
        self.cells = kept;
        self.index = self
            .cells
            .iter()
            .enumerate()
            .map(|(at, cell)| (cell.key(), at))
            .collect();
        gone
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
            carried: Seqs::default(),
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
            carried: Seqs::default(),
        })
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct ClassCells {
    pub(super) failures: FailureCells,
    pub(super) passes: PassCells,
    pub(super) exposure: u32,
}
