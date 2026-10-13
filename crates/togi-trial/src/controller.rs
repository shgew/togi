//! Keeps togi's own threads off the cores a trial loads.
//!
//! The processes togi starts are confined by their scopes; togi's own threads (the session, the
//! journal, the hardware seams, the runtime and blocking pools, the dashboard) share one process,
//! so one affinity rule covers them all. [`select`] picks the CPUs they may use for one trial from
//! the offsets the machine holds, and [`Threads`] moves every thread of the process onto them and
//! back.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, PoisonError};

use rustix::io::Errno;
use rustix::process::Pid;
use rustix::thread::CpuSet;
use togi_machine::{CoreId, CoreInfo, CpuId, PerCore, TargetOffset};

/// The CPUs togi's own threads use while a trial loads `loaded` cores and `cpus`, or `None` when
/// the topology names no core.
///
/// A core is loaded when it is one of `loaded` or holds one of `cpus`; all of its logical CPUs
/// are then off limits while any unloaded core exists. The choice, in order:
///
/// 1. every unloaded core at offset 0;
/// 2. else the least-deep unloaded core;
/// 3. else, when every core is loaded, the least-deep loaded core.
///
/// A core whose offset is not in `offsets` counts as deeper than any known one. Ties go to the
/// lowest core id. Nothing here refuses a trial: reserving a core would change what an all-core
/// trial proves.
pub(crate) fn select(
    topology: &[CoreInfo],
    loaded: &[CoreId],
    cpus: &[CpuId],
    offsets: &PerCore<TargetOffset>,
) -> Option<Vec<CpuId>> {
    let is_loaded =
        |info: &CoreInfo| loaded.contains(&info.core) || info.cpus.iter().any(|c| cpus.contains(c));
    let (busy, idle): (Vec<&CoreInfo>, Vec<&CoreInfo>) =
        topology.iter().partition(|info| is_loaded(info));
    let at_zero: Vec<&CoreInfo> = idle
        .iter()
        .copied()
        .filter(|info| offsets.get(info.core).map(TargetOffset::get) == Some(0))
        .collect();
    let chosen = if at_zero.is_empty() {
        let least = least_deep(&idle, offsets).or_else(|| least_deep(&busy, offsets))?;
        vec![least]
    } else {
        at_zero
    };
    let mut selected: Vec<CpuId> = chosen
        .iter()
        .flat_map(|info| info.cpus.iter().copied())
        .collect();
    selected.sort_unstable_by_key(|cpu| cpu.0);
    selected.dedup();
    (!selected.is_empty()).then_some(selected)
}

/// The core among `cores` whose offset is the shallowest, the lowest core id among equals.
fn least_deep<'a>(cores: &[&'a CoreInfo], offsets: &PerCore<TargetOffset>) -> Option<&'a CoreInfo> {
    cores.iter().copied().min_by_key(|info| {
        let held = offsets.get(info.core).map_or(i64::MIN, TargetOffset::get);
        (Reverse(held), info.core)
    })
}

/// What a confinement or a restoration did to the process's threads.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Moved {
    /// Threads the last pass found.
    pub(crate) threads: usize,
    /// Threads of the last pass whose affinity could not be read or set, and thread-table entries
    /// that could not be listed; they keep what they had.
    pub(crate) failed: usize,
    /// The pass limit was reached while a pass was still moving threads, so threads created
    /// meanwhile may not have been examined.
    pub(crate) incomplete: bool,
}

impl Moved {
    /// Every thread found was read and moved, and the last pass found nothing left to do.
    pub(crate) const fn complete(self) -> bool {
        self.failed == 0 && !self.incomplete
    }

    /// The counts, for a progress line.
    pub(crate) fn describe(self) -> String {
        let mut parts = vec![format!("{} threads", self.threads)];
        if self.failed > 0 {
            parts.push(format!("{} could not be read or moved", self.failed));
        }
        if self.incomplete {
            parts.push(
                "new threads were still appearing when the pass limit was reached".to_owned(),
            );
        }
        parts.join(", ")
    }
}

