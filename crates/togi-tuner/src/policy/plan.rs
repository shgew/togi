//! One launch the night policy asks the session for: the cell, the profile it runs at and the journal
//! intent that records it.

use togi_journal::{
    Activity, NightId, Placement, Seq, TaskId, Tested, TrialId, TrialIntent, WorkloadContent,
};
use togi_machine::{
    Condition, CoreId, CoreInfo, CoreSet, CpuId, InvalidRoles, PerCore, TargetOffset, TrialRole,
    WorkloadId,
};

use crate::night::{Cell, Purpose, Roles, Workloads};
use crate::{UnitProfile, UnitProfileError};

/// A reason a plan cannot become an intent or its roles.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PlanError {
    #[error("expanding the plan's profile to cores")]
    Profile(#[from] UnitProfileError),
    #[error("building the plan's trial roles")]
    Roles(#[from] InvalidRoles),
    #[error("core {0} is missing from the topology listing")]
    UnknownCore(CoreId),
    #[error("the cell's roles do not match its workloads")]
    Shape,
}

/// One trial the night policy wants launched. The session fills the content identities and journals
/// [`Self::intent`] before it starts the trial.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TrialPlan {
    pub night: NightId,
    pub task: TaskId,
    pub activity: Activity,
    pub cell: Cell,
    /// The whole tuning-group profile the trial runs at.
    pub profile: UnitProfile,
    /// What the trial's outcome is charged to: the isolated CCD at its offset, or the exact profile.
    pub tested: Tested,
    /// Whether this relaunches a cell whose earlier launch was inconclusive or truncated.
    pub retry: bool,
    /// The journal events this plan follows from.
    pub causes: Vec<Seq>,
}

impl TrialPlan {
    /// The placement contract the cell runs under.
    #[must_use]
    pub const fn placement(&self) -> Placement {
        match self.cell.purpose {
            Purpose::Isolated => Placement::Isolated,
            Purpose::Exact => Placement::Exact,
        }
    }

    /// Every workload the roles run, in the order `trial.intent.content` lists them, for the session to
    /// pair with their frozen content identities.
    #[must_use]
    pub fn workloads(&self) -> Vec<WorkloadId> {
        workloads_of(self.cell.workloads)
    }

    /// The `trial.intent` payload for trial `trial`, with `content` the identity of every workload in
    /// [`Self::workloads`]. Every R13 trial applies its whole profile at once, hence `Together`.
    pub fn intent(
        &self,
        trial: TrialId,
        cores: &[CoreInfo],
        content: Vec<WorkloadContent>,
    ) -> Result<TrialIntent, PlanError> {
        let profile: PerCore<TargetOffset> = self.profile.expand_topology(cores)?;
        let (core, loaded, offset) = match self.cell.roles {
            Roles::SingleCore { core } | Roles::SmtPair { core } => {
                (Some(core), CoreSet::default(), profile.get(core))
            }
            Roles::Neighbours { light: core } | Roles::AllThreads { core, .. } => {
                (Some(core), self.cell.loaded, None)
            }
            Roles::AllCoresIdle | Roles::Package(_) => (None, self.cell.loaded, None),
        };
        let workload = match self.cell.workloads {
            Workloads::One { workload } => workload,
            Workloads::LightHeavy { light, .. } => light,
        };
        Ok(TrialIntent {
            task_start: None,
            trial,
            kernel_cursor: None,
            kernel_error: None,
            profile,
            core,
            cores: loaded,
            offset,
            regime: self.cell.regime(),
            workload,
            duration_s: self.cell.duration.get(),
            condition: Condition::Together,
            activity: Some(self.activity),
            night: Some(self.night),
            task: Some(self.task),
            content,
            placement: Some(self.placement()),
            tested: Some(self.tested),
            retry: self.retry,
            cycle: None,
            step: None,
            hunt: None,
            group: None,
            round: None,
            rerun: false,
        })
    }

    /// The machine roles of the trial, the way the session's trial runner builds them for `seed`.
    pub fn roles(&self, seed: u64, cores: &[CoreInfo]) -> Result<Vec<TrialRole>, PlanError> {
        roles_of(&self.cell, seed, cores)
    }
}

pub(super) fn workloads_of(workloads: Workloads) -> Vec<WorkloadId> {
    match workloads {
        Workloads::One { workload } => vec![workload],
        Workloads::LightHeavy { light, heavy } => vec![light, heavy],
    }
}

/// The roles of `cell`: R1–R7 through the legacy constructor with one CPU per loaded core (both CPUs of
/// the core for R5), R8 and R9 through their role constructors, R9 on the cell's loaded cores only.
pub(super) fn roles_of(
    cell: &Cell,
    seed: u64,
    topology: &[CoreInfo],
) -> Result<Vec<TrialRole>, PlanError> {
    let first_cpu = |core: CoreId| -> Result<CpuId, PlanError> {
        topology
            .iter()
            .find(|info| info.core == core)
            .and_then(|info| info.cpus.first().copied())
            .ok_or(PlanError::UnknownCore(core))
    };
    let legacy = |workload: WorkloadId, cores: Vec<CoreId>, cpus: Vec<CpuId>| {
        TrialRole::legacy(cell.regime(), workload, seed, &cores, &cpus, topology)
            .map_err(PlanError::from)
    };
    match (cell.roles, cell.workloads) {
        (Roles::SingleCore { core }, Workloads::One { workload }) => {
            legacy(workload, vec![core], vec![first_cpu(core)?])
        }
        (Roles::SmtPair { core }, Workloads::One { workload }) => {
            let cpus = topology
                .iter()
                .find(|info| info.core == core)
                .map(|info| info.cpus.clone())
                .ok_or(PlanError::UnknownCore(core))?;
            legacy(workload, vec![core], cpus)
        }
        (Roles::AllCoresIdle | Roles::Package(_), Workloads::One { workload }) => {
            let cores: Vec<CoreId> = cell.loaded.iter().collect();
            let cpus = cores
                .iter()
                .map(|&core| first_cpu(core))
                .collect::<Result<Vec<_>, _>>()?;
            legacy(workload, cores, cpus)
        }
        (
            Roles::Neighbours { light },
            Workloads::LightHeavy {
                light: class,
                heavy,
            },
        ) => Ok(TrialRole::neighbours(class, light, heavy, seed, topology)?),
        (Roles::AllThreads { serial, .. }, Workloads::One { workload }) => {
            let loaded: Vec<CoreInfo> = topology
                .iter()
                .filter(|info| cell.loaded.contains(info.core))
                .cloned()
                .collect();
            Ok(TrialRole::all_threads(workload, serial, &loaded)?)
        }
        (
            Roles::SingleCore { .. }
            | Roles::SmtPair { .. }
            | Roles::AllCoresIdle
            | Roles::Package(_)
            | Roles::AllThreads { .. },
            Workloads::LightHeavy { .. },
        )
        | (Roles::Neighbours { .. }, Workloads::One { .. }) => Err(PlanError::Shape),
    }
}
