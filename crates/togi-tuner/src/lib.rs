//! The tuning rules: the core topology, topology-complete profiles, the failure points and
//! combinations that bound them with the safe optimum they allow, and crash classification.

mod checking;
mod class;
mod config;
mod constraints;
mod cores;
mod crash;
mod dashboard;
mod decide;
mod depth;
mod error;
mod evidence;
mod failures;
mod flight;
mod forecast;
mod hunt;
mod later;
mod ledger;
mod optimum;
mod phase_plan;
mod phases;
mod plan;
mod profile;
mod projection;
mod query;
mod r7;
mod requirement;
mod rounds;
mod search;
mod skip;
mod state;
mod topology;
mod zero;

#[cfg(test)]
mod bios_tests;
#[cfg(test)]
mod chain_tests;
#[cfg(test)]
mod checking_state_tests;
#[cfg(test)]
mod checking_tests;
#[cfg(test)]
mod dashboard_tests;
#[cfg(test)]
mod drain_tests;
#[cfg(test)]
mod fold_tests;
#[cfg(test)]
mod forecast_tests;
#[cfg(test)]
mod later_tests;
#[cfg(test)]
mod ledger_tests;
#[cfg(test)]
mod optimum_tests;
#[cfg(test)]
mod phase_plan_tests;
#[cfg(test)]
mod phase_tests;
#[cfg(test)]
mod projection_tests;
#[cfg(test)]
mod prop_tests;
#[cfg(test)]
mod query_tests;
#[cfg(test)]
mod r7_attribution_tests;
#[cfg(test)]
mod r7_escalation_tests;
#[cfg(test)]
mod r7_probe_tests;
#[cfg(test)]
mod r7_resume_tests;
#[cfg(test)]
mod r7_target_tests;
#[cfg(test)]
mod rerun_tests;
#[cfg(test)]
mod rounds_tests;
#[cfg(test)]
mod rule_tests;
#[cfg(test)]
mod schedule_tests;
#[cfg(test)]
mod skip_tests;
#[cfg(test)]
mod testkit;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod zero_tests;

pub use checking::{R7PartRole, r7_part_passes};
pub use class::{ClassKey, LoadKey, Secs};
pub use constraints::{
    Combination, CombinationId, ConstraintError, Constraints, FailurePoint, Insertion, Limit,
    Reached,
};
pub use crash::{CrashFacts, CrashKind, classify_crash, recorded_kind};
pub use dashboard::{
    CyclePart, CyclePlan, CycleStep, GroupState, HuntGroup, HuntPart, HuntPlan, MemberProbe,
    RerunPlan, SearchTurn, TrialRequirement,
};
pub use depth::Depth;
pub use error::FoldError;
pub use evidence::{
    BackendId, Count, Evidence, IdleContradiction, Observation, SessionRef, Window,
};
pub use flight::TrialHistory;
pub use forecast::{AllPass, Between, Branch, Forecast, Premise, Uncertainty};
pub use later::bios::{BiosProfile, Confirmed, Unconfirmed};
pub use phase_plan::{Candidate, PhaseNo, PhasePlan};
pub use plan::{
    Action, DecisionAction, DecisionPayload, Load, PartNo, Requirement, StepNo, TrialContext,
    TrialPlan, WorkloadChoice,
};
pub use profile::{Profile, ProfileError, TopologyMismatch};
pub use projection::{
    CheckingView, ConstraintView, CoreView, DeepeningCheck, DeepeningView, Projection,
    TemperaturePeak,
};
pub use query::{NotReady, replay};
pub use r7::query::{ChainOrder, R7CoreStatus};
pub use state::State;
pub use topology::{Ranking, RankingError, Topology, TopologyError};

/// The strategy ruleset this build tunes by (`docs/spec/tuner.md`, Ruleset): search strides, offset range,
/// phases, evidence, failure-point and combination rules, hunts and checking coverage. Bump it when any of
/// them changes; a session started under another ruleset is archived into a seeded new session. It starts at
/// 1 because the Rust build starts its own journal schema and never reads a Go journal (`journal.md`).
pub const RULESET: u32 = 2;

/// The evidence epoch this build records trial outcomes under (`docs/spec/tuner.md`, Ruleset): workload
/// content, generated backend configuration, intended durations and pass/failure detection. Bump it when a
/// change makes recorded passes incomparable with current trials; a transition then drops passes from other
/// epochs and keeps eligible failures. It starts at 1 for the same reason as [`RULESET`].
pub const EVIDENCE_EPOCH: u32 = 1;
