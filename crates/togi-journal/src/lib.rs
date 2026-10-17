//! The session journal. Other crates read journals and simulate them in memory through [`fs::MemFs`]; the only way
//! they open a journal on disk for writing is [`StateDir::lock`], because the disk filesystem is crate-private:
//!
//! ```compile_fail,E0603
//! let _ = togi_journal::fs::DiskFs::default();
//! ```

mod build;
mod defect_id;
mod dir;
mod event;
mod filter;
pub mod fs;
mod header;
mod journal;
mod payload;
mod reader;
mod receipt;
mod session_id;
mod skipped;
mod state;
#[cfg(test)]
mod testing;
mod unknown;

#[cfg(test)]
mod tests;

pub use build::{Build, Dimension, Operation, Refusal, SCHEMA, Verdict};
pub use defect_id::{DefectId, FixStamp};
pub use dir::{
    ArchiveKind, DirError, Lifetime, LockError, Marker, StateDir, StateError, recorded_build,
    recorded_build_in, session_entries_in,
};
pub use event::{DecodeError, Event, Seq, Stamp};
pub use filter::{Filter, InvalidKindSelector, KindSelector};
pub use fs::MemFs;
pub use journal::{AppendError, ArchiveError, CloseError, Journal, OpenError, WriteFailure};
pub use payload::*;
pub use reader::{
    EventReader, FileError, LookupError, ReadError, Recording, Torn, recorded_context,
    recorded_context_in,
};
pub use receipt::{SmuIntentReceipt, SmuWritten};
pub use session_id::{InvalidSessionId, SessionId};
pub use skipped::{IgnoredFields, Skipped, SkippedEvent};
pub use state::{
    BackendPaths, CoreFacts, DeadEndRef, DecisionRef, FoldError, InFlight, SessionInfo, State,
};
