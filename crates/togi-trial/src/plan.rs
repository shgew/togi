use std::time::Duration;

use togi_machine::{
    CoreId, CoreInfo, CoreSet, CpuId, LoadSchedule, PhaseSchedule, Regime, TrialRole, TrialSpec,
    Workload,
};

use crate::error::InvalidTrial;
use crate::id::TrialId;

/// Where an instance lives on disk and in its scope name.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum InstanceName {
    /// The only instance of a trial.
    Work,
    /// The one instance of a physical core.
    Core(CoreId),
    /// The instance of one logical CPU, so the two threads of an SMT core stay apart.
    Cpu(CpuId),
}

impl InstanceName {
    pub(crate) fn dir_name(self) -> String {
        match self {
            Self::Work => "work".to_owned(),
            Self::Core(core) => format!("c{:02}", core.get()),
            Self::Cpu(cpu) => format!("cpu{:02}", cpu.get()),
        }
    }
}

/// How a regime places its instances.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Layout {
    /// One instance on one core, with all the trial's logical CPUs.
    One,
    /// One instance on the first logical CPU of each loaded core.
    PerCore,
    /// One instance on each loaded logical CPU.
    PerCpu,
}

impl Layout {
    fn of(regime: Regime) -> Self {
        match regime {
            Regime::R1 | Regime::R2 | Regime::R3 | Regime::R4 | Regime::R5 => Self::One,
            Regime::R6 | Regime::R7 | Regime::R8 => Self::PerCore,
            Regime::R9 => Self::PerCpu,
        }
    }
}

/// One backend process: the workload it runs, where, and how its load changes over the trial.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InstancePlan {
    pub(crate) name: InstanceName,
    /// The physical core every failure of the instance is attributed to.
    pub(crate) core: CoreId,
    /// The logical CPUs the instance may run on; never empty.
    pub(crate) cpus: Vec<CpuId>,
    pub(crate) cpu_count: u32,
    /// Its own workload, so its own backend and its own output classifier.
    pub(crate) workload: Workload,
    pub(crate) load: Load,
}

/// How one instance's load changes. Every instance has its own, independent of the others.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Load {
    Continuous,
    Steps(LoadSchedule),
    /// R6: this instance's turn in the order the bursts run, from 0.
    Burst {
        position: usize,
    },
    /// R9: runs for `phase`, is stopped for `phase`, and so on, starting running.
    Phases {
        phase: Duration,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct TrialPlan {
    pub(crate) id: TrialId,
    pub(crate) duration: Duration,
    pub(crate) instances: Vec<InstancePlan>,
    instance_count: u32,
}

/// The roles that share one instance, led by the instance's first logical CPU.
struct Group<'a> {
    instance: CpuId,
    roles: Vec<&'a TrialRole>,
}

impl TrialPlan {
    /// Validates the specification against the topology before anything touches the disk.
    pub(crate) fn build(spec: &TrialSpec, topology: &[CoreInfo]) -> Result<Self, InvalidTrial> {
        let id = TrialId::parse(&spec.id)?;
        if spec.duration.is_zero() {
            return Err(InvalidTrial::ZeroDuration);
        }
        if spec.cores.is_empty() {
            return Err(InvalidTrial::NoCores);
        }
        let layout = Layout::of(spec.regime);
        if layout == Layout::One && spec.cores.len() > 1 {
            return Err(InvalidTrial::CoresForOneInstance);
        }
        let mut seen = CoreSet::default();
        let mut infos = Vec::with_capacity(spec.cores.len());
        for &core in &spec.cores {
            if !seen.insert(core) {
                return Err(InvalidTrial::DuplicateCore(core));
            }
            infos.push(
                topology
                    .iter()
                    .find(|info| info.core == core)
                    .ok_or(InvalidTrial::UnknownCore(core))?,
            );
        }
        match layout {
            Layout::One => {
                if spec.cpus.is_empty() {
                    return Err(InvalidTrial::NoCpus);
                }
                let assigned = infos[0];
                if let Some(&foreign) = spec.cpus.iter().find(|cpu| !assigned.cpus.contains(cpu)) {
                    return Err(InvalidTrial::ForeignCpu(foreign));
                }
            }
            Layout::PerCore => {
                let mut firsts = Vec::with_capacity(infos.len());
                for info in &infos {
                    firsts.push(
                        *info
                            .cpus
                            .first()
                            .ok_or(InvalidTrial::CoreWithoutCpu(info.core))?,
                    );
                }
                if firsts != spec.cpus {
                    return Err(InvalidTrial::CpusMismatch);
                }
            }
            Layout::PerCpu => {
                if spec.cpus.is_empty() {
                    return Err(InvalidTrial::NoCpus);
                }
                if let Some(&foreign) = spec
                    .cpus
                    .iter()
                    .find(|cpu| !infos.iter().any(|info| info.cpus.contains(cpu)))
                {
                    return Err(InvalidTrial::ForeignCpu(foreign));
                }
            }
        }
        let instances = instances(spec, topology, layout)?;
        let instance_count = checked_count(instances.len())?;
        Ok(Self {
            id,
            duration: spec.duration,
            instances,
            instance_count,
        })
    }

