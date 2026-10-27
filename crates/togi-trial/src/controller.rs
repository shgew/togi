//! Selects and establishes the controller's physical host for each trial.
//!
//! The backend scopes have their own confinement. The session, journal, hardware seams, runtime,
//! blocking pools and in-process dashboard share one process, so one affinity rule covers them.
//! [`placement`] supplies the managed shape-dependent contract; [`select`] preserves the legacy
//! scheduler's evidence contract. [`Threads`] moves every controller thread onto the chosen CPUs
//! and restores its original affinity after workload teardown.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, PoisonError};

use rustix::io::Errno;
use rustix::process::Pid;
use rustix::thread::CpuSet;
use togi_machine::{CcdId, CoreId, CoreInfo, CpuId, PerCore, TargetOffset};

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
    selected.sort_unstable_by_key(|cpu| cpu.get());
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

/// Where the controller runs. `cpus` are every logical CPU it may use.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Placement {
    pub(crate) cpus: Vec<CpuId>,
    pub(crate) contract: Contract,
}

/// A rotating or single controller host and the applied, read-back offset it held when placement
/// was computed. A missing offset stays unknown; it is never CO 0.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Host {
    pub(crate) core: CoreId,
    pub(crate) offset: Option<TargetOffset>,
}

/// The row of the managed placement table a trial fell into, or the legacy selector's choice,
/// with what it means for the trial's clock and credit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Contract {
    /// The legacy selector: no cap, no recorded host, credit unchanged.
    Legacy,
    /// Single CCD with an unloaded physical core: rotation across the unloaded cores. Capped; the
    /// host is untested and earns no exposure, while every loaded position keeps its credit.
    UnloadedHost(Host),
    /// Single CCD with every physical core loaded: rotation across the tested cores. Capped; the
    /// host is excluded from passing credit and keeps supplying failure evidence.
    ExcludedHost(Host),
    /// Every logical CPU of an entire idle CO-0 CCD. Capped; the loaded CCD keeps its credit.
    IdleCcd { ccd: CcdId },
    /// The lowest-ID unloaded CO-0 core. Uncapped; controller work is outside the tested set.
    IdleCore(Host),
    /// Rotation across the loaded cores. Uncapped; controller work counts as load on the host.
    LoadedHost(Host),
}

impl Contract {
    /// Whether the controller's cpufreq policies are capped for the trial.
    pub(crate) const fn capped(self) -> bool {
        match self {
            Self::UnloadedHost(_) | Self::ExcludedHost(_) | Self::IdleCcd { .. } => true,
            Self::Legacy | Self::IdleCore(_) | Self::LoadedHost(_) => false,
        }
    }

    /// The tested core the trial withholds passing credit from.
    pub(crate) const fn excluded(self) -> Option<CoreId> {
        match self {
            Self::ExcludedHost(host) => Some(host.core),
            Self::Legacy
            | Self::UnloadedHost(_)
            | Self::IdleCcd { .. }
            | Self::IdleCore(_)
            | Self::LoadedHost(_) => None,
        }
    }

    /// A stranger-readable account of the placement for the trial's progress lines.
    pub(crate) fn describe(self) -> String {
        let at = |host: Host| match host.offset {
            Some(offset) => format!("core {} at CO {}", host.core, offset.get()),
            None => format!("core {} at an unknown offset", host.core),
        };
        match self {
            Self::Legacy => "legacy placement, uncapped".to_owned(),
            Self::UnloadedHost(host) => format!(
                "rotating unloaded host {}, capped; the host earns no exposure credit",
                at(host)
            ),
            Self::ExcludedHost(host) => format!(
                "rotating tested host {}, capped; the host is excluded from passing credit",
                at(host)
            ),
            Self::IdleCcd { ccd } => format!("idle CO-0 CCD {ccd}, capped"),
            Self::IdleCore(host) => format!("idle CO-0 host {}, uncapped", at(host)),
            Self::LoadedHost(host) => format!(
                "rotating loaded host {}, uncapped; controller work counts as load",
                at(host)
            ),
        }
    }
}

