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
use togi_machine::{Backend, Condition, CoreId, CoreSet, PerCore, Regime, WorkloadId};

use crate::{ClassKey, Depth, LoadKey, Profile, Secs, TopologyMismatch};

pub use backend::BackendId;
use backend::Backends;
pub(crate) use cells::unique;
use cells::{ClassCells, FailureCells, Run, Seqs};
#[cfg(test)]
pub(crate) use cells::{TAIL_CAP, take_copied};

/// The session a carried fact came from.
#[derive(
    Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
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
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Window {
    /// Live passes above `since`; carried passes do not count.
    Live { since: Option<Seq> },
    /// Live passes above `since`, and carried passes from any seq.
    LiveOrCarried { since: Option<Seq> },
}

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Evidence {
    all: CoreSet,
    #[serde(with = "togi_machine::serde_pairs")]
    classes: BTreeMap<ClassKey, ClassCells>,
    idle: FailureCells,
    all_core_r6: Vec<ClassKey>,
    by_core: PerCore<Option<Vec<ClassKey>>>,
    named: PerCore<Option<BTreeSet<ClassKey>>>,
    backends: Backends,
    checking: Option<Profile>,
    exposure: BTreeMap<WorkloadId, u32>,
    last: Option<Seq>,
    speculative: bool,
    #[cfg(test)]
    #[serde(skip)]
    visits: Visits,
}

