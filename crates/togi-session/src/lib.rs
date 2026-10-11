//! The tuning session: the run in [`session`], and what every session step is built on: the journal appender, the
//! run's recovery fold, the pure evidence rules, the tuning-boot handoff, and the two ways an offset reaches the SMU:
//! the write transaction in [`write`] and the journal-failure emergency in [`emergency`].

pub mod appender;
mod begin;
pub mod boot;
mod defects;
pub mod emergency;
pub mod error;
pub mod evidence;
pub mod recover;
pub mod recovery;
mod restore;
pub mod session;
mod startup;
pub mod trial;
pub mod write;

#[cfg(test)]
mod testing;

pub use error::{AppendFailure, Flow, FoldError, JournalFailure, SessionError, Step};
pub use session::{Control, Ended, Outcome, RunInput, Session, Stop, StopPolicy};