    pub(crate) fn instance_count(&self) -> u32 {
        self.instance_count
    }

    /// Each instance's load, indexed like the instances.
    pub(crate) fn loads(&self) -> Vec<Load> {
        self.instances
            .iter()
            .map(|instance| instance.load)
            .collect()
    }

    /// Whether R6's idle-and-burst rotation runs: its instances launch stopped.
    pub(crate) fn bursts(&self) -> bool {
        self.instances
            .iter()
            .any(|instance| matches!(instance.load, Load::Burst { .. }))
    }
}

/// Groups the spec's roles by instance and checks them against the topology, the spec's cores and
/// logical CPUs, and the regime's placement.
fn instances(
    spec: &TrialSpec,
    topology: &[CoreInfo],
    layout: Layout,
) -> Result<Vec<InstancePlan>, InvalidTrial> {
    if spec.roles.is_empty() {
        return Err(InvalidTrial::NoRoles);
    }
    let mut groups: Vec<Group<'_>> = Vec::new();
    let mut cpus: Vec<CpuId> = Vec::with_capacity(spec.roles.len());
    for role in &spec.roles {
        let info = topology
            .iter()
            .find(|info| info.core == role.core)
            .ok_or(InvalidTrial::UnknownCore(role.core))?;
        if !info.cpus.contains(&role.cpu) {
            return Err(InvalidTrial::ForeignCpu(role.cpu));
        }
        if cpus.contains(&role.cpu) {
            return Err(InvalidTrial::DuplicateCpu(role.cpu));
        }
        cpus.push(role.cpu);
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.instance == role.instance)
        {
            group.roles.push(role);
        } else if role.instance == role.cpu {
            groups.push(Group {
                instance: role.instance,
                roles: vec![role],
            });
        } else {
            return Err(InvalidTrial::InstanceSplit(role.instance));
        }
    }
    let mut spec_cpus = spec.cpus.clone();
    spec_cpus.sort_unstable();
    cpus.sort_unstable();
    let mut spec_cores = spec.cores.clone();
    spec_cores.sort_unstable();
    let mut role_cores: Vec<CoreId> = spec.roles.iter().map(|role| role.core).collect();
    role_cores.sort_unstable();
    role_cores.dedup();
    if cpus != spec_cpus || role_cores != spec_cores {
        return Err(InvalidTrial::RolesMismatch);
    }
    let mut instances = Vec::with_capacity(groups.len());
    for group in &groups {
        let first = group.roles[0];
        if group.roles.iter().any(|role| {
            role.core != first.core
                || role.workload != first.workload
                || role.schedule != first.schedule
        }) {
            return Err(InvalidTrial::InstanceSplit(group.instance));
        }
        let name = match layout {
            Layout::One => InstanceName::Work,
            Layout::PerCore => InstanceName::Core(first.core),
            Layout::PerCpu => InstanceName::Cpu(group.instance),
        };
        let cpus: Vec<CpuId> = group.roles.iter().map(|role| role.cpu).collect();
        instances.push(InstancePlan {
            name,
            core: first.core,
            cpu_count: checked_count(cpus.len())?,
            cpus,
            workload: first.workload.workload().clone(),
            load: load_of(first.schedule)?,
        });
    }
    check_layout(&instances, layout)?;
    check_bursts(&instances)?;
    Ok(instances)
}