/// Moves the threads of the process. A confinement never fails a trial: a thread that cannot be
/// moved is counted in what it returns.
pub(crate) trait Controller: Send + Sync {
    /// Confines every thread of the process to `cpus`, keeping what each had before for
    /// [`Controller::restore`]. `None` when this controller does nothing.
    fn confine(&self, cpus: &[CpuId]) -> Option<Moved>;
    /// Gives every thread the affinity it had before the first `confine` since the last complete
    /// `restore`; an incomplete one stays armed for the next. `None` when nothing was confined.
    fn restore(&self) -> Option<Moved>;
}

/// A controller that leaves every thread alone. Tests that share a process with other tests use
/// it; only a re-executed child confines.
#[cfg(test)]
pub(crate) struct Idle;

#[cfg(test)]
impl Controller for Idle {
    fn confine(&self, _: &[CpuId]) -> Option<Moved> {
        None
    }

    fn restore(&self) -> Option<Moved> {
        None
    }
}

/// A call a [`Recording`] controller saw.
#[cfg(test)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Call {
    Confine(Vec<CpuId>),
    Restore,
}

/// Records the calls it gets and moves nothing.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Recording {
    pub(crate) calls: Mutex<Vec<Call>>,
}

#[cfg(test)]
impl Recording {
    pub(crate) fn calls(&self) -> Vec<Call> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

#[cfg(test)]
impl Controller for Recording {
    fn confine(&self, cpus: &[CpuId]) -> Option<Moved> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Call::Confine(cpus.to_vec()));
        Some(Moved::default())
    }

    fn restore(&self) -> Option<Moved> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Call::Restore);
        Some(Moved::default())
    }
}

/// The passes over the thread table a move may take before it gives up on a process that keeps
/// creating threads faster than they can be moved.
const PASSES: usize = 32;

/// The thread table and the affinity calls of one process.
pub(crate) trait Tasks: Send + Sync {
    /// The ids of the threads, and how many entries of the table could not be read.
    fn tids(&self) -> (Vec<u32>, usize);
    /// The affinity of the thread group leader: what a thread created since gets back.
    fn leader(&self) -> Result<CpuSet, Errno>;
    fn affinity(&self, tid: u32) -> Result<CpuSet, Errno>;
    fn set_affinity(&self, tid: u32, cpus: &CpuSet) -> Result<(), Errno>;
}

/// This process's threads, through `/proc/self/task` and the scheduler calls.
pub(crate) struct Proc;

impl Tasks for Proc {
    fn tids(&self) -> (Vec<u32>, usize) {
        let Ok(entries) = std::fs::read_dir("/proc/self/task") else {
            return (Vec::new(), 1);
        };
        let mut unread = 0;
        let mut tids: Vec<u32> = entries
            .filter_map(|entry| {
                if let Ok(entry) = entry {
                    entry.file_name().to_str()?.parse().ok()
                } else {
                    unread += 1;
                    None
                }
            })
            .collect();
        tids.sort_unstable();
        (tids, unread)
    }

    fn leader(&self) -> Result<CpuSet, Errno> {
        rustix::thread::sched_getaffinity(Some(rustix::process::getpid()))
    }

    fn affinity(&self, tid: u32) -> Result<CpuSet, Errno> {
        rustix::thread::sched_getaffinity(Some(pid_of(tid)?))
    }

    fn set_affinity(&self, tid: u32, cpus: &CpuSet) -> Result<(), Errno> {
        rustix::thread::sched_setaffinity(Some(pid_of(tid)?), cpus)
    }
}

