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
        let Ok(length) = u64::try_from(shared.len()) else {
            unreachable!("togi supports at most a 64-bit address space");
        };
        COPIED.with(|copied| copied.set(copied.get() + length));
    }
    Arc::make_mut(shared)
}

/// The most seqs a tail holds while its head is shared, so a clone copies at most this many per
/// list. It exceeds the 1,000 trials the configuration lets a step require, so the passes of
/// one step fold into a shared cell without settling it.
pub(crate) const TAIL_CAP: usize = 1_024;

/// Seqs in ascending order; each lookup is a binary search. A clone shares the `shared` head
/// until a write reaches into it, and appends while it is shared go to the owned `tail`, which
/// holds only seqs above the head and at most `TAIL_CAP` of them: the append that would pass
/// the cap copies head and tail into a head nobody shares. Cloning a state therefore costs the
/// cells and their tails, never the seqs behind them.
#[derive(Debug, Default)]
pub(super) struct Seqs {
    shared: Arc<Vec<Seq>>,
    tail: Vec<Seq>,
}

impl Clone for Seqs {
    fn clone(&self) -> Self {
        #[cfg(test)]
        COPIED.with(|copied| copied.set(copied.get() + self.tail.len() as u64));
        Self {
            shared: Arc::clone(&self.shared),
            tail: self.tail.clone(),
        }
    }
}

/// The seqs above a floor: a slice of the shared head followed by a slice of the tail.
#[derive(Clone, Copy, Debug)]
pub(super) struct Run<'a> {
    head: &'a [Seq],
    tail: &'a [Seq],
}

impl Run<'_> {
    pub(super) fn len(&self) -> usize {
        self.head.len() + self.tail.len()
    }

    pub(super) fn get(&self, at: usize) -> Option<Seq> {
        match at.checked_sub(self.head.len()) {
            None => self.head.get(at),
            Some(at) => self.tail.get(at),
        }
        .copied()
    }

    pub(super) fn first(&self) -> Option<Seq> {
        self.get(0)
    }

    pub(super) fn before(self, before: Option<Seq>) -> Self {
        let Some(before) = before else {
            return self;
        };
        Self {
            head: &self.head[..self.head.partition_point(|&seq| seq < before)],
            tail: &self.tail[..self.tail.partition_point(|&seq| seq < before)],
        }
    }

    pub(super) fn last(&self) -> Option<Seq> {
        self.tail.last().or_else(|| self.head.last()).copied()
    }
}

impl PartialEq for Seqs {
    fn eq(&self, other: &Self) -> bool {
        self.shared
            .iter()
            .chain(&self.tail)
            .eq(other.shared.iter().chain(&other.tail))
    }
}

impl Eq for Seqs {}

impl Seqs {
    /// Folds the tail into the head once no clone shares the head.
    fn settle(&mut self) {
        if !self.tail.is_empty()
            && let Some(head) = Arc::get_mut(&mut self.shared)
        {
            head.append(&mut self.tail);
        }
    }

    pub(super) fn position(&self, seq: Seq) -> Result<usize, usize> {
        match self.shared.binary_search(&seq) {
            Err(at) if at == self.shared.len() => self
                .tail
                .binary_search(&seq)
                .map(|found| at + found)
                .map_err(|found| at + found),
            found => found,
        }
    }

    /// Settles the tail into a head of its own once it is full and a clone still shares the head.
    fn make_room(&mut self) {
        if self.tail.len() >= TAIL_CAP && Arc::strong_count(&self.shared) > 1 {
            unique(&mut self.shared).append(&mut self.tail);
        }
    }

    pub(super) fn push(&mut self, seq: Seq) {
        self.make_room();
        self.push_unbounded(seq);
    }

    /// Appends without settling a full tail, for a simulation that appends a bounded number
    /// of seqs to a state it discards.
    pub(super) fn push_unbounded(&mut self, seq: Seq) {
        debug_assert!(self.last().is_none_or(|last| last < seq));
        self.settle();
        match Arc::get_mut(&mut self.shared) {
            Some(head) => head.push(seq),
            None => self.tail.push(seq),
        }
    }

