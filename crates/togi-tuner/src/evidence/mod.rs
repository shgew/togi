//! The evidence ledger: which trials passed or failed at which profiles, what a pass supports and
//! a failure rules out, and how many valid passes a class has. Every query consults the distinct
//! cells of one class, never the events that filled them.

mod backend;
mod cells;

#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::num::NonZeroU16;
use std::path::Path;

use togi_journal::Seq;
use togi_machine::{Backend, Condition, CoreId, CoreSet, Regime, WorkloadId};

use crate::{ClassKey, Profile};

pub use backend::BackendId;
use backend::Backends;
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NamedFact {
    Pass(BackendId),
    Failure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Named {
    class: ClassKey,
    profile: Profile,
    seq: Seq,
    fact: NamedFact,
}

#[cfg(test)]
#[derive(Clone, Debug, Default)]
struct Visits(std::cell::Cell<u64>);

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
    named: BTreeMap<CoreId, Vec<Named>>,
    carried: BTreeMap<Seq, SessionRef>,
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

fn forget_carried(carried: &mut BTreeMap<Seq, SessionRef>, seqs: impl Iterator<Item = Seq>) {
    for seq in seqs {
        carried.remove(&seq);
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
            carried: BTreeMap::new(),
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
        self.visits.0.set(self.visits.0.get() + 1);
    }

    #[cfg(not(test))]
    #[expect(clippy::unused_self, reason = "the test build counts cell visits here")]
    fn visit(&self) {}

    #[cfg(test)]
    pub(crate) fn take_visits(&self) -> u64 {
        self.visits.0.replace(0)
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

    pub fn set_checking_profile(&mut self, profile: Profile) {
        self.checking = Some(profile);
        self.rebuild_exposure();
    }

    /// Live, non-alone passes of `workload` under the active backend that cover the checking
    /// profile since the latest failure ruling it out.
    pub fn exposure(&self, workload: WorkloadId) -> u32 {
        self.exposure.get(&workload).copied().unwrap_or(0)
    }

    pub fn exposures(&self) -> impl Iterator<Item = (WorkloadId, u32)> + '_ {
        self.exposure.iter().map(|(&workload, &n)| (workload, n))
    }

    pub fn carried_source(&self, seq: Seq) -> Option<&SessionRef> {
        self.carried.get(&seq)
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

    fn note_named(&mut self, obs: &Observation<'_>, fact: NamedFact) {
        if let Some(core) = obs.named.filter(|_| obs.class.multi_r7()) {
            self.named.entry(core).or_default().push(Named {
                class: obs.class,
                profile: *obs.profile,
                seq: obs.seq,
                fact,
            });
        }
    }

    /// Records a live pass under the backend package now in force.
    pub fn record_pass(&mut self, obs: Observation<'_>) {
        let backend = self.backends.active(obs.class.workload().backend());
        self.insert_pass(obs, backend, None);
    }

    /// Records a pass carried from `source`, run under `backend`.
    pub fn record_carried_pass(
        &mut self,
        obs: Observation<'_>,
        backend: BackendId,
        source: SessionRef,
    ) {
        self.insert_pass(obs, backend, Some(source));
    }

    fn insert_pass(
        &mut self,
        obs: Observation<'_>,
        backend: BackendId,
        source: Option<SessionRef>,
    ) {
        self.advance(obs.seq);
        self.register(obs.class);
        self.note_named(&obs, NamedFact::Pass(backend));
        let active = self.backends.active(obs.class.workload().backend());
        let counts_exposure = source.is_none()
            && obs.condition != Condition::Alone
            && backend == active
            && self
                .checking
                .is_some_and(|checking| obs.profile.covers(&checking));
        let class = self.classes.entry(obs.class).or_default();
        let cell = class.passes.cell_mut(backend, *obs.profile);
        if let Some(source) = source {
            cell.carried.push(obs.seq);
            self.carried.insert(obs.seq, source);
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
    }

    /// Records a live failure.
    pub fn record_failure(&mut self, obs: Observation<'_>) {
        self.insert_failure(obs, None);
    }

    pub fn record_carried_failure(&mut self, obs: Observation<'_>, source: SessionRef) {
        self.insert_failure(obs, Some(source));
    }

    fn insert_failure(&mut self, obs: Observation<'_>, source: Option<SessionRef>) {
        self.advance(obs.seq);
        self.register(obs.class);
        self.note_named(&obs, NamedFact::Failure);
        let rules_out_checking = self
            .checking
            .is_some_and(|checking| obs.profile.within(&checking));
        let class = self.classes.entry(obs.class).or_default();
        let cell = class.failures.cell_mut(*obs.profile);
        if let Some(source) = source {
            cell.carried.push(obs.seq);
            self.carried.insert(obs.seq, source);
        } else {
            cell.live.push(obs.seq);
        }
        if rules_out_checking {
            let dropped = std::mem::take(&mut class.exposure);
            remove_exposure(&mut self.exposure, obs.class.workload(), dropped);
        }
    }

    /// Records an idle failure at a full profile: it invalidates every all-core R6 class.
    pub fn record_idle_failure(&mut self, seq: Seq, profile: &Profile) {
        self.insert_idle(seq, profile, None);
    }

    pub fn record_carried_idle_failure(&mut self, seq: Seq, profile: &Profile, source: SessionRef) {
        self.insert_idle(seq, profile, Some(source));
    }

    fn insert_idle(&mut self, seq: Seq, profile: &Profile, source: Option<SessionRef>) {
        self.advance(seq);
        let cell = self.idle.cell_mut(*profile);
        if let Some(source) = source {
            cell.carried.push(seq);
            self.carried.insert(seq, source);
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
    }

    fn newest_in(
        &self,
        cells: &FailureCells,
        profile: &Profile,
        since: Option<Seq>,
    ) -> Option<Seq> {
        let mut newest = None;
        for cell in cells.iter() {
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

    /// The valid passes supporting `profile` in `window`, with the first `needed` of them.
    pub fn count(
        &self,
        class: ClassKey,
        profile: &Profile,
        window: Window,
        needed: NonZeroU16,
    ) -> Count {
        let Some(cells) = self.classes.get(&class) else {
            return Count::default();
        };
        let floor = self.newest_failure(class, profile, None);
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
    ) -> Option<Seq> {
        let covered = self
            .count(
                class,
                profile,
                Window::LiveOrCarried { since: None },
                needed,
            )
            .valid
            >= u32::from(needed.get());
        if covered {
            None
        } else {
            self.newest_failure(class, profile, since)
        }
    }

    /// Asked before a failure at `profile` is recorded: the first `needed` valid passes it
    /// contradicts. A multi-core R7 failure names a core instead and contradicts nothing.
    pub fn monotonicity(
        &self,
        class: ClassKey,
        profile: &Profile,
        needed: NonZeroU16,
    ) -> Option<Vec<Seq>> {
        if class.multi_r7() {
            return None;
        }
        let count = self.count(
            class,
            profile,
            Window::LiveOrCarried { since: None },
            needed,
        );
        (count.valid >= u32::from(needed.get())).then_some(count.witnesses)
    }

    /// Asked before an idle failure at `profile` is recorded: among the all-core R6 classes with
    /// `needed` valid passes it contradicts, the one whose first valid pass is earliest.
    pub fn idle_monotonicity(
        &self,
        profile: &Profile,
        needed: NonZeroU16,
    ) -> Option<IdleContradiction> {
        self.all_core_r6
            .iter()
            .filter_map(|&class| {
                let count = self.count(
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
            .min_by_key(|found| found.passes.first().copied())
    }

    /// Discards every fact whose loaded cores include `core`, every multi-core R7 fact naming
    /// it, and the carried provenance of what goes.
    pub fn reset_core(&mut self, core: CoreId) {
        for key in self.by_core.remove(&core).unwrap_or_default() {
            if let Some(cells) = self.classes.remove(&key) {
                for cell in cells.failures.cells() {
                    forget_carried(&mut self.carried, cell.carried.iter());
                }
                for cell in cells.passes.cells() {
                    forget_carried(&mut self.carried, cell.carried.iter());
                }
            }
            self.all_core_r6.retain(|&other| other != key);
            for other in key.loaded() {
                if let Some(keys) = self.by_core.get_mut(&other) {
                    keys.retain(|&listed| listed != key);
                }
            }
        }
        for named in self.named.remove(&core).unwrap_or_default() {
            self.remove_named(named);
        }
        for facts in self.named.values_mut() {
            facts.retain(|fact| self.classes.contains_key(&fact.class));
        }
        if self.all.contains(core) {
            for cell in std::mem::take(&mut self.idle).cells() {
                forget_carried(&mut self.carried, cell.carried.iter());
            }
        }
        self.rebuild_exposure();
    }

    fn remove_named(&mut self, named: Named) {
        let Some(class) = self.classes.get_mut(&named.class) else {
            return;
        };
        match named.fact {
            NamedFact::Pass(backend) => {
                if let Some(cell) = class.passes.cell_at_mut(backend, &named.profile) {
                    cell.live.remove(named.seq);
                    cell.live_non_alone.remove(named.seq);
                    cell.carried.remove(named.seq);
                }
            }
            NamedFact::Failure => {
                if let Some(cell) = class.failures.cell_at_mut(&named.profile) {
                    cell.live.remove(named.seq);
                    cell.carried.remove(named.seq);
                }
            }
        }
        self.carried.remove(&named.seq);
    }

    fn class_exposure(&self, key: ClassKey, cells: &ClassCells, checking: &Profile) -> u32 {
        let floor = self.newest_failure(key, checking, None);
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
