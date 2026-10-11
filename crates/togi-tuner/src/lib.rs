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
    Action, DecisionAction, DecisionPayload, Load, PartNo, Requirement, RequirementKind, StepNo,
    TrialContext, TrialPlan, WorkloadChoice,
};
pub use profile::{Profile, ProfileError, TopologyMismatch};
pub use projection::{
    CheckingView, ConstraintView, CoreView, DeepeningCheck, DeepeningView, Projection,
    TemperaturePeak,
};
pub use query::{NotReady, replay};
pub use r7::query::R7CoreStatus;
pub use state::State;
pub use topology::{Ranking, RankingError, Topology, TopologyError};
