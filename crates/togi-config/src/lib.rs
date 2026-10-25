//! The operator configuration: JSON at [`DEFAULT_PATH`], read into a [`Config`] that is valid by
//! construction.
//!
//! Every setting is optional on the wire and keeps its default when omitted; the root must be a
//! JSON object, and each section an object or `null`, which keeps the section's defaults. Unknown
//! members at any level, duplicate members and out-of-range values are errors, and so is a setting
//! Ruleset 13 retired (`start_offsets`, `candidate_solo_limits`, `durations.*`, `checking.cycle`,
//! `evidence.*`), which the error names. A backend path that is absent or `null` is not configured
//! and an empty one is rejected as not absolute; an empty `backend_user` is rejected.
//!
//! The journal records a configuration as a `togi_journal::ConfigSnapshot`, a shape the journal
//! owns, so a change here cannot change the journal format. [`Config::snapshot`] writes every
//! effective setting into it; [`Config::from_snapshot`] reads one back and validates it again as a
//! configuration file is validated.
//!
//! The crate does no I/O except [`Config::load`], and checks nothing against the machine: preflight
//! checks the backend account.

mod config;
mod error;
mod values;

pub use config::{Config, DEFAULT_PATH};
pub use error::{ConfigError, LoadError, RangeError, RelativeBackendPath, Retired};
pub use values::{Backends, DeadEndPolicy, DeadEndThreshold};

#[cfg(test)]
mod tests;