fn merge(runs: &[Run<'_>], needed: usize) -> Vec<Seq> {
    let mut heads = vec![0_usize; runs.len()];
    let mut out = Vec::with_capacity(needed);
    while out.len() < needed {
        let next = runs
            .iter()
            .zip(&heads)
            .enumerate()
            .filter_map(|(at, (run, &head))| run.get(head).map(|seq| (seq, at)))
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
            by_core: PerCore::default(),
            named: PerCore::default(),
            backends: Backends::new(),
            checking: None,
            exposure: BTreeMap::new(),
            last: None,
            speculative: false,
            #[cfg(test)]
            visits: Visits::default(),
        }
    }

    /// Marks this ledger a simulation's own copy: it takes the few passes the simulation folds
    /// into shared cells without settling their tails, so a simulation never copies a history.
    pub(crate) fn speculate(&mut self) {
        self.speculative = true;
    }

    fn append(speculative: bool, seqs: &mut Seqs, seq: Seq) {
        if speculative {
            seqs.push_unbounded(seq);
        } else {
            seqs.push(seq);
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
    /// counting until the package returns. Whether the active package changed.
    pub fn set_backend(&mut self, backend: Backend, path: Option<&Path>) -> bool {
        let before = self.backends.active(backend);
        self.backends.set(backend, path);
        let changed = self.backends.active(backend) != before;
        if changed {
            self.recount_exposure(|key| key.workload().backend() == backend);
        }
        changed
    }

    pub fn set_checking_profile(&mut self, profile: Profile) -> Result<(), TopologyMismatch> {
        self.check(&profile)?;
        self.checking = Some(profile);
        self.recount_exposure(|_| true);
        Ok(())
    }

    /// Whether any pass cell of `class` can cover `profile`: when none can, the class has no
    /// valid pass there, and no cell is looked into to say so.
    pub(crate) fn may_cover(&self, class: ClassKey, profile: &Profile) -> bool {
        self.classes
            .get(&class)
            .and_then(|cells| cells.reach)
            .is_some_and(|reach| reach.covers(profile))
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
                self.by_core.get_or_insert_with(core, Vec::new).push(key);
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
            self.named
                .get_or_insert_with(core, BTreeSet::new)
                .insert(class);
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
        let speculative = self.speculative;
        let class = self.classes.entry(obs.class).or_default();
        let known = class.passes.cells().len();
        let cell = class.passes.cell_mut(backend, named, *obs.profile);
        if let Some(source) = source {
            cell.carried.push(obs.seq, source);
        } else {
            Self::append(speculative, &mut cell.live, obs.seq);
            if obs.condition != Condition::Alone {
                Self::append(speculative, &mut cell.live_non_alone, obs.seq);
            }
            if counts_exposure {
                class.exposure += 1;
                *self.exposure.entry(obs.class.workload()).or_default() += 1;
            }
        }
        if class.passes.cells().len() > known {
            class.reach = Some(
                class
                    .reach
                    .map_or(*obs.profile, |reach| reach.deeper(obs.profile)),
            );
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
        let speculative = self.speculative;
        let class = self.classes.entry(obs.class).or_default();
        let cell = class.failures.cell_mut(named, *obs.profile);
        if let Some(source) = source {
            cell.carried.push(obs.seq, source);
        } else {
            Self::append(speculative, &mut cell.live, obs.seq);
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
        let live = from.live.remove(seq).is_some();
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
        let speculative = self.speculative;
        let cell = self.idle.cell_mut(None, *profile);
        if let Some(source) = source {
            cell.carried.push(seq, source);
        } else {
            Self::append(speculative, &mut cell.live, seq);
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
        before: Option<Seq>,
    ) -> Option<Seq> {
        let mut newest = None;
        for cell in cells.cells() {
            self.visit();
            if !cell.profile.within(profile) {
                continue;
            }
            let live = cell.live.after(since).before(before).last();
            newest = newest
                .max(live)
                .max(cell.carried.after(None).before(before).last());
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
        self.newest_before(class, profile, since, None)
    }

    fn newest_before(
        &self,
        class: ClassKey,
        profile: &Profile,
        since: Option<Seq>,
        before: Option<Seq>,
    ) -> Option<Seq> {
        let own = self
            .classes
            .get(&class)
            .and_then(|cells| self.newest_in(&cells.failures, profile, since, before));
        let idle = if self.is_all_core_r6(class) {
            self.newest_in(&self.idle, profile, since, before)
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
        Ok(crate::numbers::saturating_evidence_count(own + idle))
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

    /// The admitted live and carried passes strictly before `before`, excluding failures at
    /// or after it. Backend identity and resets are those of this ledger: fold only the
    /// historical prefix when querying an earlier configuration or reset state.
    pub fn passes_before(
        &self,
        class: ClassKey,
        profile: &Profile,
        before: Seq,
    ) -> Result<usize, TopologyMismatch> {
        self.check(profile)?;
        Ok(self
            .admitted_passes(
                class,
                profile,
                Window::LiveOrCarried { since: None },
                Some(before),
            )
            .map(|run| run.len())
            .sum())
    }

    fn admitted_passes<'a>(
        &'a self,
        class: ClassKey,
        profile: &'a Profile,
        window: Window,
        before: Option<Seq>,
    ) -> impl Iterator<Item = Run<'a>> {
        let floor = self.newest_before(class, profile, None, before);
        let (since, carried_counts) = match window {
            Window::Live { since } => (since, false),
            Window::LiveOrCarried { since } => (since, true),
        };
        let live_floor = floor.max(since);
        let backend = self.backends.active(class.workload().backend());
        self.classes
            .get(&class)
            .into_iter()
            .flat_map(|cells| cells.passes.cells())
            .filter(move |cell| {
                self.visit();
                cell.backend == backend && cell.profile.covers(profile)
            })
            .flat_map(move |cell| {
                [
                    Some(cell.live.after(live_floor).before(before)),
                    carried_counts.then(|| cell.carried.after(floor).before(before)),
                ]
                .into_iter()
                .flatten()
            })
    }

    fn count_unchecked(
        &self,
        class: ClassKey,
        profile: &Profile,
        window: Window,
        needed: NonZeroU16,
    ) -> Count {
        let runs: Vec<Run<'_>> = self.admitted_passes(class, profile, window, None).collect();
        let valid = runs.iter().map(Run::len).sum();
        Count {
            valid: crate::numbers::saturating_evidence_count(valid),
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

    /// The passes showing that the loaded cores of `load` pass on their own at `profile`, a
    /// profile it failed at: valid passes of its classes, whatever the duration, under each
    /// class's active backend, at profiles where every loaded core is at least as deep as in
    /// `profile` and some unloaded core is shallower. A pass is valid while no failure of its
    /// class at an equal or shallower profile follows it. A pass at a profile `profile` is within
    /// shows nothing about the unloaded cores, so it never counts, whether or not the failure at
    /// `profile` is in the ledger yet. Taken as the failure folds, the count is a snapshot: the
    /// witnesses are the first valid passes, oldest first, up to [`togi_config::TrialCount::MAX`],
    /// so any supported trial count can later be checked against them without the ledger.
    pub(crate) fn loaded_cover(
        &self,
        load: LoadKey,
        profile: &Profile,
    ) -> Result<Count, TopologyMismatch> {
        self.check(profile)?;
        let first = ClassKey::new(load.workload, load.loaded, Secs::MIN);
        let mut runs: Vec<Run<'_>> = Vec::new();
        let mut valid = 0_usize;
        for (&class, cells) in self
            .classes
            .range(first..)
            .take_while(|(class, _)| class.load() == load)
        {
            let backend = self.backends.active(class.workload().backend());
            for cell in cells.passes.cells() {
                self.visit();
                let loaded_as_deep = load.loaded.iter().all(|core| {
                    match (cell.profile.get(core), profile.get(core)) {
                        (Some(pass), Some(failed)) => !pass.shallower_than(failed),
                        _ => false,
                    }
                });
                if cell.backend != backend || !loaded_as_deep || profile.within(&cell.profile) {
                    continue;
                }
                let floor = self.newest_unchecked(class, &cell.profile, None);
                let live = cell.live.after(floor);
                let carried = cell.carried.after(floor);
                valid += live.len() + carried.len();
                runs.push(live);
                runs.push(carried);
            }
        }
        let retained = valid.min(usize::from(togi_config::TrialCount::MAX));
        Ok(Count {
            valid: crate::numbers::saturating_evidence_count(valid),
            witnesses: merge(&runs, retained),
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
                    .is_some_and(|seq| seq < before)
                    || cell
                        .carried
                        .after(None)
                        .first()
                        .is_some_and(|seq| seq < before)
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
        for key in self
            .by_core
            .get_mut(core)
            .and_then(Option::take)
            .unwrap_or_default()
        {
            self.visit();
            self.classes.remove(&key);
            self.all_core_r6.retain(|&other| other != key);
            for other in key.loaded() {
                if let Some(keys) = self.by_core.get_mut(other).and_then(Option::as_mut) {
                    keys.retain(|&listed| listed != key);
                }
            }
        }
        for key in self
            .named
            .get_mut(core)
            .and_then(Option::take)
            .unwrap_or_default()
        {
            self.visit();
            let Some(cells) = self.classes.get_mut(&key) else {
                continue;
            };
            cells.failures.remove_named(core);
            cells.passes.remove_named(core);
            cells.reach = ClassCells::reach_of(&cells.passes);
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
        crate::numbers::saturating_evidence_count(total)
    }

    /// Every class's exposure, and the sum per workload, computed from the cells alone.
    fn exposure_from_scratch(&self) -> (BTreeMap<ClassKey, u32>, BTreeMap<WorkloadId, u32>) {
        let mut per_class = BTreeMap::new();
        let mut per_workload = BTreeMap::new();
        let Some(checking) = self.checking else {
            return (per_class, per_workload);
        };
        for (&key, cells) in &self.classes {
            let n = self.class_exposure(key, cells, &checking);
            per_class.insert(key, n);
            if n > 0 {
                *per_workload.entry(key.workload()).or_default() += n;
            }
        }
        (per_class, per_workload)
    }

    fn rebuild_exposure(&mut self) {
        let (per_class, per_workload) = self.exposure_from_scratch();
        for (key, class) in &mut self.classes {
            class.exposure = per_class.get(key).copied().unwrap_or(0);
        }
        self.exposure = per_workload;
    }

    /// Recounts the exposure of the classes `affects` names that can have any: a class whose
    /// passes cover neither the checking profile now nor, by its standing exposure, before has
    /// none, and is not looked into. The sums per workload move by what each class changed.
    fn recount_exposure(&mut self, affects: impl Fn(ClassKey) -> bool) {
        let Some(checking) = self.checking else {
            return;
        };
        let changed: Vec<(ClassKey, u32, u32)> = self
            .classes
            .iter()
            .filter(|&(&key, cells)| {
                affects(key)
                    && (cells.exposure > 0
                        || cells.reach.is_some_and(|reach| reach.covers(&checking)))
            })
            .filter_map(|(&key, cells)| {
                let now = self.class_exposure(key, cells, &checking);
                (now != cells.exposure).then_some((key, cells.exposure, now))
            })
            .collect();
        for (key, before, now) in changed {
            if let Some(class) = self.classes.get_mut(&key) {
                class.exposure = now;
            }
            if now > before {
                *self.exposure.entry(key.workload()).or_default() += now - before;
            } else {
                remove_exposure(&mut self.exposure, key.workload(), before - now);
            }
        }
    }

    /// The per-class and per-workload exposure as maintained, and as a rebuild from the cells
    /// gives it.
    #[cfg(test)]
    pub(crate) fn debug_rebuild(
        &self,
    ) -> [(BTreeMap<ClassKey, u32>, BTreeMap<WorkloadId, u32>); 2] {
        let maintained = (
            self.classes
                .iter()
                .filter(|(_, cells)| cells.exposure > 0)
                .map(|(&key, cells)| (key, cells.exposure))
                .collect(),
            self.exposure.clone(),
        );
        let (mut per_class, per_workload) = self.exposure_from_scratch();
        per_class.retain(|_, n| *n > 0);
        [maintained, (per_class, per_workload)]
    }
}
