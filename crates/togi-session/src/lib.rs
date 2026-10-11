//! The tuning session. This part holds what every session step is built on: the journal appender, the run's recovery
//! fold, the pure evidence rules, the tuning-boot handoff, and the two ways an offset reaches the SMU: the write
//! transaction in [`write`] and the journal-failure emergency in [`emergency`].

pub mod appender;
pub mod boot;
pub mod emergency;
pub mod error;
pub mod evidence;
pub mod recovery;
pub mod trial;
pub mod write;

#[cfg(test)]
mod testing;

pub use error::{AppendFailure, Flow, FoldError, JournalFailure, SessionError, Step};
