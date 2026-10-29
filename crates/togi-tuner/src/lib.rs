//! The Ruleset 14 tuner (`docs/spec/tuner-ruleset14.md`): per-unit edge ceilings and an experimental
//! applied-profile climb ([`climb`]), the night's mapping and reproduction rules ([`night`]), daily
//! observations and exact-profile holds ([`day`]), and the composed [`State`] that folds the journal
//! and answers what a night or day does next ([`policy`]). Statistical caps ([`cap`]) are diagnostics only.
//! The crate also holds the machine's core [`Topology`], the honest partial chains
//! R7 mapping derives ([`HonestChain`]), and crash classification for session recovery
//! ([`classify_crash`]).

pub mod cap;
mod class;
pub mod climb;
mod crash;
pub mod day;
pub mod night;
mod numbers;
mod partial;
pub mod policy;
mod topology;
pub mod unit;

#[cfg(test)]
mod honest_chain_tests;
#[cfg(test)]
mod tests;

pub use class::Secs;
pub use crash::{CrashFacts, CrashKind, classify_crash, recorded_kind};
pub use partial::{HonestChain, PartialBasis, UncoveredReason};
pub use policy::{
    Action, CapProjection, Contents, DayProjection, DayQuery, Decision, EndReason, EventProjection,
    FirmwareValidity, FoldError, HoldProjection, MappingProjection, NightProjection, NightQuery,
    NotReady, Params, PlanError, PolicyPayload, Projection, QualificationProgress,
    QualificationProjection, QualificationStage, ReproductionProjection, State, TrialPlan,
    UnitProjection, VetoProjection, VetoSource,
};
pub use policy::{
    ClimbCampaignProjection, ClimbDailyEvidence, ClimbEventProjection, ClimbGoalSupport,
    ClimbHoldProjection, ClimbPermissionMode, ClimbProfiles, ClimbProjection,
    ClimbProposalProjection, ClimbQualificationProjection, ClimbQualificationState,
    ClimbStepProjection, ClimbUnitProjection, ClimbUnitState, DiagnosticStatisticalProjection,
};
pub use topology::{Topology, TopologyError};
pub use unit::{Phase, TuningUnit, UnitId, UnitProfile, UnitProfileError, Units};

/// The strategy ruleset this build tunes by (`docs/spec/tuner.md`, Ruleset): its evidence, cap, night,
/// day and daily-event rules. Bump it when any of them changes; a session started under another ruleset is
/// archived and a fresh session maps from scratch, importing only raw day observations. It starts at 1
/// because the Rust build starts its own journal schema and never reads a Go journal (`journal.md`); 3 is
/// the Ruleset 13 design.
pub const RULESET: u32 = 3;

/// The evidence epoch this build records trial outcomes under (`docs/spec/tuner.md`, Ruleset): workload
/// content, generated backend configuration, intended durations and pass/failure detection. Bump it when a
/// change makes recorded passes incomparable with current trials; a session recorded under another epoch is
/// then archived and the new session starts fresh (`docs/spec/journal.md`, Transitions). It starts at 1 for
/// the same reason as [`RULESET`].
pub const EVIDENCE_EPOCH: u32 = 1;