/// What the regime's placement demands of the instances the roles formed. A per-core regime needs
/// no check of its own: the roles already cover exactly the trial's cores and their first logical
/// CPUs, each once.
fn check_layout(instances: &[InstancePlan], layout: Layout) -> Result<(), InvalidTrial> {
    let fits = match layout {
        Layout::One => instances.len() == 1,
        Layout::PerCore => true,
        Layout::PerCpu => instances.iter().all(|instance| instance.cpus.len() == 1),
    };
    if fits {
        Ok(())
    } else {
        Err(InvalidTrial::InstanceLayout)
    }
}

/// The bursts of R6 take turns, so their positions are each of `0..n` once.
fn check_bursts(instances: &[InstancePlan]) -> Result<(), InvalidTrial> {
    let mut positions: Vec<usize> = instances
        .iter()
        .filter_map(|instance| match instance.load {
            Load::Burst { position } => Some(position),
            Load::Continuous | Load::Steps(_) | Load::Phases { .. } => None,
        })
        .collect();
    positions.sort_unstable();
    if positions
        .iter()
        .enumerate()
        .all(|(n, &position)| n == position)
    {
        Ok(())
    } else {
        Err(InvalidTrial::BurstPositions)
    }
}

fn load_of(schedule: PhaseSchedule) -> Result<Load, InvalidTrial> {
    match schedule {
        PhaseSchedule::Sustained {} => Ok(Load::Continuous),
        PhaseSchedule::Steps { load } => Ok(Load::Steps(load)),
        PhaseSchedule::Burst { position } => Ok(Load::Burst { position }),
        PhaseSchedule::Alternate { phase_s: 0, .. } => Err(InvalidTrial::ZeroPhase),
        // The serial CPU never stops; the others follow the phases.
        PhaseSchedule::Alternate { serial: true, .. } => Ok(Load::Continuous),
        PhaseSchedule::Alternate {
            phase_s,
            serial: false,
        } => Ok(Load::Phases {
            phase: Duration::from_secs(u64::from(phase_s)),
        }),
    }
}

