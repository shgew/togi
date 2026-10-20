//! Per-logical-CPU roles of a trial: which workload each loaded CPU runs, in which instance and on which
//! schedule. The builders derive every role set from the topology, so the runtime, the journal and the tests
//! share one derivation.

use crate::object_only;
use crate::{
    CoreId, CoreInfo, CpuId, LoadSchedule, R9_PHASE_S, Regime, TrialSpec, WorkloadId, order,
};
use serde::Serialize;

object_only! {
    /// When a role's workload runs.
    ///
    /// `Sustained` runs the whole trial. `Steps` suspends and resumes the workload by its load schedule; a
    /// random schedule carries the seed that reproduces its periods. `Burst` is an R6 core's turn among the
    /// trial's cores in scheduling order, 0-based. `Alternate` is an R9 CPU: its workload runs in each all-thread
    /// phase of `phase_s` seconds and, when `serial`, in each one-thread phase too, so the one `serial` CPU stays
    /// active while the others rest.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum PhaseSchedule {
        Sustained {},
        Steps { load: LoadSchedule },
        Burst { position: usize },
        Alternate { phase_s: u32, serial: bool },
    }
}

object_only! {
    /// What one loaded logical CPU of a trial runs.
    ///
    /// `instance` is the first CPU of the process that runs `workload` on this CPU: roles that name the same
    /// instance share one process, as both threads of an R5 core do. `workload` is a workload that runs, never an
    /// R8 or R9 catalog entry, which only names the class.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct TrialRole {
        pub core: CoreId,
        pub cpu: CpuId,
        pub instance: CpuId,
        pub workload: WorkloadId,
        pub schedule: PhaseSchedule,
    }
}

/// A role set the topology or the trial's request cannot carry.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InvalidRoles {
    #[error("a trial loads no core")]
    NoCores,
    #[error("core {0} is loaded twice")]
    DuplicateCore(CoreId),
    #[error("core {0} is not in the topology")]
    UnknownCore(CoreId),
    #[error("core {0} has no logical CPU")]
    CoreWithoutCpu(CoreId),
    #[error("cpu {0} is loaded twice")]
    DuplicateCpu(CpuId),
    #[error("cpu {0} is not a logical CPU of its core")]
    ForeignCpu(CpuId),
    #[error("cpu {0} is not in the topology")]
    UnknownCpu(CpuId),
    #[error("{regime} trials do not take {requested} cores and {cpus} logical CPUs")]
    LoadShape {
        regime: Regime,
        requested: usize,
        cpus: usize,
    },
    #[error("{regime} has no legacy roles")]
    NotLegacy { regime: Regime },
    #[error("workload {got} is not in the {expected} catalog")]
    WrongCatalog { expected: Regime, got: WorkloadId },
    #[error("light core {0} has no neighbour on its CCD")]
    NoNeighbours(CoreId),
    #[error("the trial has no roles")]
    NoRoles,
    #[error("the roles do not name an R8 light core with neighbours or an R9 serial CPU")]
    NoAnchor,
    #[error("the roles differ from the ones the trial's request derives")]
    RolesDiffer,
    #[error("the roles load different cores or CPUs than the trial lists")]
    LoadDiffers,
}

fn core_info(topology: &[CoreInfo], core: CoreId) -> Result<&CoreInfo, InvalidRoles> {
    topology
        .iter()
        .find(|info| info.core == core)
        .ok_or(InvalidRoles::UnknownCore(core))
}

fn first_cpu(info: &CoreInfo) -> Result<CpuId, InvalidRoles> {
    info.cpus
        .first()
        .copied()
        .ok_or(InvalidRoles::CoreWithoutCpu(info.core))
}

fn expect_catalog(expected: Regime, got: WorkloadId) -> Result<(), InvalidRoles> {
    if got.regime() == expected {
        Ok(())
    } else {
        Err(InvalidRoles::WrongCatalog { expected, got })
    }
}

/// How a workload that runs is scheduled in a trial of its own: R3 by random steps, R4 by duty cycle, the rest
/// continuously.
fn steady_schedule(workload: WorkloadId, seed: u64) -> PhaseSchedule {
    match LoadSchedule::for_regime(workload.regime(), workload, seed) {
        Some(load) => PhaseSchedule::Steps { load },
        None => PhaseSchedule::Sustained {},
    }
}

