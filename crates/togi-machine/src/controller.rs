/// Whether a trial uses the current scheduler's placement or the explicitly managed controller
/// placement contract. Managed trials are opt-in until the scheduler adopts that evidence contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerMode {
    Legacy,
    Managed {
        /// The caller's persistent trial ordinal, independent of workload rotation and pass quotas.
        rotation: u64,
        /// Which placement contract the trial's cell is trialled under.
        purpose: Purpose,
    },
}

/// The placement contract a cell is trialled under (`docs/spec/workloads.md`, Controller
/// placement).
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    /// Other CCDs at CO 0 and unloaded; the quieter mapping contract.
    Isolated,
    /// The exact profile, qualified at its own placement.
    Exact,
}

/// The controller evidence contract chosen by a session caller. Existing schedules retain
/// `Legacy` until they adopt managed placement and process-fault evidence together.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerContract {
    Legacy,
    Managed,
}

impl ControllerContract {
    /// Derives trial placement from the durable global trial ordinal, not a workload quota, and
    /// the purpose of the cell the trial runs.
    pub const fn for_trial(self, rotation: u64, purpose: Purpose) -> ControllerMode {
        match self {
            Self::Legacy => ControllerMode::Legacy,
            Self::Managed => ControllerMode::Managed { rotation, purpose },
        }
    }
}
