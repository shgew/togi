//! Trial evidence extracted from a session journal: [`extract`] produces [`SessionFacts`] containing
//! trial classes, observations and reset cutoffs for consumers of recorded evidence.

mod error;
mod extract;
mod fact;
mod session;

#[cfg(test)]
mod tests;

pub use error::FactsError;
pub use extract::extract;
pub use fact::{
    BackendIdentity, Decisive, Fact, FactId, FactSource, Observation, Telemetry, TrialClass,
};
pub use session::{Effect, Reset, ResetCutoffs, SessionFacts, TrialActivity};