impl TrialRole {
    /// The distinct loaded cores of `roles`, in role order.
    pub fn cores(roles: &[Self]) -> Vec<CoreId> {
        let mut cores = Vec::new();
        for role in roles {
            if !cores.contains(&role.core) {
                cores.push(role.core);
            }
        }
        cores
    }

    /// The loaded logical CPUs of `roles`, in role order.
    pub fn cpus(roles: &[Self]) -> Vec<CpuId> {
        roles.iter().map(|role| role.cpu).collect()
    }

    /// The roles of an R1 to R7 trial: one per listed CPU, every one running `workload`.
    ///
    /// R6 and R7 list one CPU per core, in `cores` order, each its own instance; R6's roles carry their core's
    /// turn among the cores in scheduling order. R1 to R5 list the CPUs of the one core, all in one instance
    /// that starts at the first listed CPU.
    pub fn legacy(
        regime: Regime,
        workload: WorkloadId,
        seed: u64,
        cores: &[CoreId],
        cpus: &[CpuId],
        topology: &[CoreInfo],
    ) -> Result<Vec<Self>, InvalidRoles> {
        if regime.has_roles() {
            return Err(InvalidRoles::NotLegacy { regime });
        }
        expect_catalog(regime, workload)?;
        if cores.is_empty() {
            return Err(InvalidRoles::NoCores);
        }
        let mut infos: Vec<&CoreInfo> = Vec::with_capacity(cores.len());
        for &core in cores {
            if infos.iter().any(|seen| seen.core == core) {
                return Err(InvalidRoles::DuplicateCore(core));
            }
            infos.push(core_info(topology, core)?);
        }
        let shape = InvalidRoles::LoadShape {
            regime,
            requested: cores.len(),
            cpus: cpus.len(),
        };
        let mut roles = Vec::with_capacity(cpus.len());
        if regime.instance_per_core() {
            if cpus.len() != cores.len() {
                return Err(shape);
            }
            let selected: Vec<CoreInfo> = infos.iter().map(|&info| info.clone()).collect();
            let turns = order(&selected);
            for (info, &cpu) in infos.iter().zip(cpus) {
                if !info.cpus.contains(&cpu) {
                    return Err(InvalidRoles::ForeignCpu(cpu));
                }
                let schedule = if regime == Regime::R6 {
                    let position = turns.iter().position(|&core| core == info.core);
                    PhaseSchedule::Burst {
                        position: position.ok_or(InvalidRoles::UnknownCore(info.core))?,
                    }
                } else {
                    steady_schedule(workload, seed)
                };
                roles.push(Self {
                    core: info.core,
                    cpu,
                    instance: cpu,
                    workload,
                    schedule,
                });
            }
        } else {
            let &[info] = infos.as_slice() else {
                return Err(shape);
            };
            let Some(&instance) = cpus.first() else {
                return Err(shape);
            };
            let schedule = steady_schedule(workload, seed);
            for (index, &cpu) in cpus.iter().enumerate() {
                if !info.cpus.contains(&cpu) {
                    return Err(InvalidRoles::ForeignCpu(cpu));
                }
                if cpus[..index].contains(&cpu) {
                    return Err(InvalidRoles::DuplicateCpu(cpu));
                }
                roles.push(Self {
                    core: info.core,
                    cpu,
                    instance,
                    workload,
                    schedule,
                });
            }
        }
        Ok(roles)
    }

