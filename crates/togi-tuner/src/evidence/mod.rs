//! The evidence ledger: which trials passed or failed at which profiles, what a pass supports and
//! a failure rules out, and how many valid passes a class has. Every query consults the distinct
//! cells of one class, never the events that filled them.

mod backend;
mod cells;

#[cfg(test)]
mod tests;

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU16;
use std::path::Path;

use togi_journal::Seq;
use togi_machine::{Backend, Condition, CoreId, CoreSet, Regime, WorkloadId};

use crate::{ClassKey, Profile, TopologyMismatch};

pub use backend::BackendId;
use backend::Backends;
#[cfg(test)]
pub(crate) use cells::take_copied;
pub(crate) use cells::unique;
use cells::{ClassCells, FailureCells};

/// The session a carried fact came from.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SessionRef(Box<str>);

impl SessionRef {
    pub fn new(session: impl Into<Box<str>>) -> Self {
        Self(session.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Which passes a count admits. Passes at or below the newest covering failure never count.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Window {
    /// Live passes above `since`; carried passes do not count.
    Live { since: Option<Seq> },
    /// Live passes above `since`, and carried passes from any seq.
    LiveOrCarried { since: Option<Seq> },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Count {
    pub valid: u32,
    /// The first `needed` valid passes, oldest first.
    pub witnesses: Vec<Seq>,
}

/// An idle failure contradicting the passes of one all-core R6 class.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdleContradiction {
    pub class: ClassKey,
    pub passes: Vec<Seq>,
}

/// One conclusive trial end: the intent's class and applied profile.
#[derive(Clone, Copy, Debug)]
pub struct Observation<'a> {
    pub seq: Seq,
    pub class: ClassKey,
    pub profile: &'a Profile,
    pub condition: Condition,
    /// The core a multi-core R7 trial names; reset discards the fact with that core.
    pub named: Option<CoreId>,
}

#[cfg(test)]
#[derive(Clone, Debug, Default)]
pub(crate) struct Visits(pub(crate) std::sync::Arc<std::sync::atomic::AtomicU64>);

#[cfg(test)]
impl PartialEq for Visits {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

#[cfg(test)]
impl Eq for Visits {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Evidence {
    all: CoreSet,
    classes: BTreeMap<ClassKey, ClassCells>,
    idle: FailureCells,
    all_core_r6: Vec<ClassKey>,
    by_core: BTreeMap<CoreId, Vec<ClassKey>>,
    named: BTreeMap<CoreId, BTreeSet<ClassKey>>,
    backends: Backends,
    checking: Option<Profile>,
    exposure: BTreeMap<WorkloadId, u32>,
    last: Option<Seq>,
    #[cfg(test)]
    visits: Visits,
}

fn merge(runs: &[&[Seq]], needed: usize) -> Vec<Seq> {
    let mut heads = vec![0_usize; runs.len()];
    let mut out = Vec::with_capacity(needed);
    while out.len() < needed {
        let next = runs
            .iter()
            .zip(&heads)
            .enumerate()
            .filter_map(|(at, (run, &head))| run.get(head).map(|&seq| (seq, at)))
            .min();
        let Some((seq, at)) = next else { break };
        out.push(seq);
        heads[at] += 1;
    }
    out
}

fn remove_exposure(exposure: &mut BTreeMap<WorkloadId, u32>, workload: WorkloadId, n: u32) {
    if let Entry::Occupied(mut slot) = exposure.entry(workload) {
        *slot.get_mut() = slot.get().saturating_sub(n);
        if *slot.get() == 0 {
            slot.remove();
        }
    }
}

impl Evidence {
    /// An empty ledger for a machine whose cores are `all`.
    pub fn new(all: CoreSet) -> Self {
        Self {
            all,
            classes: BTreeMap::new(),
            idle: FailureCells::default(),
            all_core_r6: Vec::new(),
            by_core: BTreeMap::new(),
            named: BTreeMap::new(),
            backends: Backends::new(),
            checking: None,
            exposure: BTreeMap::new(),
            last: None,
            #[cfg(test)]
            visits: Visits::default(),
        }
    }

    #[cfg(test)]
    fn visit(&self) {
        self.visits
            .0
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    #[cfg(not(test))]
    #[expect(clippy::unused_self, reason = "the test build counts cell visits here")]
    fn visit(&self) {}

    #[cfg(test)]
    pub(crate) fn take_visits(&self) -> u64 {
        self.visits.0.swap(0, std::sync::atomic::Ordering::Relaxed)
    }

    /// How many of this ledger's lists of seqs share their storage with the same list of `other`,
    /// and how many it has.
    #[cfg(test)]
    pub(crate) fn shared_with(&self, other: &Self) -> (usize, usize) {
        let mut shared = 0;
        let mut total = 0;
        for (cells, twin) in self
            .classes
            .iter()
            .filter_map(|(class, cells)| other.classes.get(class).map(|twin| (cells, twin)))
        {
            for (cell, other) in cells.passes.cells().iter().zip(twin.passes.cells()) {
                total += 2;
                shared += usize::from(cell.live.shares_storage(&other.live));
                shared += usize::from(cell.live_non_alone.shares_storage(&other.live_non_alone));
            }
            for (cell, other) in cells.failures.cells().iter().zip(twin.failures.cells()) {
                total += 1;
                shared += usize::from(cell.live.shares_storage(&other.live));
            }
        }
        (shared, total)
    }

    fn check(&self, profile: &Profile) -> Result<(), TopologyMismatch> {
        TopologyMismatch::check(self.all, profile.cores())
    }

    fn is_all_core_r6(&self, class: ClassKey) -> bool {
        class.regime() == Regime::R6 && class.loaded() == self.all
    }

    /// The id of a backend package, for a carried pass to cite the package it ran under.
    pub fn backend_id(&mut self, backend: Backend, path: Option<&Path>) -> BackendId {
        self.backends.intern(backend, path)
    }

    pub fn active_backend(&self, backend: Backend) -> BackendId {
        self.backends.active(backend)
    }

    /// Switches the package a backend runs from; passes recorded under other packages stop
    /// counting until the package returns.
    pub fn set_backend(&mut self, backend: Backend, path: Option<&Path>) {
        self.backends.set(backend, path);
        self.rebuild_exposure();
    }

    pub fn set_checking_profile(&mut self, profile: Profile) -> Result<(), TopologyMismatch> {
        self.check(&profile)?;
        self.checking = Some(profile);
        self.rebuild_exposure();
        Ok(())
    }

    /// Live, non-alone passes of `workload` under the active backend that cover the checking
    /// profile since the latest failure ruling it out.
    pub fn exposure(&self, workload: WorkloadId) -> u32 {
        self.exposure.get(&workload).copied().unwrap_or(0)
    }

    pub fn exposures(&self) -> impl Iterator<Item = (WorkloadId, u32)> + '_ {
        self.exposure.iter().map(|(&workload, &n)| (workload, n))
    }

    /// The session a carried fact came from, while the cell holding it stands. Looks through
    /// the distinct cells, not through facts.
    pub fn carried_source(&self, seq: Seq) -> Option<&SessionRef> {
        for cells in self.classes.values() {
            for cell in cells.failures.cells() {
                self.visit();
                if let Some(source) = cell.carried.source(seq) {
                    return Some(source);
                }
            }
            for cell in cells.passes.cells() {
                self.visit();
                if let Some(source) = cell.carried.source(seq) {
                    return Some(source);
                }
            }
        }
        for cell in self.idle.cells() {
            self.visit();
            if let Some(source) = cell.carried.source(seq) {
                return Some(source);
            }
        }
        None
    }

    fn advance(&mut self, seq: Seq) {
        debug_assert!(
            self.last.is_none_or(|last| last < seq),
            "evidence seqs must ascend"
        );
        self.last = Some(seq);
    }

    fn register(&mut self, key: ClassKey) {
        if let Entry::Vacant(slot) = self.classes.entry(key) {
            slot.insert(ClassCells::default());
            for core in key.loaded() {
                self.by_core.entry(core).or_default().push(key);
            }
            if self.is_all_core_r6(key) {
                self.all_core_r6.push(key);
            }
        }
    }

    fn named_of(obs: &Observation<'_>) -> Option<CoreId> {
        obs.named.filter(|_| obs.class.multi_r7())
    }

    fn note_named(&mut self, class: ClassKey, named: Option<CoreId>) {
        if let Some(core) = named {
            self.named.entry(core).or_default().insert(class);
        }
    }

    /// Records a live pass that ran under the backend package `backend`: the one in force when
    /// its trial began.
    pub fn record_pass(
        &mut self,
        obs: Observation<'_>,
        backend: BackendId,
    ) -> Result<(), TopologyMismatch> {
        self.insert_pass(obs, backend, None)
    }

    /// Records a pass carried from `source`, run under `backend`.
    pub fn record_carried_pass(
        &mut self,
        obs: Observation<'_>,
        backend: BackendId,
        source: SessionRef,
    ) -> Result<(), TopologyMismatch> {
        self.insert_pass(obs, backend, Some(source))
    }

    fn insert_pass(
        &mut self,
        obs: Observation<'_>,
        backend: BackendId,
        source: Option<SessionRef>,
    ) -> Result<(), TopologyMismatch> {
        self.check(obs.profile)?;
        self.advance(obs.seq);
        self.register(obs.class);
        let named = Self::named_of(&obs);
        self.note_named(obs.class, named);
        let active = self.backends.active(obs.class.workload().backend());
        let counts_exposure = source.is_none()
            && obs.condition != Condition::Alone
            && backend == active
            && self
                .checking
                .is_some_and(|checking| obs.profile.covers(&checking));
        let class = self.classes.entry(obs.class).or_default();
        let cell = class.passes.cell_mut(backend, named, *obs.profile);
        if let Some(source) = source {
            cell.carried.push(obs.seq, source);
        } else {
            cell.live.push(obs.seq);
            if obs.condition != Condition::Alone {
                cell.live_non_alone.push(obs.seq);
            }
            if counts_exposure {
                class.exposure += 1;
                *self.exposure.entry(obs.class.workload()).or_default() += 1;
            }
        }
        Ok(())
    }

    /// Records a live failure.
    pub fn record_failure(&mut self, obs: Observation<'_>) -> Result<(), TopologyMismatch> {
        self.insert_failure(obs, None)
    }

    pub fn record_carried_failure(
        &mut self,
        obs: Observation<'_>,
        source: SessionRef,
    ) -> Result<(), TopologyMismatch> {
        self.insert_failure(obs, Some(source))
    }

    fn insert_failure(
        &mut self,
        obs: Observation<'_>,
        source: Option<SessionRef>,
    ) -> Result<(), TopologyMismatch> {
        self.check(obs.profile)?;
        self.advance(obs.seq);
        self.register(obs.class);
        let named = Self::named_of(&obs);
        self.note_named(obs.class, named);
        let rules_out_checking = self
            .checking
            .is_some_and(|checking| obs.profile.within(&checking));
        let class = self.classes.entry(obs.class).or_default();
        let cell = class.failures.cell_mut(named, *obs.profile);
        if let Some(source) = source {
            cell.carried.push(obs.seq, source);
        } else {
            cell.live.push(obs.seq);
        }
        if rules_out_checking {
            let dropped = std::mem::take(&mut class.exposure);
            remove_exposure(&mut self.exposure, obs.class.workload(), dropped);
        }
        Ok(())
    }

    /// A multi-core R7 failure recorded at `seq` turned out to name `core`: the fact moves to the
    /// cells of that core, so that resetting it discards the fact. Facts of other classes, and
    /// facts already naming a core, are left as they are.
    pub fn name_failure(
        &mut self,
        class: ClassKey,
        profile: &Profile,
        seq: Seq,
        core: CoreId,
    ) -> Result<(), TopologyMismatch> {
        self.check(profile)?;
        if !class.multi_r7() {
            return Ok(());
        }
        let Some(cells) = self.classes.get_mut(&class) else {
            return Ok(());
        };
        let Some(from) = cells.failures.cell_at_mut((None, *profile)) else {
            return Ok(());
        };
        let live = from.live.remove(seq);
        let source = if live { None } else { from.carried.remove(seq) };
        if !live && source.is_none() {
            return Ok(());
        }
        let to = cells.failures.cell_mut(Some(core), *profile);
        if let Some(source) = source {
            to.carried.insert(seq, source);
        } else {
            to.live.insert(seq);
        }
        self.note_named(class, Some(core));
        Ok(())
    }

    /// Records an idle failure at a full profile: it invalidates every all-core R6 class.
    pub fn record_idle_failure(
        &mut self,
        seq: Seq,
        profile: &Profile,
    ) -> Result<(), TopologyMismatch> {
        self.insert_idle(seq, profile, None)
    }

    pub fn record_carried_idle_failure(
        &mut self,
        seq: Seq,
        profile: &Profile,
        source: SessionRef,
    ) -> Result<(), TopologyMismatch> {
        self.insert_idle(seq, profile, Some(source))
    }

    fn insert_idle(
        &mut self,
        seq: Seq,
        profile: &Profile,
        source: Option<SessionRef>,
    ) -> Result<(), TopologyMismatch> {
        self.check(profile)?;
        self.advance(seq);
        let cell = self.idle.cell_mut(None, *profile);
        if let Some(source) = source {
            cell.carried.push(seq, source);
        } else {
            cell.live.push(seq);
        }
        if self
            .checking
            .is_some_and(|checking| profile.within(&checking))
        {
            for key in &self.all_core_r6 {
                if let Some(class) = self.classes.get_mut(key) {
                    let dropped = std::mem::take(&mut class.exposure);
                    remove_exposure(&mut self.exposure, key.workload(), dropped);
                }
            }
        }
        Ok(())
    }

    fn newest_in(
        &self,
        cells: &FailureCells,
        profile: &Profile,
        since: Option<Seq>,
    ) -> Option<Seq> {
        let mut newest = None;
        for cell in cells.cells() {
            self.visit();
            if !cell.profile.within(profile) {
                continue;
            }
            let live = cell.live.last().filter(|&seq| Some(seq) > since);
            newest = newest.max(live).max(cell.carried.last());
        }
        newest
    }

    /// The newest failure that rules `profile` out and is admitted: carried from any seq, live
    /// above `since`. A newer live failure at or below `since` does not hide an older carried
    /// one. Idle failures count for all-core R6 classes.
    pub fn newest_failure(
        &self,
        class: ClassKey,
        profile: &Profile,
        since: Option<Seq>,
    ) -> Result<Option<Seq>, TopologyMismatch> {
        self.check(profile)?;
        Ok(self.newest_unchecked(class, profile, since))
    }

    fn newest_unchecked(
        &self,
        class: ClassKey,
        profile: &Profile,
        since: Option<Seq>,
    ) -> Option<Seq> {
        let own = self
            .classes
            .get(&class)
            .and_then(|cells| self.newest_in(&cells.failures, profile, since));
        let idle = if self.is_all_core_r6(class) {
            self.newest_in(&self.idle, profile, since)
        } else {
            None
        };
        own.max(idle)
    }

    fn live_in(&self, cells: &FailureCells, profile: &Profile, since: Option<Seq>) -> usize {
        cells
            .cells()
            .iter()
            .filter(|cell| {
                self.visit();
                cell.profile.within(profile)
            })
            .map(|cell| cell.live.after(since).len())
            .sum()
    }

    pub(crate) fn live_failures(
        &self,
        class: ClassKey,
        profile: &Profile,
        since: Option<Seq>,
    ) -> Result<u32, TopologyMismatch> {
        self.check(profile)?;
        let own = self
            .classes
            .get(&class)
            .map_or(0, |cells| self.live_in(&cells.failures, profile, since));
        let idle = if self.is_all_core_r6(class) {
            self.live_in(&self.idle, profile, since)
        } else {
            0
        };
        Ok(u32::try_from(own + idle).unwrap_or(u32::MAX))
    }

    /// The valid passes supporting `profile` in `window`, with the first `needed` of them.
    pub fn count(
        &self,
        class: ClassKey,
        profile: &Profile,
        window: Window,
        needed: NonZeroU16,
    ) -> Result<Count, TopologyMismatch> {
        self.check(profile)?;
        Ok(self.count_unchecked(class, profile, window, needed))
    }

    fn count_unchecked(
        &self,
        class: ClassKey,
        profile: &Profile,
        window: Window,
        needed: NonZeroU16,
    ) -> Count {
        let Some(cells) = self.classes.get(&class) else {
            return Count::default();
        };
        let floor = self.newest_unchecked(class, profile, None);
        let (since, carried_counts) = match window {
            Window::Live { since } => (since, false),
            Window::LiveOrCarried { since } => (since, true),
        };
        let live_floor = floor.max(since);
        let backend = self.backends.active(class.workload().backend());
        let mut runs: Vec<&[Seq]> = Vec::new();
        let mut valid = 0_usize;
        for cell in cells.passes.cells() {
            self.visit();
            if cell.backend != backend || !cell.profile.covers(profile) {
                continue;
            }
            let live = cell.live.after(live_floor);
            valid += live.len();
            runs.push(live);
            if carried_counts {
                let carried = cell.carried.after(floor);
                valid += carried.len();
                runs.push(carried);
            }
        }
        Count {
            valid: u32::try_from(valid).unwrap_or(u32::MAX),
            witnesses: merge(&runs, usize::from(needed.get())),
        }
    }

    /// The failure ruling `profile` out, unless `needed` newer passes cover it.
    pub fn failing_seq(
        &self,
        class: ClassKey,
        profile: &Profile,
        since: Option<Seq>,
        needed: NonZeroU16,
    ) -> Result<Option<Seq>, TopologyMismatch> {
        self.check(profile)?;
        let covered = self
            .count_unchecked(
                class,
                profile,
                Window::LiveOrCarried { since: None },
                needed,
            )
            .valid
            >= u32::from(needed.get());
        Ok(if covered {
            None
        } else {
            self.newest_unchecked(class, profile, since)
        })
    }

    /// Asked before a failure at `profile` is recorded: the first `needed` valid passes it
    /// contradicts. A multi-core R7 failure names a core instead and contradicts nothing.
    pub fn monotonicity(
        &self,
        class: ClassKey,
        profile: &Profile,
        needed: NonZeroU16,
    ) -> Result<Option<Vec<Seq>>, TopologyMismatch> {
        self.check(profile)?;
        if class.multi_r7() {
            return Ok(None);
        }
        let count = self.count_unchecked(
            class,
            profile,
            Window::LiveOrCarried { since: None },
            needed,
        );
        Ok((count.valid >= u32::from(needed.get())).then_some(count.witnesses))
    }

    /// Asked before an idle failure at `profile` is recorded: among the all-core R6 classes with
    /// `needed` valid passes it contradicts, the one whose first valid pass is earliest.
    pub fn idle_monotonicity(
        &self,
        profile: &Profile,
        needed: NonZeroU16,
    ) -> Result<Option<IdleContradiction>, TopologyMismatch> {
        self.check(profile)?;
        Ok(self
            .all_core_r6
            .iter()
            .filter_map(|&class| {
                let count = self.count_unchecked(
                    class,
                    profile,
                    Window::LiveOrCarried { since: None },
                    needed,
                );
                (count.valid >= u32::from(needed.get())).then_some(IdleContradiction {
                    class,
                    passes: count.witnesses,
                })
            })
            .min_by_key(|found| found.passes.first().copied()))
    }

    /// Whether some failure of `regime` at a duration of at most `longest` is admitted and was
    /// recorded before `before`: carried from any seq, live above `after`. Idle failures are
    /// all-core R6 failures at no duration.
    pub fn failed_before(
        &self,
        regime: Regime,
        longest: crate::Secs,
        after: Option<Seq>,
        before: Seq,
    ) -> bool {
        let admitted = |cells: &FailureCells| {
            cells.cells().iter().any(|cell| {
                self.visit();
                cell.live
                    .after(after)
                    .first()
                    .is_some_and(|&seq| seq < before)
                    || cell
                        .carried
                        .after(None)
                        .first()
                        .is_some_and(|&seq| seq < before)
            })
        };
        (regime == Regime::R6 && admitted(&self.idle))
            || self
                .classes
                .iter()
                .filter(|(class, _)| class.regime() == regime && class.duration() <= longest)
                .any(|(_, cells)| admitted(&cells.failures))
    }

    /// Discards every fact whose loaded cores include `core`, every multi-core R7 fact naming
    /// it, and the carried provenance of what goes. Whole cells go; no fact is looked up one by
    /// one.
    pub fn reset_core(&mut self, core: CoreId) {
        for key in self.by_core.remove(&core).unwrap_or_default() {
            self.visit();
            self.classes.remove(&key);
            self.all_core_r6.retain(|&other| other != key);
            for other in key.loaded() {
                if let Some(keys) = self.by_core.get_mut(&other) {
                    keys.retain(|&listed| listed != key);
                }
            }
        }
        for key in self.named.remove(&core).unwrap_or_default() {
            self.visit();
            let Some(cells) = self.classes.get_mut(&key) else {
                continue;
            };
            cells.failures.remove_named(core);
            cells.passes.remove_named(core);
        }
        if self.all.contains(core) {
            self.idle = FailureCells::default();
        }
        self.rebuild_exposure();
    }

    fn class_exposure(&self, key: ClassKey, cells: &ClassCells, checking: &Profile) -> u32 {
        let floor = self.newest_unchecked(key, checking, None);
        let backend = self.backends.active(key.workload().backend());
        let mut total = 0_usize;
        for cell in cells.passes.cells() {
            self.visit();
            if cell.backend == backend && cell.profile.covers(checking) {
                total += cell.live_non_alone.after(floor).len();
            }
        }
        u32::try_from(total).unwrap_or(u32::MAX)
    }

    fn rebuild_exposure(&mut self) {
        self.exposure.clear();
        let Some(checking) = self.checking else {
            return;
        };
        let counts: Vec<(ClassKey, u32)> = self
            .classes
            .iter()
            .map(|(&key, cells)| (key, self.class_exposure(key, cells, &checking)))
            .collect();
        for (key, n) in counts {
            if let Some(class) = self.classes.get_mut(&key) {
                class.exposure = n;
            }
            if n > 0 {
                *self.exposure.entry(key.workload()).or_default() += n;
            }
        }
    }
}
