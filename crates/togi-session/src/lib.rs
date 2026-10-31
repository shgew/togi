//! The tuning session: [`run`] takes a [`RunInput`] through startup, the tuning loop and the cleanup every exit
//! shares, and hands the journal back in [`Ended`]. [`reset_all`] is the journal command that changes what a later
//! run does; [`draft_record`] and [`append_record`] journal a crash no day observer saw.

mod appender;
mod begin;
mod boot;
mod close;
mod commands;
mod day_record;
mod defects;
mod emergency;
mod error;
mod evidence;
mod execute;
mod prepare;
mod publication;
mod recover;
mod recovery;
mod restore;
mod resume;
mod run;
mod run_trial;
mod session;
mod snapshot;
mod startup;
mod trial;
mod write;

#[cfg(test)]
mod testing;

#[cfg(test)]
mod whole;

pub use appender::AppendHook;
pub use boot::BootError;
pub use commands::{CommandError, reset_all};
pub use day_record::{
    AppendRecordError, Crash, Draft, Entered, OffsetSpec, RecordError, append_record, draft_record,
};
pub use emergency::{
    Outcome as Emergency, Readback, Response, Withheld, Zeroed, zero_and_read_back,
};
pub use error::{AppendFailure, FoldError, JournalCause, JournalFailure, SessionError};
pub use evidence::adjudicate_trial_end;
pub use prepare::{Manifest, PendingImport, PrepareError, Prepared, prepare, prepare_kept};
pub use publication::PublicationPolicy;
pub use resume::{Divergence, DocumentDivergence, ResumeCapsule};
pub use run::run;
pub use session::{ConfigSource, Ended, Panic, RunInput, Stop};
pub use snapshot::{ReadState, SnapshotError, SnapshotStatus, verify_snapshot};
pub use write::{
    DayAppliedProfile, DayRecovery, DayWriteError, ProfileFailure, Refused, WriteFailure,
    apply_day_profile,
};
