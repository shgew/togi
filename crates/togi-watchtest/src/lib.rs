//! Deterministic simulated session journals for the dashboard, replay and status tests: a dev-dependency only.
//!
//! [`Scenario`] names every watch golden scenario that has a journal and builds it from a [`Base`] simulation run
//! once per process; [`install`] writes one as `events.jsonl`. The helpers a scenario is built from are public:
//! [`simulate`] and [`probe_machine`], and the cut functions that stop a journal at the state a test names.
//! Every journal names [`build`], so a release's version bump changes no frame.

mod base;
mod cut;
mod error;
mod install;
mod rerun;
mod scenario;
mod simulate;

pub use base::Base;
pub use cut::{
    before_crash_detected, cut_at, cut_passed_checking_trial, cut_passed_trial, cut_trial,
    cut_when, hunt_with_unconfirmed_profile,
};
pub use error::{Error, InstallError};
pub use install::install;
pub use rerun::{CycleCompletedByRerun, cycle_completed_by_rerun};
pub use scenario::Scenario;
pub use simulate::{
    Stop, Until, build, crash_trial, default_machine, machine, probe_machine, simulate,
};
