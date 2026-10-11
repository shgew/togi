//! The operator configuration: JSON at [`DEFAULT_PATH`], read into a [`Config`] that is valid by
//! construction.
//!
//! Every setting is optional on the wire and keeps its default when omitted; the root and each
//! section must be a JSON object. Unknown members at any level, duplicate members, out-of-range
//! values and a core with both a start offset and a candidate solo limit are errors. Core ids are
//! canonical decimal object keys within 0..=15 and offsets are integers within [-50, 0]; nothing
//! is clamped. A backend path that is absent or `null` is not configured and an empty one is
//! rejected as not absolute; an empty `backend_user` is rejected.
//!
//! The serde shape of [`Config`] is part of the journal schema: `config.loaded` embeds it, and
//! reading the event validates it again. Serialization writes every setting, with unset backend
//! paths and `backend_user` as `null`, and leaves out the evidence trial count, which
//! deserialization recomputes from `miss` and `rate`. A change to the shape is a journal schema
//! decision.
//!
//! The crate does no I/O except [`Config::load`], and checks nothing against the machine: the
//! session checks override cores against the topology, and preflight checks the backend account.

mod config;
mod error;
mod overrides;
mod values;

pub use config::{Config, DEFAULT_PATH};
pub use error::{
    ConfigError, EmptySchedule, LoadError, ProbabilityError, RangeError, RelativeBackendPath,
    TrialsError,
};
pub use overrides::InitialOverride;
pub use values::{
    Backends, CheckingSchedule, DeadEndPolicy, DeadEndThreshold, EvidencePolicy, Probability,
    R7BudgetSeconds, TrialCount, TrialSeconds,
};

#[cfg(test)]
mod tests;