fn pid_of(tid: u32) -> Result<Pid, Errno> {
    i32::try_from(tid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or(Errno::SRCH)
}

/// What each thread could run on before the first confinement.
struct Saved {
    /// The thread group leader's affinity, `None` when it could not be read.
    leader: Option<CpuSet>,
    tasks: HashMap<u32, CpuSet>,
}

impl Saved {
    fn capture(tasks: &impl Tasks) -> Self {
        let found = tasks
            .tids()
            .0
            .into_iter()
            .filter_map(|tid| Some((tid, tasks.affinity(tid).ok()?)))
            .collect();
        Self {
            leader: tasks.leader().ok(),
            tasks: found,
        }
    }
}

/// Every thread of a process, confined to the CPUs it is told to use. Each thread is set to
/// exactly those CPUs, whatever it could run on before: the kernel keeps a cgroup's own bound, and
/// a process that was pinned elsewhere is still moved. Threads created later inherit the affinity
/// of the thread that creates them, and the passes repeat until none finds a thread to move, so a
/// thread created while a pass runs by a thread the pass has not reached yet is caught by the
/// next. What a pass could not read or set, and a pass limit reached while threads were still
/// being moved, are reported in what the call returns.
pub(crate) struct Threads<T: Tasks = Proc> {
    tasks: T,
    saved: Mutex<Option<Saved>>,
}

impl<T: Tasks> Threads<T> {
    pub(crate) fn over(tasks: T) -> Self {
        Self {
            tasks,
            saved: Mutex::new(None),
        }
    }
}

impl<T: Tasks> Controller for Threads<T> {
    fn confine(&self, cpus: &[CpuId]) -> Option<Moved> {
        let wanted = cpu_set(cpus);
        let mut guard = self.saved.lock().unwrap_or_else(PoisonError::into_inner);
        if guard.is_none() {
            *guard = Some(Saved::capture(&self.tasks));
        }
        Some(sweep(&self.tasks, |_, current| {
            if !is_empty(current) && intersection(current, &wanted) == *current {
                Verdict::Keep
            } else {
                Verdict::Move(wanted)
            }
        }))
    }

    fn restore(&self) -> Option<Moved> {
        let mut guard = self.saved.lock().unwrap_or_else(PoisonError::into_inner);
        let saved = guard.as_ref()?;
        let moved = sweep(&self.tasks, |tid, current| {
            match saved.tasks.get(&tid).copied().or(saved.leader) {
                Some(target) if target == *current => Verdict::Keep,
                Some(target) => Verdict::Move(target),
                None => Verdict::Move(CpuSet::new()),
            }
        });
        if moved.complete() {
            *guard = None;
        }
        Some(moved)
    }
}

/// What a pass does with a thread.
enum Verdict {
    Keep,
    Move(CpuSet),
}

/// Moves every thread as `decide` says until a pass moves none, or the pass limit is reached. A
/// thread that is gone is skipped; one whose affinity cannot be read or set is counted failed. A
/// thread moved once is not moved again, so a kernel that narrows a mask to its cgroup does not
/// keep a pass going.
fn sweep<T: Tasks>(tasks: &T, decide: impl Fn(u32, &CpuSet) -> Verdict) -> Moved {
    let mut settled = HashSet::new();
    let mut moved = Moved::default();
    for _ in 0..PASSES {
        let (tids, unlisted) = tasks.tids();
        moved = Moved {
            threads: 0,
            failed: unlisted,
            incomplete: false,
        };
        let mut changed = false;
        for tid in tids {
            let current = match tasks.affinity(tid) {
                Ok(current) => current,
                Err(Errno::SRCH) => continue,
                Err(_) => {
                    moved.threads += 1;
                    moved.failed += 1;
                    continue;
                }
            };
            moved.threads += 1;
            if settled.contains(&tid) {
                continue;
            }
            let Verdict::Move(target) = decide(tid, &current) else {
                continue;
            };
            if is_empty(&target) {
                moved.failed += 1;
                continue;
            }
            match tasks.set_affinity(tid, &target) {
                Ok(()) => {
                    settled.insert(tid);
                    changed = true;
                }
                Err(Errno::SRCH) => moved.threads -= 1,
                Err(_) => moved.failed += 1,
            }
        }
        if !changed {
            return moved;
        }
    }
    moved.incomplete = true;
    moved
}

fn cpu_set(cpus: &[CpuId]) -> CpuSet {
    let mut set = CpuSet::new();
    for cpu in cpus {
        let index = usize::from(cpu.0);
        if index < CpuSet::MAX_CPU {
            set.set(index);
        }
    }
    set
}

fn intersection(a: &CpuSet, b: &CpuSet) -> CpuSet {
    let mut both = CpuSet::new();
    for cpu in 0..CpuSet::MAX_CPU {
        if a.is_set(cpu) && b.is_set(cpu) {
            both.set(cpu);
        }
    }
    both
}

fn is_empty(set: &CpuSet) -> bool {
    set.count() == 0
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use togi_machine::CcdId;

    use super::*;

    fn core(n: u8) -> CoreId {
        CoreId::new(i64::from(n)).unwrap()
    }

    /// Four cores, core n on logical CPUs n and n + 4.
    fn topology() -> Vec<CoreInfo> {
        (0..4_u8)
            .map(|n| CoreInfo {
                core: core(n),
                ccd: CcdId(0),
                cpus: vec![CpuId(u16::from(n)), CpuId(u16::from(n) + 4)],
            })
            .collect()
    }

    /// An offset per core, in core order; `None` leaves a core unknown.
    fn held(values: &[Option<i64>]) -> PerCore<TargetOffset> {
        values
            .iter()
            .enumerate()
            .filter_map(|(n, value)| {
                let core = core(u8::try_from(n).unwrap());
                Some((core, TargetOffset::new((*value)?).unwrap()))
            })
            .collect()
    }

    fn cpus(list: &[u16]) -> Vec<CpuId> {
        list.iter().map(|&cpu| CpuId(cpu)).collect()
    }

    fn pick(loaded: &[u8], with: &[u16], offsets: &[Option<i64>]) -> Option<Vec<CpuId>> {
        let loaded: Vec<CoreId> = loaded.iter().map(|&n| core(n)).collect();
        select(&topology(), &loaded, &cpus(with), &held(offsets))
    }

    fn all(values: [i64; 4]) -> Vec<Option<i64>> {
        values.into_iter().map(Some).collect()
    }

    #[test]
    fn every_unloaded_core_at_zero_is_chosen_with_both_logical_cpus() {
        let chosen = pick(&[0], &[0], &all([-20, 0, -5, 0]));
        assert_eq!(chosen, Some(cpus(&[1, 3, 5, 7])));
    }

    #[test]
    fn a_loaded_core_at_zero_is_not_chosen_while_an_unloaded_one_exists() {
        let chosen = pick(&[0, 1], &[0, 1], &all([0, 0, -5, -9]));
        assert_eq!(chosen, Some(cpus(&[2, 6])));
    }

    #[test]
    fn the_whole_loaded_physical_core_is_excluded_not_just_the_workload_cpu() {
        let chosen = pick(&[], &[0], &all([0, -3, -3, -3])).unwrap();
        assert!(
            !chosen.contains(&CpuId(0)) && !chosen.contains(&CpuId(4)),
            "{chosen:?}"
        );
        let chosen = pick(&[], &[4], &all([0, -3, -3, -3])).unwrap();
        assert!(
            !chosen.contains(&CpuId(0)) && !chosen.contains(&CpuId(4)),
            "{chosen:?}"
        );
    }

    #[test]
    fn without_an_unloaded_zero_the_least_deep_unloaded_core_is_chosen() {
        let chosen = pick(&[0], &[0], &all([0, -10, -3, -7]));
        assert_eq!(chosen, Some(cpus(&[2, 6])));
    }

    #[test]
    fn a_tie_between_unloaded_cores_goes_to_the_lowest_core_id() {
        let chosen = pick(&[0], &[0], &all([0, -4, -4, -4]));
        assert_eq!(chosen, Some(cpus(&[1, 5])));
    }

    #[test]
    fn a_core_with_an_unknown_offset_counts_as_deeper_than_any_known_one() {
        let offsets = [Some(0), None, Some(-40), None];
        assert_eq!(pick(&[0], &[0], &offsets), Some(cpus(&[2, 6])));
        let offsets = [Some(0), None, None, None];
        assert_eq!(pick(&[0], &[0], &offsets), Some(cpus(&[1, 5])));
    }

    #[test]
    fn when_every_core_is_loaded_the_least_deep_loaded_core_is_chosen() {
        let chosen = pick(&[0, 1, 2, 3], &[0, 1, 2, 3], &all([-5, -2, -9, -2]));
        assert_eq!(chosen, Some(cpus(&[1, 5])));
    }

    #[test]
    fn an_all_core_trial_at_zero_falls_back_to_the_lowest_loaded_core() {
        let chosen = pick(&[0, 1, 2, 3], &[0, 1, 2, 3], &all([0, 0, 0, 0]));
        assert_eq!(chosen, Some(cpus(&[0, 4])));
    }

    #[test]
    fn a_core_holding_a_loaded_cpu_counts_as_loaded_without_being_listed() {
        let chosen = pick(&[], &[5], &all([-1, -1, -1, -1]));
        assert_eq!(chosen, Some(cpus(&[0, 4])));
    }

    #[test]
    fn a_topology_without_cores_selects_nothing() {
        assert_eq!(
            select(&[], &[core(0)], &cpus(&[0]), &PerCore::default()),
            None
        );
    }

    fn set(list: &[u16]) -> CpuSet {
        cpu_set(&cpus(list))
    }

    #[derive(Default)]
    struct Table {
        masks: std::collections::BTreeMap<u32, CpuSet>,
        unreadable: std::collections::BTreeSet<u32>,
        immovable: std::collections::BTreeSet<u32>,
        /// Every successful move makes another thread, as a creator the move has not reached yet.
        breeding: Option<CpuSet>,
        next: u32,
    }

    /// A process whose threads the test lists, so the controller's passes are exercised without
    /// touching the affinity of the test process.
    #[derive(Clone, Default)]
    struct Fake(Arc<Mutex<Table>>);

    impl Fake {
        fn with(threads: &[(u32, &[u16])]) -> Self {
            let fake = Self::default();
            for (tid, list) in threads {
                fake.add(*tid, list);
            }
            fake
        }

        fn add(&self, tid: u32, list: &[u16]) {
            let mut table = self.0.lock().unwrap();
            table.masks.insert(tid, set(list));
            table.next = table.next.max(tid);
        }

        fn mask(&self, tid: u32) -> CpuSet {
            self.0.lock().unwrap().masks[&tid]
        }

        fn count(&self) -> usize {
            self.0.lock().unwrap().masks.len()
        }
    }

    impl Tasks for Fake {
        fn tids(&self) -> (Vec<u32>, usize) {
            (self.0.lock().unwrap().masks.keys().copied().collect(), 0)
        }

        fn leader(&self) -> Result<CpuSet, Errno> {
            self.0
                .lock()
                .unwrap()
                .masks
                .get(&1)
                .copied()
                .ok_or(Errno::SRCH)
        }

        fn affinity(&self, tid: u32) -> Result<CpuSet, Errno> {
            let table = self.0.lock().unwrap();
            if table.unreadable.contains(&tid) {
                return Err(Errno::IO);
            }
            table.masks.get(&tid).copied().ok_or(Errno::SRCH)
        }

        fn set_affinity(&self, tid: u32, mask: &CpuSet) -> Result<(), Errno> {
            let mut table = self.0.lock().unwrap();
            if table.immovable.contains(&tid) {
                return Err(Errno::PERM);
            }
            table.masks.insert(tid, *mask);
            if let Some(fresh) = table.breeding {
                table.next += 1;
                let next = table.next;
                table.masks.insert(next, fresh);
            }
            Ok(())
        }
    }

    #[test]
    fn a_process_pinned_to_a_loaded_cpu_is_still_moved_to_the_selected_cpus_and_back() {
        let fake = Fake::with(&[(1, &[0]), (2, &[0])]);
        let threads = Threads::over(fake.clone());
        let moved = threads.confine(&cpus(&[2, 3])).unwrap();
        assert_eq!(
            (moved.threads, moved.failed, moved.incomplete),
            (2, 0, false)
        );
        assert!(fake.mask(1) == set(&[2, 3]) && fake.mask(2) == set(&[2, 3]));
        let moved = threads.restore().unwrap();
        assert!(moved.complete());
        assert!(fake.mask(1) == set(&[0]) && fake.mask(2) == set(&[0]));
        assert_eq!(threads.restore(), None, "nothing is left to restore");
    }

    #[test]
    fn a_thread_created_during_the_trial_gets_the_leaders_mask_back() {
        let fake = Fake::with(&[(1, &[0, 1]), (2, &[0, 1])]);
        let threads = Threads::over(fake.clone());
        threads.confine(&cpus(&[1])).unwrap();
        fake.add(5, &[1]);
        let moved = threads.restore().unwrap();
        assert!(moved.complete());
        assert!(fake.mask(5) == set(&[0, 1]) && fake.mask(2) == set(&[0, 1]));
    }

    #[test]
    fn an_unreadable_thread_is_reported_failed_and_the_restore_stays_armed_until_it_is_complete() {
        let fake = Fake::with(&[(1, &[0, 1]), (2, &[0, 1])]);
        fake.0.lock().unwrap().unreadable.insert(2);
        let threads = Threads::over(fake.clone());
        let moved = threads.confine(&cpus(&[1])).unwrap();
        assert_eq!((moved.threads, moved.failed), (2, 1));
        assert!(!moved.complete());
        assert!(fake.mask(1) == set(&[1]));
        let moved = threads.restore().unwrap();
        assert!(!moved.complete(), "{moved:?}");
        fake.0.lock().unwrap().unreadable.clear();
        let moved = threads.restore().unwrap();
        assert!(moved.complete(), "{moved:?}");
        assert!(fake.mask(1) == set(&[0, 1]));
    }

    #[test]
    fn a_thread_that_refuses_the_move_is_counted_failed() {
        let fake = Fake::with(&[(1, &[0, 1]), (2, &[0, 1])]);
        fake.0.lock().unwrap().immovable.insert(2);
        let moved = Threads::over(fake.clone()).confine(&cpus(&[1])).unwrap();
        assert_eq!(
            (moved.threads, moved.failed, moved.incomplete),
            (2, 1, false)
        );
        assert!(fake.mask(2) == set(&[0, 1]));
    }

    #[test]
    fn threads_that_keep_appearing_past_the_pass_limit_are_reported_incomplete() {
        let fake = Fake::with(&[(1, &[0, 1])]);
        fake.0.lock().unwrap().breeding = Some(set(&[0, 1]));
        let moved = Threads::over(fake.clone()).confine(&cpus(&[1])).unwrap();
        assert!(moved.incomplete && !moved.complete(), "{moved:?}");
        assert!(fake.count() > 2);
    }

    #[test]
    fn a_stable_process_takes_two_passes_and_reports_complete() {
        let fake = Fake::with(&[(1, &[0, 1]), (2, &[0, 1]), (3, &[1])]);
        let moved = Threads::over(fake.clone()).confine(&cpus(&[1])).unwrap();
        assert!(moved.complete(), "{moved:?}");
        assert_eq!(moved.threads, 3);
    }
}
