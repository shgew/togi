use std::time::Duration;

use togi_machine::{
    CoreId, CoreInfo, CoreSet, CpuId, LoadSchedule, Regime, TrialSpec, Workload, order,
};

use crate::error::InvalidTrial;
use crate::id::TrialId;

/// Where an instance lives on disk and in its scope name.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum InstanceName {
    Work,
    Core(CoreId),
}

impl InstanceName {
    pub(crate) fn dir_name(self) -> String {
        match self {
            Self::Work => "work".to_owned(),
            Self::Core(core) => format!("c{:02}", core.get()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InstancePlan {
    pub(crate) name: InstanceName,
    pub(crate) core: CoreId,
    /// The logical CPUs the instance may run on; never empty.
    pub(crate) cpus: Vec<CpuId>,
    pub(crate) cpu_count: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Load {
    Continuous,
    Steps(LoadSchedule),
    /// R6: instance indices in the order their bursts run.
    Bursts {
        order: Vec<usize>,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct TrialPlan {
    pub(crate) id: TrialId,
    pub(crate) regime: Regime,
    pub(crate) workload: Workload,
    pub(crate) duration: Duration,
    pub(crate) instances: Vec<InstancePlan>,
    pub(crate) load: Load,
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
        if !spec.regime.instance_per_core() && spec.cores.len() > 1 {
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
        let instances = if spec.regime.instance_per_core() {
            let mut instances = Vec::with_capacity(infos.len());
            for info in &infos {
                let first = *info
                    .cpus
                    .first()
                    .ok_or(InvalidTrial::CoreWithoutCpu(info.core))?;
                instances.push(InstancePlan {
                    name: InstanceName::Core(info.core),
                    core: info.core,
                    cpus: vec![first],
                    cpu_count: 1,
                });
            }
            let derived: Vec<CpuId> = instances.iter().map(|i| i.cpus[0]).collect();
            if derived != spec.cpus {
                return Err(InvalidTrial::CpusMismatch);
            }
            instances
        } else {
            if spec.cpus.is_empty() {
                return Err(InvalidTrial::NoCpus);
            }
            let assigned = infos[0];
            if let Some(&foreign) = spec.cpus.iter().find(|cpu| !assigned.cpus.contains(cpu)) {
                return Err(InvalidTrial::ForeignCpu(foreign));
            }
            let cpu_count = checked_count(spec.cpus.len())?;
            vec![InstancePlan {
                name: InstanceName::Work,
                core: spec.cores[0],
                cpus: spec.cpus.clone(),
                cpu_count,
            }]
        };
        let load = match (LoadSchedule::for_spec(spec), spec.regime) {
            (Some(schedule), _) => Load::Steps(schedule),
            (None, Regime::R6) => {
                let selected: Vec<CoreInfo> = infos.iter().map(|&info| info.clone()).collect();
                let order = order(&selected)
                    .into_iter()
                    .filter_map(|core| spec.cores.iter().position(|&c| c == core))
                    .collect();
                Load::Bursts { order }
            }
            (None, _) => Load::Continuous,
        };
        Ok(Self {
            id,
            regime: spec.regime,
            workload: spec.workload.workload().clone(),
            duration: spec.duration,
            instances,
            load,
        })
    }

    #[expect(
        clippy::cast_possible_truncation,
        reason = "validated unique cores bound instances to 16"
    )]
    pub(crate) fn instance_count(&self) -> u32 {
        self.instances.len() as u32
    }

    pub(crate) fn per_core(&self) -> bool {
        self.regime.instance_per_core()
    }
}

pub(crate) fn checked_count(count: usize) -> Result<u32, InvalidTrial> {
    u32::try_from(count).map_err(InvalidTrial::Count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::InvalidId;
    use togi_machine::{CcdId, Condition, pick_workload};

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
        TrialSpec {
            id: "t1".into(),
            regime,
            workload: pick_workload(regime, 0).id(),
            condition: Condition::Alone,
            cores: cores.iter().map(|&n| core(n)).collect(),
            cpus: cpus.iter().map(|&n| CpuId::from(n)).collect(),
            duration: Duration::from_secs(10),
            index: 0,
            seed: 7,
            offsets: togi_machine::PerCore::default(),
        }
    }

    #[test]
    fn single_instance_regimes_get_one_work_instance_with_all_cpus() {
        let plan = TrialPlan::build(&spec(Regime::R5, &[1], &[1, 9]), &topology()).unwrap();
        assert_eq!(plan.instances.len(), 1);
        assert_eq!(plan.instances[0].name, InstanceName::Work);
        assert_eq!(plan.instances[0].core, core(1));
        assert_eq!(plan.instances[0].cpus, [CpuId::from(1), CpuId::from(9)]);
        assert_eq!(plan.load, Load::Continuous);
    }

    #[test]
    fn per_core_regimes_take_the_first_cpu_of_each_core() {
        let plan = TrialPlan::build(&spec(Regime::R7, &[2, 0], &[2, 0]), &topology()).unwrap();
        let names: Vec<_> = plan.instances.iter().map(|i| i.name.dir_name()).collect();
        assert_eq!(names, ["c02", "c00"]);
        assert_eq!(plan.instances[1].cpus, [CpuId::from(0)]);
        assert_eq!(plan.load, Load::Continuous);
    }

    #[test]
    fn r6_bursts_follow_the_machine_order_mapped_to_instances() {
        let plan =
            TrialPlan::build(&spec(Regime::R6, &[3, 0, 2, 1], &[3, 0, 2, 1]), &topology()).unwrap();
        // order() interleaves ccd0 [0,1] and ccd1 [2,3]: 0, 2, 1, 3 -> positions in [3,0,2,1]
        assert_eq!(
            plan.load,
            Load::Bursts {
                order: vec![1, 2, 3, 0]
            }
        );
    }

    #[test]
    fn load_steps_come_from_the_spec() {
        let plan = TrialPlan::build(&spec(Regime::R3, &[0], &[0]), &topology()).unwrap();
        assert_eq!(plan.load, Load::Steps(LoadSchedule::Random { seed: 7 }));
        let plan = TrialPlan::build(&spec(Regime::R4, &[0], &[0]), &topology()).unwrap();
        assert!(matches!(plan.load, Load::Steps(LoadSchedule::Duty { .. })));
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