    /// Adds `seq` at its place, returning where it went unless it was here. A seq that lands
    /// above everything the head holds goes to the tail; one that lands inside the head is
    /// among the few recorded after it only when the head is not shared.
    pub(super) fn insert(&mut self, seq: Seq) -> Option<usize> {
        self.settle();
        self.make_room();
        let at = self.position(seq).err()?;
        match at.checked_sub(self.shared.len()) {
            Some(in_tail) => self.tail.insert(in_tail, seq),
            None => unique(&mut self.shared).insert(at, seq),
        }
        Some(at)
    }

    pub(super) fn last(&self) -> Option<Seq> {
        self.tail.last().or_else(|| self.shared.last()).copied()
    }

    /// The seqs above `floor`.
    pub(super) fn after(&self, floor: Option<Seq>) -> Run<'_> {
        let start = self.shared.partition_point(|&seq| Some(seq) <= floor);
        if start < self.shared.len() {
            return Run {
                head: &self.shared[start..],
                tail: &self.tail,
            };
        }
        let start = self.tail.partition_point(|&seq| Some(seq) <= floor);
        Run {
            head: &[],
            tail: &self.tail[start..],
        }
    }

    /// Removes `seq`, returning where it was.
    pub(super) fn remove(&mut self, seq: Seq) -> Option<usize> {
        let at = self.position(seq).ok()?;
        match at.checked_sub(self.shared.len()) {
            Some(in_tail) => self.tail.remove(in_tail),
            None => unique(&mut self.shared).remove(at),
        };
        Some(at)
    }

    #[cfg(test)]
    pub(super) fn shares_storage(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
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
        if let Some(at) = self.seqs.insert(seq) {
            unique(&mut self.sources).insert(at, source);
        }
    }

    /// Removes `seq`, returning the session it came from.
    pub(super) fn remove(&mut self, seq: Seq) -> Option<SessionRef> {
        let at = self.seqs.remove(seq)?;
        Some(unique(&mut self.sources).remove(at))
    }

    pub(super) fn source(&self, seq: Seq) -> Option<&SessionRef> {
        let at = self.seqs.position(seq).ok()?;
        self.sources.get(at)
    }

    /// The seqs above `floor`.
    pub(super) fn after(&self, floor: Option<Seq>) -> Run<'_> {
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

/// A class's cells, the exposure its live passes add, and `reach`: per core, the deepest offset of
/// any of its pass cells, so that no pass cell covers a profile `reach` does not cover.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct ClassCells {
    pub(super) failures: FailureCells,
    pub(super) passes: PassCells,
    pub(super) exposure: u32,
    pub(super) reach: Option<Profile>,
}

