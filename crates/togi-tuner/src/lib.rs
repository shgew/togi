//! The tuning rules: the core topology, topology-complete profiles, the failure points and
//! combinations that bound them with the safe optimum they allow, and crash classification.

#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the public queries that compose the decision functions land with G6"
    )
)]

mod checking;
mod class;
mod config;
mod constraints;
mod cores;
mod crash;
mod decide;
mod depth;
mod error;
mod evidence;
mod failures;
mod flight;
mod later;
mod ledger;
mod optimum;
mod phases;
mod plan;
mod profile;
mod r7;
mod requirement;
mod search;
mod skip;
mod state;
mod topology;
mod zero;

#[cfg(test)]
mod chain_tests;
#[cfg(test)]
mod checking_state_tests;
#[cfg(test)]
mod checking_tests;
#[cfg(test)]
mod fold_tests;
#[cfg(test)]
mod later_tests;
#[cfg(test)]
mod ledger_tests;
#[cfg(test)]
mod optimum_tests;
#[cfg(test)]
mod phase_tests;
#[cfg(test)]
mod prop_tests;
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
pub use depth::Depth;
pub use error::FoldError;
pub use evidence::{
    BackendId, Count, Evidence, IdleContradiction, Observation, SessionRef, Window,
};
pub use flight::TrialHistory;
pub use plan::{
    Action, DecisionAction, DecisionPayload, Load, PartNo, Requirement, RequirementKind, StepNo,
    TrialContext, TrialPlan, WorkloadChoice,
};
pub use profile::{Profile, ProfileError, TopologyMismatch};
pub use r7::query::R7CoreStatus;
pub use state::State;
pub use topology::{Ranking, RankingError, Topology, TopologyError};
