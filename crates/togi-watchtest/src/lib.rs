//! Deterministic simulated session journals for the dashboard, replay and status tests: a dev-dependency only.
//!
//! [`Scenario`] names every watch golden scenario that has a journal and builds it from a [`Base`] simulation run
//! once per process; [`install`] writes one as `events.jsonl`, while [`install_with_fs`] also supports memory fixtures.
//! The night bases run `togi run`'s session on the simulator; the calendar base runs Ruleset 13 over a simulated
//! month of days and nights, for the states a single night never reaches (qualification, day steps, reproduction).
//! The helpers a scenario is built from are public:
//! [`simulate`], the machines it runs, and the cut functions that stop a journal at the state a test names.
//! Every journal names [`build`], so a release's version bump changes no frame.

mod base;
mod cut;
mod error;
mod install;
mod scenario;
mod simulate;

pub use base::Base;
pub use cut::{before_crash_detected, cut_at, cut_passed_trial, cut_trial, cut_when};
pub use error::{Error, InstallError};
pub use install::{install, install_events, install_events_with_fs, install_with_fs};
pub use scenario::Scenario;
pub use simulate::{Halt, build, crash_trial, default_machine, machine, simulate};