pub(crate) fn checked_count(count: usize) -> Result<u32, InvalidTrial> {
    u32::try_from(count).map_err(InvalidTrial::Count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::InvalidId;
    use togi_machine::{CcdId, Condition, WorkloadId, pick_workload};

    fn core(n: u8) -> CoreId {
        CoreId::new(i64::from(n)).unwrap()
    }

    fn topology() -> Vec<CoreInfo> {
        (0..4)
            .map(|n| CoreInfo {
                core: core(n),
                ccd: CcdId::try_from(n / 2).unwrap(),
                cpus: vec![CpuId::from(u16::from(n)), CpuId::from(u16::from(n) + 8)],
            })
            .collect()
    }

    fn spec(regime: Regime, cores: &[u8], cpus: &[u16]) -> TrialSpec {
        let workload = pick_workload(regime, 0).id();
        let cores: Vec<CoreId> = cores.iter().map(|&n| core(n)).collect();
        let cpus: Vec<CpuId> = cpus.iter().map(|&n| CpuId::from(n)).collect();
        // A specification the regime refuses has no roles; its cores and CPUs are checked first.
        let roles =
            TrialRole::legacy(regime, workload, 7, &cores, &cpus, &topology()).unwrap_or_default();
        TrialSpec {
            id: "t1".into(),
            regime,
            workload,
            condition: Condition::Alone,
            cores,
            cpus,
            duration: Duration::from_secs(10),
            index: 0,
            seed: 7,
            offsets: togi_machine::PerCore::default(),
            roles,
            controller: togi_machine::ControllerMode::Legacy,
        }
    }

    /// A specification whose cores and CPUs are exactly what `roles` cover, in their order.
    fn spec_of(regime: Regime, roles: Vec<TrialRole>) -> TrialSpec {
        let mut cores: Vec<CoreId> = Vec::new();
        for role in &roles {
            if !cores.contains(&role.core) {
                cores.push(role.core);
            }
        }
        TrialSpec {
            id: "t1".into(),
            regime,
            workload: pick_workload(regime, 0).id(),
            condition: Condition::Alone,
            cores,
            cpus: roles.iter().map(|role| role.cpu).collect(),
            duration: Duration::from_secs(10),
            index: 0,
            seed: 7,
            offsets: togi_machine::PerCore::default(),
            roles,
            controller: togi_machine::ControllerMode::Legacy,
        }
    }

    fn neighbours(class_index: usize, light: u8) -> TrialSpec {
        let class = WorkloadId::new(Regime::R8, class_index).unwrap();
        let heavy = pick_workload(Regime::R2, 0).id();
        let roles = TrialRole::neighbours(class, core(light), heavy, 7, &topology()).unwrap();
        spec_of(Regime::R8, roles)
    }

    fn all_threads(serial: u16) -> TrialSpec {
        let class = pick_workload(Regime::R9, 0).id();
        let roles = TrialRole::all_threads(class, CpuId::from(serial), &topology()).unwrap();
        spec_of(Regime::R9, roles)
    }

    fn refused(spec: &TrialSpec) -> InvalidTrial {
        TrialPlan::build(spec, &topology()).unwrap_err()
    }

    #[test]
    fn single_instance_regimes_get_one_work_instance_with_all_cpus() {
        let plan = TrialPlan::build(&spec(Regime::R5, &[1], &[1, 9]), &topology()).unwrap();
        assert_eq!(plan.instances.len(), 1);
        assert_eq!(plan.instances[0].name, InstanceName::Work);
        assert_eq!(plan.instances[0].core, core(1));
        assert_eq!(plan.instances[0].cpus, [CpuId::from(1), CpuId::from(9)]);
        assert_eq!(plan.instances[0].cpu_count, 2);
        assert_eq!(plan.loads(), [Load::Continuous]);
    }

    #[test]
    fn per_core_regimes_take_the_first_cpu_of_each_core() {
        let plan = TrialPlan::build(&spec(Regime::R7, &[2, 0], &[2, 0]), &topology()).unwrap();
        let names: Vec<_> = plan.instances.iter().map(|i| i.name.dir_name()).collect();
        assert_eq!(names, ["c02", "c00"]);
        assert_eq!(plan.instances[1].cpus, [CpuId::from(0)]);
        assert_eq!(plan.loads(), [Load::Continuous, Load::Continuous]);
    }

    #[test]
    fn r6_bursts_follow_the_machine_order_mapped_to_instances() {
        let plan =
            TrialPlan::build(&spec(Regime::R6, &[3, 0, 2, 1], &[3, 0, 2, 1]), &topology()).unwrap();
        // order() interleaves ccd0 [0,1] and ccd1 [2,3]: 0, 2, 1, 3, so in [3,0,2,1] the
        // instances take turns 1, 2, 3, 0.
        assert_eq!(
            plan.loads(),
            [
                Load::Burst { position: 3 },
                Load::Burst { position: 0 },
                Load::Burst { position: 1 },
                Load::Burst { position: 2 },
            ]
        );
    }

    #[test]
    fn load_steps_come_from_the_spec() {
        let plan = TrialPlan::build(&spec(Regime::R3, &[0], &[0]), &topology()).unwrap();
        assert_eq!(
            plan.loads(),
            [Load::Steps(LoadSchedule::Random { seed: 7 })]
        );
        let plan = TrialPlan::build(&spec(Regime::R4, &[0], &[0]), &topology()).unwrap();
        assert!(matches!(
            plan.loads()[..],
            [Load::Steps(LoadSchedule::Duty { .. })]
        ));
    }

    #[test]
    fn r8_runs_the_light_core_beside_sustained_neighbours_each_in_its_own_instance() {
        let spec = neighbours(0, 1);
        let plan = TrialPlan::build(&spec, &topology()).unwrap();
        let names: Vec<_> = plan.instances.iter().map(|i| i.name.dir_name()).collect();
        assert_eq!(names, ["c00", "c01"]);
        assert_eq!(plan.loads(), [Load::Continuous, Load::Continuous]);
        for (instance, role) in plan.instances.iter().zip(&spec.roles) {
            assert_eq!(instance.core, role.core);
            assert_eq!(instance.cpus, [role.cpu]);
            assert_eq!(instance.workload, *role.workload.workload());
        }
        assert_ne!(
            plan.instances[0].workload, plan.instances[1].workload,
            "the neighbour and the light core run different workloads"
        );
    }

    #[test]
    fn r8_random_light_core_is_the_only_instance_with_steps() {
        let plan = TrialPlan::build(&neighbours(3, 1), &topology()).unwrap();
        assert_eq!(
            plan.loads(),
            [
                Load::Continuous,
                Load::Steps(LoadSchedule::Random { seed: 7 })
            ]
        );
    }

    #[test]
    fn r9_gives_every_logical_cpu_its_own_named_instance_and_keeps_the_serial_one_running() {
        let plan = TrialPlan::build(&all_threads(9), &topology()).unwrap();
        assert_eq!(plan.instances.len(), 8);
        let names: Vec<_> = plan.instances.iter().map(|i| i.name.dir_name()).collect();
        assert_eq!(
            names,
            [
                "cpu00", "cpu08", "cpu01", "cpu09", "cpu02", "cpu10", "cpu03", "cpu11"
            ]
        );
        let phase = Load::Phases {
            phase: Duration::from_secs(10),
        };
        for (instance, load) in plan.instances.iter().zip(plan.loads()) {
            let serial = instance.cpus == [CpuId::from(9)];
            assert_eq!(load, if serial { Load::Continuous } else { phase });
        }
        assert_eq!(plan.instances[0].core, plan.instances[1].core);
    }

    #[test]
    fn a_trial_without_roles_is_refused() {
        let mut spec = spec(Regime::R7, &[0], &[0]);
        spec.roles.clear();
        assert_eq!(refused(&spec), InvalidTrial::NoRoles);
    }

    #[test]
    fn a_role_on_a_cpu_of_another_core_is_refused() {
        let mut spec = spec(Regime::R7, &[0], &[0]);
        spec.roles[0].core = core(1);
        assert_eq!(refused(&spec), InvalidTrial::ForeignCpu(CpuId::from(0)));
    }

    #[test]
    fn a_role_on_an_unknown_core_is_refused() {
        let mut spec = spec(Regime::R7, &[0], &[0]);
        spec.roles[0].core = core(9);
        assert_eq!(refused(&spec), InvalidTrial::UnknownCore(core(9)));
    }

    #[test]
    fn two_roles_on_one_cpu_are_refused() {
        let mut spec = spec(Regime::R5, &[1], &[1, 9]);
        spec.roles[1].cpu = CpuId::from(1);
        assert_eq!(refused(&spec), InvalidTrial::DuplicateCpu(CpuId::from(1)));
    }

    #[test]
    fn roles_that_leave_out_a_cpu_of_the_trial_are_refused() {
        let mut spec = spec(Regime::R7, &[0, 1], &[0, 1]);
        spec.roles.pop();
        assert_eq!(refused(&spec), InvalidTrial::RolesMismatch);
    }

    #[test]
    fn an_instance_must_be_led_by_its_first_role() {
        let mut spec = spec(Regime::R5, &[1], &[1, 9]);
        spec.roles[0].instance = CpuId::from(9);
        assert_eq!(refused(&spec), InvalidTrial::InstanceSplit(CpuId::from(9)));
    }

    #[test]
    fn roles_of_one_instance_must_share_their_schedule() {
        let mut spec = spec(Regime::R5, &[1], &[1, 9]);
        spec.roles[1].schedule = PhaseSchedule::Burst { position: 0 };
        assert_eq!(refused(&spec), InvalidTrial::InstanceSplit(CpuId::from(1)));
    }

    #[test]
    fn a_single_instance_regime_with_two_instances_is_refused() {
        let mut spec = spec(Regime::R5, &[1], &[1, 9]);
        spec.roles[1].instance = CpuId::from(9);
        assert_eq!(refused(&spec), InvalidTrial::InstanceLayout);
    }

    #[test]
    fn an_all_threads_instance_with_two_cpus_is_refused() {
        let mut spec = all_threads(9);
        spec.roles[1].instance = spec.roles[0].cpu;
        assert_eq!(refused(&spec), InvalidTrial::InstanceLayout);
    }

    #[test]
    fn burst_positions_must_be_each_of_zero_to_n_once() {
        let mut spec = spec(Regime::R6, &[0, 1], &[0, 1]);
        spec.roles[0].schedule = PhaseSchedule::Burst { position: 5 };
        assert_eq!(refused(&spec), InvalidTrial::BurstPositions);
    }

    #[test]
    fn a_zero_second_phase_is_refused() {
        let mut spec = all_threads(9);
        for role in &mut spec.roles {
            role.schedule = PhaseSchedule::Alternate {
                phase_s: 0,
                serial: false,
            };
        }
        assert_eq!(refused(&spec), InvalidTrial::ZeroPhase);
    }

    #[test]
    fn invalid_specs_are_refused_before_any_side_effect() {
        let topo = topology();
        let mut bad_id = spec(Regime::R1, &[0], &[0]);
        bad_id.id = "../x".into();
        assert!(matches!(
            TrialPlan::build(&bad_id, &topo),
            Err(InvalidTrial::Id(_))
        ));
        let mut empty_id = bad_id;
        empty_id.id = String::new();
        assert_eq!(
            TrialPlan::build(&empty_id, &topo).unwrap_err(),
            InvalidTrial::Id(InvalidId::Empty)
        );
        let mut zero = spec(Regime::R1, &[0], &[0]);
        zero.duration = Duration::ZERO;
        assert_eq!(
            TrialPlan::build(&zero, &topo).unwrap_err(),
            InvalidTrial::ZeroDuration
        );
        assert_eq!(
            TrialPlan::build(&spec(Regime::R1, &[], &[0]), &topo).unwrap_err(),
            InvalidTrial::NoCores
        );
        assert_eq!(
            TrialPlan::build(&spec(Regime::R7, &[1, 1], &[1, 1]), &topo).unwrap_err(),
            InvalidTrial::DuplicateCore(core(1))
        );
        assert_eq!(
            TrialPlan::build(&spec(Regime::R1, &[9], &[0]), &topo).unwrap_err(),
            InvalidTrial::UnknownCore(core(9))
        );
        assert_eq!(
            TrialPlan::build(&spec(Regime::R1, &[0], &[]), &topo).unwrap_err(),
            InvalidTrial::NoCpus
        );
        assert_eq!(
            TrialPlan::build(&spec(Regime::R1, &[0], &[1]), &topo).unwrap_err(),
            InvalidTrial::ForeignCpu(CpuId::from(1))
        );
        assert_eq!(
            TrialPlan::build(&spec(Regime::R9, &[0], &[]), &topo).unwrap_err(),
            InvalidTrial::NoCpus,
            "the per-CPU layout needs a CPU"
        );
        assert_eq!(
            TrialPlan::build(&spec(Regime::R9, &[0], &[0, 8, 1]), &topo).unwrap_err(),
            InvalidTrial::ForeignCpu(CpuId::from(1)),
            "the per-CPU layout refuses a CPU outside the selected cores"
        );
        assert_eq!(
            TrialPlan::build(&spec(Regime::R1, &[0, 1], &[1]), &topo).unwrap_err(),
            InvalidTrial::CoresForOneInstance,
            "work on core 1 must not be attributed to core 0"
        );
        assert_eq!(
            TrialPlan::build(&spec(Regime::R5, &[1, 0], &[1, 9]), &topo).unwrap_err(),
            InvalidTrial::CoresForOneInstance
        );
        assert_eq!(
            TrialPlan::build(&spec(Regime::R5, &[1], &[1, 8]), &topo).unwrap_err(),
            InvalidTrial::ForeignCpu(CpuId::from(8)),
            "a CPU of another core is refused for the one assigned core"
        );
        assert_eq!(
            TrialPlan::build(&spec(Regime::R6, &[0, 1], &[0]), &topo).unwrap_err(),
            InvalidTrial::CpusMismatch
        );
        let mut without_cpu = topology();
        without_cpu[0].cpus.clear();
        assert_eq!(
            TrialPlan::build(&spec(Regime::R7, &[0], &[0]), &without_cpu).unwrap_err(),
            InvalidTrial::CoreWithoutCpu(core(0))
        );
    }
}

#[cfg(test)]
mod numeric_tests {
    use super::checked_count;

    #[test_case::test_case(0, Some(0); "zero")]
    #[test_case::test_case(16, Some(16); "physical core bound")]
    #[test_case::test_case(u32::MAX as usize, Some(u32::MAX); "largest exact count")]
    #[test_case::test_case(u32::MAX as usize + 1, None; "first overflow")]
    #[cfg(target_pointer_width = "64")]
    fn trial_counts_refuse_overflow(count: usize, expected: Option<u32>) {
        assert_eq!(checked_count(count).ok(), expected);
    }
}