/// Chooses managed placement from the physical trial shape, not the regime's name or workload
/// quota, following the controller placement table of the workloads spec in order. `None` when
/// the trial is not managed or no host has logical CPUs.
pub(crate) fn placement(
    topology: &[CoreInfo],
    spec: &togi_machine::TrialSpec,
) -> Option<Placement> {
    let togi_machine::ControllerMode::Managed { rotation, .. } = spec.controller else {
        return None;
    };
    let is_loaded = |info: &&CoreInfo| {
        spec.cores.contains(&info.core) || info.cpus.iter().any(|cpu| spec.cpus.contains(cpu))
    };
    let mut cores: Vec<&CoreInfo> = topology.iter().collect();
    cores.sort_unstable_by_key(|info| info.core);
    let (tested, unloaded): (Vec<&CoreInfo>, Vec<&CoreInfo>) =
        cores.iter().copied().partition(is_loaded);
    let first = *tested.first()?;
    let single_ccd = topology.iter().all(|info| info.ccd == first.ccd);
    if single_ccd {
        // Any unloaded core qualifies here, whatever its offset; the cap keeps it from
        // stealing the tested cores' clock.
        return if unloaded.is_empty() {
            rotating(&tested, rotation, spec, Contract::ExcludedHost)
        } else {
            rotating(&unloaded, rotation, spec, Contract::UnloadedHost)
        };
    }
    // An idle CCD is useful only when every one of its cores is known to hold CO 0.
    // R6's instances deliberately stop and burst; its entire target remains tested.
    if spec.regime != togi_machine::Regime::R6
        && tested.iter().all(|info| info.ccd == first.ccd)
        && let Some(ccd) = topology
            .iter()
            .filter(|info| info.ccd != first.ccd)
            .map(|info| info.ccd)
            .filter(|ccd| {
                topology
                    .iter()
                    .filter(|info| info.ccd == *ccd)
                    .all(|info| offsets_zero(spec, info) && !is_loaded(&info))
            })
            .min()
    {
        let mut cpus: Vec<CpuId> = topology
            .iter()
            .filter(|info| info.ccd == ccd)
            .flat_map(|info| info.cpus.iter().copied())
            .collect();
        cpus.sort_unstable();
        cpus.dedup();
        return (!cpus.is_empty()).then_some(Placement {
            cpus,
            contract: Contract::IdleCcd { ccd },
        });
    }
    // Only a CO-0 unloaded core qualifies: a nonzero one is never chosen merely because it is
    // shallower than the loaded cores.
    if let Some(idle) = unloaded.iter().find(|info| offsets_zero(spec, info)) {
        return (!idle.cpus.is_empty()).then(|| Placement {
            cpus: idle.cpus.clone(),
            contract: Contract::IdleCore(host(spec, idle)),
        });
    }
    rotating(&tested, rotation, spec, Contract::LoadedHost)
}

fn offsets_zero(spec: &togi_machine::TrialSpec, info: &CoreInfo) -> bool {
    spec.offsets.get(info.core).map(TargetOffset::get) == Some(0)
}

fn host(spec: &togi_machine::TrialSpec, info: &CoreInfo) -> Host {
    Host {
        core: info.core,
        offset: spec.offsets.get(info.core),
    }
}

