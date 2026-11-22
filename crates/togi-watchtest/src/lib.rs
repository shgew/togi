//! Deterministic simulated session journals for the dashboard, replay and status tests: a dev-dependency only.
//!
//! [`Scenario`] names every watch golden scenario that has a journal and builds it from a [`Base`] simulation run
//! once per process; [`install`] writes one as `events.jsonl`, while [`install_with_fs`] also supports memory fixtures.
//! The night bases run the real session to journal milestones with explicit simulator outcomes.
//! The reproduction and filler bases stage desktop observations to exercise promotion, profile application
//! and the following night's work without running a calendar.
//! Every journal names [`build`], so a release's version bump changes no frame.

mod base;
mod cut;
mod error;
mod install;
mod scenario;
mod simulate;
mod staged_day;

pub use base::Base;
pub use cut::{before_crash_detected, cut_at, cut_passed_trial, cut_trial, cut_when};
pub use error::{Error, InstallError};
pub use install::{install, install_events, install_events_with_fs, install_with_fs};
pub use scenario::Scenario;
pub use simulate::build;
