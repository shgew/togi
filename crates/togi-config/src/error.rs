use std::{io, path::PathBuf};

use togi_machine::{Backend, CoreId, InvalidCore, InvalidTarget};

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{value} is not within [{min}, {max}]")]
pub struct RangeError {
    pub value: i64,
    pub min: u32,
    pub max: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[error("{0} is not a number strictly between 0 and 1")]
pub struct ProbabilityError(pub f64);

#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[error("miss {miss} and rate {rate} do not give between 1 and 1000 trials per step")]
pub struct TrialsError {
    pub miss: f64,
    pub rate: f64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("the schedule must name at least one regime")]
pub struct EmptySchedule;

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{path:?} is not an absolute path")]
pub struct RelativeBackendPath {
    pub backend: Backend,
    pub path: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("parse config JSON")]
    Syntax(#[from] serde_json::Error),
    #[error("durations.{field}")]
    Duration {
        field: &'static str,
        source: RangeError,
    },
    #[error("evidence.{field}")]
    Probability {
        field: &'static str,
        source: ProbabilityError,
    },
    #[error("evidence")]
    Trials(#[from] TrialsError),
    #[error("dead_ends.{field}")]
    DeadEnd {
        field: &'static str,
        source: RangeError,
    },
    #[error("checking.cycle")]
    Schedule(#[from] EmptySchedule),
    #[error("{table}.{key:?}: a core id is a plain decimal number such as \"3\"")]
    CoreKey { table: &'static str, key: String },
    #[error("{table}")]
    Core {
        table: &'static str,
        source: InvalidCore,
    },
    #[error("{table}.{core} is set more than once")]
    DuplicateCore { table: &'static str, core: CoreId },
    #[error("{table}.{core}")]
    Offset {
        table: &'static str,
        core: CoreId,
        source: InvalidTarget,
    },
    #[error("start_offsets.{0} and candidate_solo_limits.{0}: set at most one per core")]
    CoreConflict(CoreId),
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
