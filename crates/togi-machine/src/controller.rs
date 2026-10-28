/// The managed controller placement request for a trial.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ControllerMode {
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
