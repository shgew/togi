//! What a transition carries into the next session, selected without I/O.
//!
//! The session lists the archived sources newest first and implements [`Archives`]; [`select`] folds each source
//! once into a [`SourceSummary`], reading it only while a walk needs it and never one behind the reset boundary.
//! The [`PendingCarry`] it returns holds no facts: [`PendingCarry::resolve`] turns it into a [`ResolvedCarry`] once
//! the session knows its BIOS context, backends and evidence epoch. The session appends the result.

mod archives;
mod resolve;
mod seeds;
mod select;
mod summary;

#[cfg(test)]
mod tests;

pub use archives::{Archives, CarryError};
pub use resolve::{ResolvedCarry, copied_identity};
pub use select::{PendingCarry, select, settled};
pub use summary::SourceSummary;
