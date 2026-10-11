//! The tuning rules: the core topology, topology-complete profiles, the failure points and
//! combinations that bound them with the safe optimum they allow, and crash classification.

#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the public queries that compose the decision functions land with G6"
    )
)]

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
mod ledger;
mod optimum;
mod phases;
mod plan;
mod profile;
mod requirement;
mod search;
mod skip;
mod state;
mod topology;

#[cfg(test)]
mod ledger_tests;
#[cfg(test)]
mod phase_tests;
#[cfg(test)]
mod prop_tests;
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

pub use class::{ClassKey, Secs};
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
pub use profile::{Profile, ProfileError};
pub use state::State;
pub use topology::{Ranking, RankingError, Topology, TopologyError};
