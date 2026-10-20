/// Whether a trial uses the current scheduler's placement or the explicitly managed controller
/// placement contract. Managed trials are opt-in until the scheduler adopts that evidence contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerMode {
    Legacy,
    /// The caller's persistent trial ordinal, independent of workload rotation and pass quotas.
    Managed { rotation: u64 },
}