/// The host at the caller's persistent trial ordinal modulo the eligible count; `eligible` is in
/// core-ID order.
fn rotating(
    eligible: &[&CoreInfo],
    rotation: u64,
    spec: &togi_machine::TrialSpec,
    contract: fn(Host) -> Contract,
) -> Option<Placement> {
    let count = u64::try_from(eligible.len())
        .ok()
        .filter(|count| *count != 0)?;
    let index = usize::try_from(rotation % count).ok()?;
    let info = eligible.get(index)?;
    (!info.cpus.is_empty()).then(|| Placement {
        cpus: info.cpus.clone(),
        contract: contract(host(spec, info)),
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

/// Moves the process's threads and reports incomplete moves. The supervisor decides whether
/// incompleteness withholds exposure under the trial's selected controller contract.
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
        let index = usize::from(cpu.get());
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

    use super::*;

    fn core(n: u8) -> CoreId {
        CoreId::new(i64::from(n)).unwrap()
    }

    /// Four cores, core n on logical CPUs n and n + 4.
    fn topology() -> Vec<CoreInfo> {
        (0..4_u8)
            .map(|n| CoreInfo {
                core: core(n),
                ccd: CcdId::new(0).unwrap(),
                cpus: vec![CpuId::from(u16::from(n)), CpuId::from(u16::from(n) + 4)],
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
        list.iter().map(|&cpu| CpuId::from(cpu)).collect()
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
            !chosen.contains(&CpuId::from(0)) && !chosen.contains(&CpuId::from(4)),
            "{chosen:?}"
        );
        let chosen = pick(&[], &[4], &all([0, -3, -3, -3])).unwrap();
        assert!(
            !chosen.contains(&CpuId::from(0)) && !chosen.contains(&CpuId::from(4)),
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

    fn managed(
        regime: togi_machine::Regime,
        loaded: &[u8],
        offsets: [i64; 4],
        rotation: u64,
        single_ccd: bool,
    ) -> Option<Placement> {
        managed_with(
            regime,
            loaded,
            held(&all(offsets)),
            togi_machine::ControllerMode::Managed {
                rotation,
                purpose: togi_machine::Purpose::Isolated,
            },
            single_ccd,
        )
    }

    fn managed_with(
        regime: togi_machine::Regime,
        loaded: &[u8],
        offsets: PerCore<TargetOffset>,
        controller: togi_machine::ControllerMode,
        single_ccd: bool,
    ) -> Option<Placement> {
        let mut topology = topology();
        if !single_ccd {
            for info in &mut topology[2..] {
                info.ccd = CcdId::new(1).unwrap();
            }
        }
        let spec = togi_machine::TrialSpec {
            id: "placement".to_owned(),
            regime,
            workload: togi_machine::pick_workload(regime, 0).id(),
            condition: togi_machine::Condition::Together,
            cores: loaded.iter().map(|&n| core(n)).collect(),
            cpus: loaded.iter().map(|&n| CpuId::from(u16::from(n))).collect(),
            roles: Vec::new(),
            duration: std::time::Duration::from_secs(120),
            index: 0,
            seed: 0,
            offsets,
            controller,
        };
        placement(&topology, &spec)
    }

    /// The host `core` at the applied offset the test gave it.
    fn host_at(n: u8, offset: i64) -> Host {
        Host {
            core: core(n),
            offset: Some(TargetOffset::new(offset).unwrap()),
        }
    }

    fn on_core(n: u8, contract: Contract) -> Placement {
        Placement {
            cpus: cpus(&[u16::from(n), u16::from(n) + 4]),
            contract,
        }
    }

    #[test_case::test_case(togi_machine::Regime::R1; "light mapping")]
    #[test_case::test_case(togi_machine::Regime::R2; "heavy mapping")]
    #[test_case::test_case(togi_machine::Regime::R7; "CCD mapping")]
    #[test_case::test_case(togi_machine::Regime::R8; "neighbour mapping")]
    fn mapping_uses_the_entire_idle_zero_ccd_capped(regime: togi_machine::Regime) {
        let got = managed(regime, &[0, 1], [-20, -20, 0, 0], 3, false);
        assert_eq!(
            got,
            Some(Placement {
                cpus: cpus(&[2, 3, 6, 7]),
                contract: Contract::IdleCcd {
                    ccd: CcdId::new(1).unwrap()
                },
            })
        );
        let contract = got.unwrap().contract;
        assert_eq!((contract.capped(), contract.excluded()), (true, None));
    }

    /// The same shape and offsets under each purpose, for a trial that loads cores 0 and 1 of one
    /// CCD beside an entire idle CO-0 CCD.
    fn idle_ccd_shape(purpose: togi_machine::Purpose) -> Option<Placement> {
        managed_with(
            togi_machine::Regime::R1,
            &[0, 1],
            held(&all([-20, -20, 0, 0])),
            togi_machine::ControllerMode::Managed {
                rotation: 3,
                purpose,
            },
            false,
        )
    }

    #[test]
    fn exact_and_isolated_trials_of_one_shape_get_the_same_idle_ccd_placement() {
        let isolated = idle_ccd_shape(togi_machine::Purpose::Isolated);
        assert_eq!(
            isolated,
            Some(Placement {
                cpus: cpus(&[2, 3, 6, 7]),
                contract: Contract::IdleCcd {
                    ccd: CcdId::new(1).unwrap()
                },
            })
        );
        assert_eq!(idle_ccd_shape(togi_machine::Purpose::Exact), isolated);
    }

    #[test]
    fn an_idle_ccd_with_a_nonzero_core_is_not_an_idle_zero_ccd() {
        let got = managed(
            togi_machine::Regime::R1,
            &[0, 1],
            [-20, -20, 0, -1],
            0,
            false,
        );
        assert_eq!(
            got,
            Some(Placement {
                cpus: cpus(&[2, 6]),
                contract: Contract::IdleCore(host_at(2, 0)),
            })
        );
    }

    #[test_case::test_case(togi_machine::Regime::R7, 0, 0, -20; "all core first host")]
    #[test_case::test_case(togi_machine::Regime::R7, 1, 1, -10; "all core second host")]
    #[test_case::test_case(togi_machine::Regime::R7, 4, 0, -20; "all core wraps")]
    #[test_case::test_case(togi_machine::Regime::R9, 3, 3, 0; "all threads")]
    #[test_case::test_case(togi_machine::Regime::R6, 2, 2, -5; "idle bursts")]
    fn full_machine_rotates_uncapped_and_keeps_host_credit(
        regime: togi_machine::Regime,
        rotation: u64,
        host: u8,
        offset: i64,
    ) {
        let got = managed(regime, &[0, 1, 2, 3], [-20, -10, -5, 0], rotation, false);
        assert_eq!(
            got,
            Some(on_core(host, Contract::LoadedHost(host_at(host, offset))))
        );
        let contract = got.unwrap().contract;
        assert_eq!((contract.capped(), contract.excluded()), (false, None));
    }

    #[test_case::test_case(togi_machine::Regime::R6; "idle subset")]
    #[test_case::test_case(togi_machine::Regime::R7; "partial load")]
    fn a_partial_uses_an_unloaded_zero_core_when_no_idle_zero_ccd_exists(
        regime: togi_machine::Regime,
    ) {
        let got = managed(regime, &[0, 2], [-20, 0, -20, -10], 1, false);
        assert_eq!(got, Some(on_core(1, Contract::IdleCore(host_at(1, 0)))));
        let contract = got.unwrap().contract;
        assert_eq!((contract.capped(), contract.excluded()), (false, None));
    }

    #[test]
    fn partials_without_an_unloaded_zero_rotate_among_the_loaded_cores_not_the_least_deep_idle() {
        assert_eq!(
            managed(
                togi_machine::Regime::R7,
                &[0, 2],
                [-20, -1, -30, -1],
                1,
                false
            ),
            Some(on_core(2, Contract::LoadedHost(host_at(2, -30))))
        );
    }

    #[test]
    fn r6_uses_one_zero_core_even_when_an_entire_idle_zero_ccd_exists() {
        assert_eq!(
            managed(
                togi_machine::Regime::R6,
                &[0, 1],
                [-20, -20, 0, 0],
                7,
                false
            ),
            Some(on_core(2, Contract::IdleCore(host_at(2, 0))))
        );
    }

    #[test_case::test_case(0, 1, 0; "first unloaded core at CO 0")]
    #[test_case::test_case(1, 3, -7; "second unloaded core at a nonzero offset")]
    #[test_case::test_case(2, 1, 0; "wraps over the two unloaded cores")]
    #[test_case::test_case(u64::MAX, 3, -7; "large ordinal")]
    fn single_ccd_with_an_unloaded_core_rotates_across_unloaded_cores_regardless_of_offsets(
        rotation: u64,
        host: u8,
        offset: i64,
    ) {
        let got = managed(
            togi_machine::Regime::R7,
            &[0, 2],
            [-20, 0, -20, -7],
            rotation,
            true,
        );
        assert_eq!(
            got,
            Some(on_core(host, Contract::UnloadedHost(host_at(host, offset))))
        );
        let contract = got.unwrap().contract;
        assert_eq!(
            (contract.capped(), contract.excluded()),
            (true, None),
            "the unloaded host earns nothing and withholds no loaded position's credit"
        );
    }

    #[test_case::test_case(togi_machine::Regime::R1, 9, 0; "one tested core rotates over the three idle ones")]
    #[test_case::test_case(togi_machine::Regime::R6, 10, 1; "R6 on one CCD is not uncapped")]
    #[test_case::test_case(togi_machine::Regime::R2, 11, 3; "last idle core")]
    fn a_single_tested_core_on_one_ccd_never_hosts_the_controller_while_a_core_is_unloaded(
        regime: togi_machine::Regime,
        rotation: u64,
        host: u8,
    ) {
        let got = managed(regime, &[2], [-9, 0, -20, -4], rotation, true);
        assert_eq!(
            got.map(|placement| placement.cpus),
            Some(cpus(&[u16::from(host), u16::from(host) + 4]))
        );
    }

    #[test_case::test_case(0, 0; "first tested core")]
    #[test_case::test_case(1, 1; "second tested core")]
    #[test_case::test_case(3, 3; "last tested core")]
    #[test_case::test_case(6, 2; "wraps")]
    #[test_case::test_case(u64::MAX, 3; "large ordinal")]
    fn single_ccd_with_every_core_loaded_rotates_capped_across_tested_cores_and_excludes_the_host(
        rotation: u64,
        host: u8,
    ) {
        let offsets = [-20, -10, -5, 0];
        let got = managed(
            togi_machine::Regime::R7,
            &[0, 1, 2, 3],
            offsets,
            rotation,
            true,
        );
        assert_eq!(
            got,
            Some(on_core(
                host,
                Contract::ExcludedHost(host_at(host, offsets[usize::from(host)]))
            ))
        );
        let contract = got.unwrap().contract;
        assert_eq!(
            (contract.capped(), contract.excluded()),
            (true, Some(core(host)))
        );
    }

    #[test]
    fn the_ordinal_not_a_workload_index_picks_the_host_when_every_core_is_loaded() {
        let got = managed_with(
            togi_machine::Regime::R7,
            &[0, 1, 2, 3],
            held(&all([0, 0, 0, 0])),
            togi_machine::ControllerMode::Managed {
                rotation: 5,
                purpose: togi_machine::Purpose::Exact,
            },
            true,
        );
        assert_eq!(
            got,
            Some(on_core(1, Contract::ExcludedHost(host_at(1, 0)))),
            "5 mod 4"
        );
    }

    #[test]
    fn a_missing_offset_is_recorded_as_unknown_never_as_zero() {
        let offsets = held(&[Some(-3), None, Some(-3), Some(-3)]);
        let mode = togi_machine::ControllerMode::Managed {
            rotation: 0,
            purpose: togi_machine::Purpose::Exact,
        };
        // Core 1 is the lone unloaded core of the single CCD.
        let got = managed_with(togi_machine::Regime::R7, &[0, 2, 3], offsets, mode, true);
        assert_eq!(
            got,
            Some(on_core(
                1,
                Contract::UnloadedHost(Host {
                    core: core(1),
                    offset: None,
                })
            ))
        );
        // Multi-CCD, no unloaded CO-0 core, and an unknown one is not CO 0: loaded rotation.
        let offsets = held(&[Some(-3), None, Some(-3), Some(-3)]);
        let got = managed_with(togi_machine::Regime::R7, &[0, 2], offsets, mode, false);
        assert_eq!(got, Some(on_core(0, Contract::LoadedHost(host_at(0, -3)))));
    }

    #[test]
    fn only_a_managed_trial_has_a_managed_placement() {
        assert_eq!(
            managed_with(
                togi_machine::Regime::R7,
                &[0],
                held(&all([0; 4])),
                togi_machine::ControllerMode::Legacy,
                true,
            ),
            None
        );
    }

    #[test]
    fn a_trial_that_loads_nothing_has_no_placement() {
        assert_eq!(
            managed(togi_machine::Regime::R7, &[], [0; 4], 0, true),
            None
        );
    }

    #[test]
    fn the_description_names_the_host_and_its_offset() {
        let host = host_at(3, -7);
        assert_eq!(
            Contract::UnloadedHost(host).describe(),
            "rotating unloaded host core 3 at CO -7, capped; the host earns no exposure credit"
        );
        assert!(
            Contract::ExcludedHost(Host {
                core: core(3),
                offset: None
            })
            .describe()
            .contains("core 3 at an unknown offset")
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
        assert_eq!(fake.mask(1), set(&[2, 3]));
        assert_eq!(fake.mask(2), set(&[2, 3]));
        let moved = threads.restore().unwrap();
        assert!(moved.complete());
        assert_eq!(fake.mask(1), set(&[0]));
        assert_eq!(fake.mask(2), set(&[0]));
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
        assert_eq!(fake.mask(5), set(&[0, 1]));
        assert_eq!(fake.mask(2), set(&[0, 1]));
    }

    #[test]
    fn an_unreadable_thread_is_reported_failed_and_the_restore_stays_armed_until_it_is_complete() {
        let fake = Fake::with(&[(1, &[0, 1]), (2, &[0, 1])]);
        fake.0.lock().unwrap().unreadable.insert(2);
        let threads = Threads::over(fake.clone());
        let moved = threads.confine(&cpus(&[1])).unwrap();
        assert_eq!((moved.threads, moved.failed), (2, 1));
        assert!(!moved.complete());
        assert_eq!(fake.mask(1), set(&[1]));
        let moved = threads.restore().unwrap();
        assert!(!moved.complete(), "{moved:?}");
        fake.0.lock().unwrap().unreadable.clear();
        let moved = threads.restore().unwrap();
        assert!(moved.complete(), "{moved:?}");
        assert_eq!(fake.mask(1), set(&[0, 1]));
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
        assert_eq!(fake.mask(2), set(&[0, 1]));
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
        let moved = Threads::over(fake).confine(&cpus(&[1])).unwrap();
        assert!(moved.complete(), "{moved:?}");
        assert_eq!(moved.threads, 3);
    }
}
