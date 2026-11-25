//! The versioned inputs shared by the comparison harness and both simulator binaries.

use std::num::NonZeroU32;

use togi_machine::{TargetOffset, TrialSpec};
use togi_sim::{ScriptedMachine, ScriptedTopology, TrialOutcome};

/// Path relative to each source tree, checked before a comparison can build or reuse a run.
pub const CONTRACT_PATH: &str = "crates/togi-tool-sim/scripted-input-version";
/// Changes whenever the named sets or any of their fixed simulator inputs change.
pub const CONTRACT_VERSION: &str = include_str!("../scripted-input-version").trim_ascii();

/// One complete session script, not a workload on a modeled CPU.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, clap::ValueEnum)]
pub enum ScriptedCase {
    MappingStop,
    MceResume,
    PowerButtonReset,
    ProfilePlacement,
    NoNonzeroWork,
}

impl ScriptedCase {
    pub const ALL: [Self; 5] = [
        Self::MappingStop,
        Self::MceResume,
        Self::PowerButtonReset,
        Self::ProfilePlacement,
        Self::NoNonzeroWork,
    ];
    pub const SMOKE: [Self; 2] = [Self::MappingStop, Self::MceResume];

    pub const fn name(self) -> &'static str {
        match self {
            Self::MappingStop => "mapping-stop",
            Self::MceResume => "mce-resume",
            Self::PowerButtonReset => "power-button-reset",
            Self::ProfilePlacement => "profile-placement",
            Self::NoNonzeroWork => "no-nonzero-work",
        }
    }

    /// Upper bound on real tuning boots; the profile case has three scripted nights.
    pub const fn max_boots(self) -> NonZeroU32 {
        NonZeroU32::MIN.saturating_add(match self {
            Self::MappingStop => 0,
            Self::MceResume | Self::PowerButtonReset => 1,
            Self::ProfilePlacement | Self::NoNonzeroWork => 2,
        })
    }
}

/// An ordinary developer session with an explicit nonzero compute error and no fitted inputs.
pub fn developer_machine() -> ScriptedMachine {
    ScriptedMachine::new(
        ScriptedTopology::FourCores,
        TargetOffset::ZERO,
        developer_outcome,
    )
}

fn developer_outcome(trial: &TrialSpec, offsets: &[TargetOffset]) -> TrialOutcome {
    if trial.regime == togi_machine::Regime::R1
        && let Some(&core) = trial.cores.first()
        && offsets[core.index()].get() <= -10
    {
        return TrialOutcome::ComputationError {
            core,
            at: std::time::Duration::from_secs(7),
        };
    }
    TrialOutcome::Pass
}

#[cfg(test)]
mod tests;
