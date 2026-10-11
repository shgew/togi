//! The tuning session: [`run`] takes a [`RunInput`] through startup, the tuning loop and the cleanup every exit
//! shares, and hands the journal back in [`Ended`]. [`reset_core`] and [`reset_all`] are the two journal commands
//! that change what a later run does.

mod appender;
mod begin;
mod boot;
mod close;
mod commands;
mod defects;
mod emergency;
mod error;
mod evidence;
mod execute;
mod publication;
mod recover;
mod recovery;
mod restore;
mod run;
mod run_trial;
mod session;
mod startup;
mod trial;
mod write;

#[cfg(test)]
mod testing;

pub use boot::BootError;
pub use commands::{CommandError, reset_all, reset_core};
pub use emergency::{Outcome as Emergency, Readback, Response, Withheld, Zeroed};
pub use error::{AppendFailure, FoldError, JournalCause, JournalFailure, SessionError};
pub use publication::PublicationPolicy;
pub use run::run;
pub use session::{ConfigSource, Ended, Prompt, PromptError, RunInput, Stop, StopPolicy};
pub use write::{ConstraintRefusal, ProfileFailure, Refused, WriteFailure};