    /// The roles of an R8 trial: `light`'s first CPU runs the light workload `class` names, R1 sustained or R3
    /// with random steps from `seed`, and the first CPU of each other core of its CCD runs `heavy`, an R2
    /// workload, sustained. One instance per core, cores in ascending order.
    pub fn neighbours(
        class: WorkloadId,
        light: CoreId,
        heavy: WorkloadId,
        seed: u64,
        topology: &[CoreInfo],
    ) -> Result<Vec<Self>, InvalidRoles> {
        expect_catalog(Regime::R8, class)?;
        expect_catalog(Regime::R2, heavy)?;
        let ccd = core_info(topology, light)?.ccd;
        let mut shared: Vec<&CoreInfo> = topology.iter().filter(|info| info.ccd == ccd).collect();
        shared.sort_by_key(|info| info.core);
        if shared.len() < 2 {
            return Err(InvalidRoles::NoNeighbours(light));
        }
        let running = class.runs();
        shared
            .into_iter()
            .map(|info| {
                let cpu = first_cpu(info)?;
                let (workload, schedule) = if info.core == light {
                    (running, steady_schedule(running, seed))
                } else {
                    (heavy, PhaseSchedule::Sustained {})
                };
                Ok::<_, InvalidRoles>(Self {
                    core: info.core,
                    cpu,
                    instance: cpu,
                    workload,
                    schedule,
                })
            })
            .collect()
    }

    /// The roles of an R9 trial: every logical CPU of every core, in ascending core and listed CPU order, runs
    /// the R1 workload `class` names as its own instance. All CPUs run in each all-thread phase; only `serial`
    /// runs in each one-thread phase.
    pub fn all_threads(
        class: WorkloadId,
        serial: CpuId,
        topology: &[CoreInfo],
    ) -> Result<Vec<Self>, InvalidRoles> {
        expect_catalog(Regime::R9, class)?;
        let mut cores: Vec<&CoreInfo> = topology.iter().collect();
        cores.sort_by_key(|info| info.core);
        if cores.is_empty() {
            return Err(InvalidRoles::NoCores);
        }
        if let Some(info) = cores.iter().find(|info| info.cpus.is_empty()) {
            return Err(InvalidRoles::CoreWithoutCpu(info.core));
        }
        if !cores.iter().any(|info| info.cpus.contains(&serial)) {
            return Err(InvalidRoles::UnknownCpu(serial));
        }
        let workload = class.runs();
        let mut roles = Vec::new();
        for info in cores {
            for &cpu in &info.cpus {
                if roles.iter().any(|role: &Self| role.cpu == cpu) {
                    return Err(InvalidRoles::DuplicateCpu(cpu));
                }
                roles.push(Self {
                    core: info.core,
                    cpu,
                    instance: cpu,
                    workload,
                    schedule: PhaseSchedule::Alternate {
                        phase_s: R9_PHASE_S,
                        serial: cpu == serial,
                    },
                });
            }
        }
        Ok(roles)
    }
}

impl TrialSpec {
    /// Checks the trial's roles against the topology: they must be exactly the ones the trial's regime derives
    /// for its request, and its cores and CPUs must be the ones they load. An R8 trial's light core and heavy
    /// workload and an R9 trial's serial CPU come from the roles themselves.
    pub fn check_roles(&self, topology: &[CoreInfo]) -> Result<(), InvalidRoles> {
        let first = self.roles.first().ok_or(InvalidRoles::NoRoles)?;
        let derived = match self.regime {
            Regime::R8 => {
                let running = self.workload.runs();
                let light = self
                    .roles
                    .iter()
                    .find(|role| role.workload == running)
                    .ok_or(InvalidRoles::NoAnchor)?;
                let heavy = self
                    .roles
                    .iter()
                    .find(|role| role.workload != running)
                    .map_or(first.workload, |role| role.workload);
                TrialRole::neighbours(self.workload, light.core, heavy, self.seed, topology)?
            }
            Regime::R9 => {
                let serial = self
                    .roles
                    .iter()
                    .find(|role| {
                        matches!(role.schedule, PhaseSchedule::Alternate { serial: true, .. })
                    })
                    .ok_or(InvalidRoles::NoAnchor)?;
                TrialRole::all_threads(self.workload, serial.cpu, topology)?
            }
            Regime::R1
            | Regime::R2
            | Regime::R3
            | Regime::R4
            | Regime::R5
            | Regime::R6
            | Regime::R7 => TrialRole::legacy(
                self.regime,
                self.workload,
                self.seed,
                &self.cores,
                &self.cpus,
                topology,
            )?,
        };
        if derived != self.roles {
            return Err(InvalidRoles::RolesDiffer);
        }
        if TrialRole::cores(&self.roles) != self.cores || TrialRole::cpus(&self.roles) != self.cpus
        {
            return Err(InvalidRoles::LoadDiffers);
        }
        Ok(())
    }
}
