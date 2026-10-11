use std::time::Duration;

use togi_machine::{CoreId, CoreInfo, CpuId, LoadSchedule, Regime, TrialSpec, Workload};

use crate::id::{InvalidId, TrialId};

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum InvalidTrial {
    #[error(transparent)]
    Id(#[from] InvalidId),
    #[error("the trial duration is zero")]
    ZeroDuration,
    #[error("the trial names no core")]
    NoCores,
    #[error("core {0} is listed twice")]
    DuplicateCore(CoreId),
    #[error("core {0} is not in the topology")]
    UnknownCore(CoreId),
    #[error("core {0} has no logical CPU in the topology")]
    CoreWithoutCpu(CoreId),
    #[error("the trial names no logical CPU")]
    NoCpus,
    #[error("logical CPU {} belongs to none of the trial's cores", .0.0)]
    ForeignCpu(CpuId),
    #[error("the logical CPUs do not match the first CPU of each core")]
    CpusMismatch,
}

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
    pub name: InstanceName,
    pub core: CoreId,
    pub cpus: Vec<CpuId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Load {
    Continuous,
    Steps(LoadSchedule),
    /// R6: instance indices in the order their bursts run.
    Bursts { order: Vec<usize> },
}

#[derive(Clone, Debug)]
pub(crate) struct TrialPlan {
    pub id: TrialId,
    pub regime: Regime,
    pub workload: Workload,
    pub duration: Duration,
    pub instances: Vec<InstancePlan>,
    pub load: Load,
}

impl TrialPlan {
    pub(crate) fn build(spec: &TrialSpec, topology: &[CoreInfo]) -> Result<Self, InvalidTrial> {
        let _ = (spec, topology);
        unimplemented!()
    }

    pub(crate) fn per_core(&self) -> bool {
        self.regime.instance_per_core()
    }
}
