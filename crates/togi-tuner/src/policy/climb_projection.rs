//! Unit-generic, journal-derived presentation of bounded exploration.
//!
//! These are data, not permission calculations. The policy supplies ceilings, distances,
//! qualification and exact-profile rate bounds; displays must not derive authority from S.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use togi_journal::{
    ApplicationId, CampaignEventPhase, CampaignHistory, CampaignId, Classification, ClimbAction,
    ClimbBlocker, DayEventId, Edge, ExposureClass, FirmwareRestoration, Seq, UnitStopReason,
    UnitVector,
};
use togi_machine::{BiosContext, CoreSet, ObservedOffset, PerCore, TargetOffset};

use super::CapProjection;
use crate::{Phase, UnitId};

/// The only permission model: a hold and an exact-vector night certificate, never S.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClimbPermissionMode {
    #[default]
    ExperimentalHoldExactNightQualification,
}

/// An empty projection means that no campaign/profile has been reconstructed yet.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ClimbProjection {
    pub permission: ClimbPermissionMode,
    pub profiles: ClimbProfiles,
    pub units: Vec<ClimbUnitProjection>,
    /// Absence is unavailable history, not a fresh unused allowance.
    pub campaign: Option<ClimbCampaignProjection>,
    /// Only compatible daily observations of the current exact applied vector.
    pub daily: Option<ClimbDailyEvidence>,
    pub firmware_restoration: Option<FirmwareRestoration>,
}

/// Applied/read-back, proposed and paused-saved values are deliberately independent.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClimbProfiles {
    pub applied: Option<UnitVector>,
    pub read_back: Option<PerCore<ObservedOffset>>,
    pub proposed: Option<UnitVector>,
    pub paused_saved: Option<UnitVector>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ClimbUnitProjection {
    pub unit: UnitId,
    pub phase: Phase,
    pub parent: Option<UnitId>,
    pub cores: CoreSet,
    pub inherited_start: TargetOffset,
    pub applied: TargetOffset,
    /// Unknown, observed failure with witness, or write-limit censoring with witnesses.
    pub edge: Edge,
    pub ceiling: Option<TargetOffset>,
    /// D minus C, supplied by policy; unknown C has no distance.
    pub remaining: Option<u32>,
    pub hold: ClimbHoldProjection,
    pub next: Option<ClimbProposalProjection>,
    pub state: ClimbUnitState,
    pub latest_step: Option<ClimbStepProjection>,
    pub diagnostic_s: Option<DiagnosticStatisticalProjection>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClimbHoldProjection {
    pub credited_s: u64,
    pub required_s: u64,
    pub witnesses: Vec<Seq>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClimbProposalProjection {
    pub profile: UnitVector,
    pub qualification: ClimbQualificationProjection,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClimbQualificationProjection {
    pub state: ClimbQualificationState,
    pub outstanding: Vec<ExposureClass>,
    pub witnesses: Vec<Seq>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClimbQualificationState {
    Missing,
    InProgress,
    Complete { certificate: Seq },
    Failed { witnesses: Vec<Seq> },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClimbUnitState {
    Climbing,
    Held {
        reason: Vec<ClimbBlocker>,
    },
    Finished {
        reason: UnitStopReason,
        at: Timestamp,
        profile: UnitVector,
        last_clean: TargetOffset,
        witnesses: Vec<Seq>,
    },
}

/// The latest successfully applied change, with its exact before/after vectors.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClimbStepProjection {
    pub application: ApplicationId,
    pub at: Timestamp,
    pub action: ClimbAction,
    pub before: UnitVector,
    pub after: UnitVector,
    pub witnesses: Vec<Seq>,
}

/// Statistical S is retained solely as a non-authorizing diagnostic.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DiagnosticStatisticalProjection {
    pub cap: CapProjection,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClimbCampaignProjection {
    pub campaign: CampaignId,
    pub history: CampaignHistory,
    pub limit: u32,
    /// Lifetime raw charges: reclassification never refunds these.
    pub consumed: u32,
    pub remaining: u32,
    pub provisional: u32,
    pub reclassified: u32,
    pub exhausted: bool,
    /// Every unique event, including non-CPU and post-finish observations.
    pub events: Vec<ClimbEventProjection>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ClimbEventProjection {
    pub event: DayEventId,
    pub aliases: Vec<DayEventId>,
    pub phase: CampaignEventPhase,
    pub unit_phase: Phase,
    pub profile: UnitVector,
    pub provisional: bool,
    pub classification: Classification,
    pub reclassified: bool,
    pub witnesses: Vec<Seq>,
}

/// Pointwise, one-sided 95% exact Poisson bound under a fixed-rate model.
/// No qualification, stock-baseline or previous-profile hours belong in this record.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ClimbDailyEvidence {
    pub profile: UnitVector,
    pub firmware: BiosContext,
    pub epoch: u32,
    pub observed_s: u64,
    pub clean_s: u64,
    pub crashes: u32,
    /// None without observed time; never display that absence as a zero rate.
    pub poisson_upper_per_h: Option<f64>,
    /// Supplied only from this exact vector's own compatible daily observations.
    pub goal: ClimbGoalSupport,
    pub witnesses: Vec<Seq>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClimbGoalSupport {
    InsufficientEvidence,
    MeetsGoal,
}
