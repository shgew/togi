//! Known tuning defects: [`KNOWN_DEFECTS`] describes the affected decisions, and [`DefectState`]
//! folds journal events into the findings a session records.

mod registry;
mod state;

#[cfg(test)]
mod tests;

pub use registry::{Clause, Entry, KNOWN_DEFECTS, fixed_stamp, is_ordered};
pub use state::{DefectState, FindingView, PendingFinding};
