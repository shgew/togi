//! The operator configuration: JSON at [`DEFAULT_PATH`], read into a [`Config`] that is valid by
//! construction.
//!
//! Every setting is optional on the wire and keeps its default when omitted; the root must be a
//! JSON object, and each section an object or `null`, which keeps the section's defaults. Unknown
//! members at any level, duplicate members, out-of-range values and a core with both a start
//! offset and a candidate solo limit are errors. Core ids are canonical decimal object keys within
//! 0..=15 and offsets are integers within [-50, 0]; nothing is clamped. A backend path that is
//! absent or `null` is not configured and an empty one is rejected as not absolute; an empty
//! `backend_user` is rejected.
//!
//! The journal records a configuration as a `togi_journal::ConfigSnapshot`, a shape the journal
//! owns, so a change here cannot change the journal format. [`Config::snapshot`] writes every
//! effective setting into it; [`Config::from_snapshot`] reads one back and validates it again as a
//! configuration file is validated.
//!
//! The crate does no I/O except [`Config::load`], and checks nothing against the machine: the
//! session checks override cores against the topology, and preflight checks the backend account.

mod config;
mod error;
mod overrides;
mod values;

pub use config::{Config, DEFAULT_PATH};
pub use error::{
    ConfigError, InvalidSchedule, LoadError, ProbabilityError, RangeError, RelativeBackendPath,
    TrialsError,
};
pub use overrides::InitialOverride;
pub use values::{
    Backends, CheckingSchedule, DeadEndPolicy, DeadEndThreshold, EvidencePolicy, FULL_CYCLE,
    Probability, R7BudgetSeconds, TrialCount, TrialSeconds,
};

#[cfg(test)]
mod tests;