impl ClassCells {
    pub(super) fn reach_of(passes: &PassCells) -> Option<Profile> {
        passes
            .cells()
            .iter()
            .map(|cell| cell.profile)
            .reduce(|reach, profile| reach.deeper(&profile))
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[derive(Clone, Debug)]
    enum Op {
        Push,
        Insert(u32),
        Remove(u32),
        Fork,
        Drop,
    }

    fn ops() -> impl Strategy<Value = Vec<Op>> {
        proptest::collection::vec(
            prop_oneof![
                4 => Just(Op::Push),
                2 => (0_u32..40).prop_map(Op::Insert),
                2 => (0_u32..40).prop_map(Op::Remove),
                2 => Just(Op::Fork),
                1 => Just(Op::Drop),
            ],
            1..80,
        )
    }

    fn s(value: u32) -> Seq {
        Seq::new(u64::from(value) + 1).unwrap()
    }

    fn all(seqs: &Seqs) -> Vec<Seq> {
        let run = seqs.after(None);
        (0..run.len()).filter_map(|at| run.get(at)).collect()
    }

    proptest! {
        #![proptest_config(crate::prop_tests::seeded(256))]

        #[test]
        fn seqs_behave_like_an_ordered_set_across_clones(script in ops(), floor in 0_u32..60) {
            let mut live = vec![(Seqs::default(), std::collections::BTreeSet::new())];
            for op in script {
                let at = live.len() - 1;
                match op {
                    Op::Push => {
                        let (seqs, model) = &mut live[at];
                        let next = model.last().map_or(0, |&last| last + 1);
                        seqs.push(s(next));
                        model.insert(next);
                    }
                    Op::Insert(raw) => {
                        let (seqs, model) = &mut live[at];
                        let placed = seqs.insert(s(raw));
                        prop_assert_eq!(placed.is_some(), model.insert(raw));
                        if let Some(index) = placed {
                            prop_assert_eq!(model.iter().position(|&v| v == raw), Some(index));
                        }
                    }
                    Op::Remove(raw) => {
                        let (seqs, model) = &mut live[at];
                        let removed = seqs.remove(s(raw));
                        let index = model.iter().position(|&v| v == raw);
                        prop_assert_eq!(removed, index);
                        model.remove(&raw);
                    }
                    Op::Fork => live.push(live[at].clone()),
                    Op::Drop if live.len() > 1 => {
                        live.remove(0);
                    }
                    Op::Drop => {}
                }
                for (seqs, model) in &live {
                    let want: Vec<Seq> = model.iter().map(|&v| s(v)).collect();
                    prop_assert_eq!(all(seqs), want.clone());
                    prop_assert_eq!(seqs.last(), want.last().copied());
                    let above: Vec<Seq> = model
                        .iter()
                        .filter(|&&v| v > floor)
                        .map(|&v| s(v))
                        .collect();
                    let run = seqs.after(Some(s(floor)));
                    prop_assert_eq!(run.len(), above.len());
                    prop_assert_eq!(run.first(), above.first().copied());
                }
                let (tip, tip_model) = live.last().unwrap();
                let mut rebuilt = Seqs::default();
                for &v in tip_model {
                    rebuilt.push(s(v));
                }
                prop_assert!(tip == &rebuilt);
            }
        }
    }

    #[test]
    fn appending_to_a_shared_list_copies_none_of_it() {
        let mut seqs = Seqs::default();
        for raw in 1..=1_000 {
            seqs.push(s(raw));
        }
        let twin = seqs.clone();
        take_copied();
        for raw in 1_001..=1_010 {
            seqs.push(s(raw));
        }
        assert_eq!(take_copied(), 0);
        assert!(seqs.shares_storage(&twin));
        assert_eq!(twin.last(), Some(s(1_000)));
        assert_eq!(seqs.last(), Some(s(1_010)));
        seqs.insert(s(0));
        assert_eq!(take_copied(), 1_000);
        assert!(!seqs.shares_storage(&twin));
    }
}

#[cfg(test)]
mod cap_tests {
    use super::*;

    fn s(value: u64) -> Seq {
        Seq::new(value).unwrap()
    }

    #[test]
    fn a_retained_clone_makes_the_tail_settle_once_it_is_full() {
        let mut seqs = Seqs::default();
        for raw in 1..=500 {
            seqs.push(s(raw));
        }
        let retained = seqs.clone();
        take_copied();
        let appended = 3 * TAIL_CAP as u64;
        for raw in 501..501 + appended {
            seqs.push(s(raw));
            assert!(seqs.tail.len() <= TAIL_CAP);
        }
        assert_eq!(take_copied(), 500);
        assert_eq!(retained.last(), Some(s(500)));
        assert_eq!(seqs.last(), Some(s(500 + appended)));
        assert!(!seqs.shares_storage(&retained));
        take_copied();
        let twin = seqs.clone();
        assert!(twin.tail.len() <= TAIL_CAP);
        assert_eq!(take_copied(), twin.tail.len() as u64);
        assert_eq!(twin, seqs);
    }

    #[test]
    fn a_simulation_may_outgrow_the_cap_without_settling() {
        let mut seqs = Seqs::default();
        for raw in 1..=500 {
            seqs.push(s(raw));
        }
        let retained = seqs.clone();
        take_copied();
        for raw in 501..501 + 2 * TAIL_CAP as u64 {
            seqs.push_unbounded(s(raw));
        }
        assert_eq!(take_copied(), 0);
        assert!(seqs.shares_storage(&retained));
    }
}
