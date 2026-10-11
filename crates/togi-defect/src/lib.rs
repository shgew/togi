mod registry;
mod state;

#[cfg(test)]
mod tests;

pub use registry::{Clause, DefectId, Entry, FixStamp, KNOWN_DEFECTS, fixed_stamp, is_ordered};
pub use state::{DefectState, FindingView, Guidance, PendingFinding};
