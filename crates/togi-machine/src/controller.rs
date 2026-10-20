/// Whether a trial uses the current scheduler's placement or the explicitly managed controller
/// placement contract. Managed trials are opt-in until the scheduler adopts that evidence contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerMode {
    Legacy,
    /// The caller's persistent trial ordinal, independent of workload rotation and pass quotas.
    Managed {
        rotation: u64,
    },
}

/// The controller evidence contract chosen by a session caller. Existing schedules retain
/// `Legacy` until they adopt managed placement and process-fault evidence together.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerContract {
    Legacy,
    Managed,
}

impl ControllerContract {
    /// Derives trial placement from the durable global trial ordinal, not a workload quota.
    pub const fn for_trial(self, rotation: u64) -> ControllerMode {
        match self {
            Self::Legacy => ControllerMode::Legacy,
            Self::Managed => ControllerMode::Managed { rotation },
        }
    }
}
