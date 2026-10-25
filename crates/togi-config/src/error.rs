use std::{io, path::PathBuf};

use togi_machine::Backend;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{value} is not within [{min}, {max}]")]
pub struct RangeError {
    pub value: i64,
    pub min: u32,
    pub max: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{path:?} is not an absolute path")]
pub struct RelativeBackendPath {
    pub backend: Backend,
    pub path: PathBuf,
}

/// A setting Ruleset 12 had that Ruleset 13 retired. The night policy owns what these tuned (start points,
/// trial lengths, evidence quotas, the checking schedule), so a configuration that sets one is refused instead of
/// silently ignored.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Retired {
    #[error("start_offsets")]
    StartOffsets,
    #[error("candidate_solo_limits")]
    CandidateSoloLimits,
    #[error("durations.search_trial_s")]
    SearchTrial,
    #[error("durations.short_trial_s")]
    ShortTrial,
    #[error("durations.checking_trial_s")]
    CheckingTrial,
    #[error("durations.checking_idle_s")]
    CheckingIdle,
    #[error("durations.checking_all_core_s")]
    CheckingAllCore,
    #[error("evidence.miss")]
    EvidenceMiss,
    #[error("evidence.rate")]
    EvidenceRate,
    #[error("checking.cycle")]
    CheckingCycle,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("parse config JSON")]
    Syntax(#[from] serde_json::Error),
    #[error("{0}: Ruleset 13 retired this setting; remove it from the configuration")]
    Retired(Retired),
    #[error("dead_ends.{field}")]
    DeadEnd {
        field: &'static str,
        source: RangeError,
    },
    #[error("backends.{}", source.backend)]
    Backend {
        #[from]
        source: RelativeBackendPath,
    },
    #[error("backend_user: must name an account; leave it out to configure none")]
    EmptyBackendUser,
}

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("cannot read config {}", path.display())]
    Read { path: PathBuf, source: io::Error },
    #[error("invalid config {}", path.display())]
    Parse { path: PathBuf, source: ConfigError },
}
