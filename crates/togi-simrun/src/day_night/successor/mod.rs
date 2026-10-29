//! Policy-neutral successor experiment records, independent of simulator truth implementations.
//!
//! Days are one-based continuous calendar coordinates. Intervals are half-open: a full
//! 730-day observation covers `[1, 731)`. Profile intervals split at truth transitions and
//! the original first-finish boundary; restoration learning overlaps post-finish exposure.
//! Application boundaries and CPU-suspect events used to verify clean holds also split
//! intervals: the scorer never prorates active hours across an unresolved interruption.
//! Optional invocation timings identify whole execution attempts; equal identities must
//! carry equal elapsed times, and absent timings remain unavailable rather than zero.
//! The physical profile timeline explicitly distinguishes ordinary daily deployment from
//! night trials. Daily safety, clean holds and K/T/H exclude night trial intervals, whose
//! exact tested profiles and truth-side risk remain recorded. Missing policy allowance or
//! classification is explicit absence, never a synthetic zero account or suspect decision.
//! `Record::validate` checks representation and accounting; the benchmark owns acceptance.
//! Source-allocation consistency scales the recorded numerical tolerance to ratio magnitude,
//! including rejected profiles; this never relaxes exact admissibility membership.

mod policy;
mod profile;
#[cfg(test)]
mod tests;
mod validate;

pub use policy::{
    AllowanceLedger, Classification, FinishReason, LearningEvent, PolicyReport, PolicyUnit,
    StepIdentity, UnitFinish, UnitPhase,
};
pub use validate::ValidationError;

use serde::{Deserialize, Serialize};
use togi_machine::{CoreInfo, PerCore, object_only};

/// Dense registered tuning-unit index, not a CCD identifier.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "u16", into = "u16")]
pub struct UnitId(u16);

impl UnitId {
    /// Creates an index representable by the physical-core container.
    pub fn new(index: u16) -> Result<Self, ValidationError> {
        if index < 16 {
            Ok(Self(index))
        } else {
            Err(ValidationError::Invalid("unit index"))
        }
    }
    pub const fn get(self) -> u16 {
        self.0
    }
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}
impl TryFrom<u16> for UnitId {
    type Error = ValidationError;
    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<UnitId> for u16 {
    fn from(value: UnitId) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnitMode {
    CcdUniform,
    PerCore,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Split {
    Dev,
    Holdout,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunProfile {
    ReducedDev,
    Registered,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Learning,
    PostFinish,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Period {
    Day,
    Night,
}

object_only! {
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Record {
        pub provenance: Provenance,
        pub epochs: Vec<Epoch>,
        pub intervals: Vec<ProfileInterval>,
        pub events: Vec<MachineEvent>,
        pub units: Vec<UnitSummary>,
        pub policy: PolicyReport,
        pub steps: Vec<ProfileStep>,
        pub restorations: Vec<FirmwareRestoration>,
        pub accounts: RiskAccounts,
        pub exposure: Exposure,
        pub outcome: Outcome,
        pub runtime: Runtime,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Provenance {
        pub registration_id: String,
        pub registration_sha256: String,
        pub source_revision: String,
        pub simulator_revision: String,
        pub runner_revision: String,
        pub report_revision: String,
        /// Absent before execution; a registered score requires the pinned hash.
        pub binary_sha256: Option<String>,
        pub input_sha256: Option<String>,
        pub policy_identity: String,
        pub policy_source_sha256: String,
        pub parameters_sha256: String,
        pub slope: f64,
        pub split: Split,
        pub seed: u64,
        pub scenario: String,
        pub unit_mode: UnitMode,
        pub topology: Vec<CoreInfo>,
        pub membership: Vec<PolicyUnit>,
        pub run_profile: RunProfile,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Epoch {
        pub id: String,
        pub start_day: f64,
        pub end_day: f64,
        pub context: String,
        pub reference: DepthReference,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DepthReference {
        pub operational_max: u32,
        /// A certified upper bound equal to the operational value establishes the optimum.
        pub certified_upper_bound: u32,
        pub physical_max: u32,
        pub per_unit_max_feasible: Vec<u8>,
        pub tolerance: f64,
        pub certificate: String,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ProfileInterval {
        pub start_day: f64,
        pub end_day: f64,
        pub period: Period,
        #[serde(deserialize_with = "profile::deserialize")]
        pub profile: PerCore<u8>,
        pub membership: Vec<PolicyUnit>,
        pub epoch: String,
        pub verified_daily_active_hours: f64,
        pub phase: Phase,
        pub restoration_learning: Option<String>,
        pub admissibility: Admissibility,
        /// Integrated actual-history risk, not a periodic reference-rate estimate.
        pub sources: Vec<SourceRisk>,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Admissibility {
        pub joint_ratio: f64,
        pub unit_ratios: Vec<f64>,
        pub member: bool,
    }

}

object_only! {
    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    #[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
    pub enum RiskSource {
        Unit { unit: UnitId },
        Interaction { identity: String },
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct SourceRisk {
        pub source: RiskSource,
        pub expected_failures: f64,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    #[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
    pub enum TruthCause {
        Co { source: RiskSource, implicated_units: Vec<UnitId> },
        Background {},
        NonCpu {},
        Unattributed {},
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct MachineEvent {
        pub policy_event: LearningEvent,
        pub period: Period,
        pub truth: TruthCause,
        pub phase: Phase,
        pub restoration_learning: Option<String>,
        #[serde(deserialize_with = "profile::deserialize")]
        pub profile: PerCore<u8>,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct UnitSummary {
        pub unit: UnitId,
        pub finish: Option<UnitFinish>,
        pub promotions: u64,
        pub retreats: Retreats,
        pub hold_hours: f64,
        pub qualification_launches: u64,
        pub qualification_hours: f64,
        pub night_hours: f64,
        pub ceiling_evidence: Vec<CeilingEvidence>,
    }

    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Retreats {
        pub co_counts: u64,
        pub background_counts: u64,
        pub non_cpu_counts: u64,
        pub unattributed_counts: u64,
        pub steps: u64,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    #[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
    pub enum EdgeObservation {
        Measured { u_depth: u8 },
        CensoredWriteLimit {},
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct CeilingEvidence {
        pub day: f64,
        pub epoch: String,
        pub edge: EdgeObservation,
        pub ceiling_depth: u8,
        pub witnesses: Vec<String>,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ProfileStep {
        pub id: String,
        pub day: f64,
        #[serde(deserialize_with = "profile::deserialize")]
        pub before: PerCore<u8>,
        #[serde(deserialize_with = "profile::deserialize")]
        pub after: PerCore<u8>,
        pub kind: StepKind,
    }

}

object_only! {
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
    pub enum StepKind {
        Climb { unit: UnitId, ceiling_depth: u8, clean_hold_hours: f64, qualification: Qualification },
        Restoration { restoration: String },
        Retreat { event: Option<String>, units: Vec<UnitId> },
        StockReset { epoch: String },
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Qualification {
        pub id: String,
        pub day: f64,
        pub epoch: String,
        #[serde(deserialize_with = "profile::deserialize")]
        pub profile: PerCore<u8>,
        pub stock_controls: Vec<String>,
        pub fresh_witnesses: Vec<String>,
        pub reused_witnesses: Vec<String>,
        pub safety_checks_satisfied: bool,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct FirmwareRestoration {
        pub id: String,
        pub epoch: String,
        pub transition_day: f64,
        #[serde(deserialize_with = "profile::deserialize")]
        pub previous_profile: PerCore<u8>,
        pub membership: Vec<PolicyUnit>,
        pub stock_reset_day: f64,
        #[serde(deserialize_with = "profile::deserialize")]
        pub stock_readback: PerCore<u8>,
        pub outcome: RestorationOutcome,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
    pub enum RestorationOutcome {
        Blocked { reason: String },
        Applied { day: f64, qualification: Qualification, clean_stock_hours: f64, result: RestorationResult },
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
    pub enum RestorationResult {
        Retained {},
        Crashed {
            event: String,
            stock_step_back_day: f64,
            #[serde(deserialize_with = "profile::deserialize")]
            stock_readback: PerCore<u8>,
            stopped_units: Vec<UnitId>,
            witnesses: Vec<String>
        },
    }

}

object_only! {
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct RiskAccounts {
        pub learning: Vec<SourceAccount>,
        pub post_finish: Vec<SourceAccount>,
    }

    /// Unique true daily CO crashes, daily active machine-hours and actual-history hazard.
    /// Night failures remain separately identifiable by `MachineEvent::period`.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct SourceAccount {
        pub source: RiskSource,
        pub k: u64,
        pub active_hours: f64,
        pub expected_failures: f64,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Exposure {
        pub intended_seconds: f64,
        pub observed_seconds: f64,
        pub eligible_seconds: f64,
        pub onset_active_seconds: f64,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
    pub enum Outcome {
        CompleteConverged { observed_until_day: f64 },
        CompleteUnconvergedCrashFree { observed_until_day: f64 },
        CompleteUnconvergedWithCoCrashes { observed_until_day: f64 },
        Truncated { observed_until_day: f64, reason: String, lower_bound: PartialCounts },
        HarnessError { observed_until_day: f64, error: String },
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct PartialCounts {
        pub daily_interruptions: u64,
        pub daily_co_crashes: u64,
        pub active_hours: f64,
    }

    /// Whole invocation elapsed time, excluding compilation. Repeated records with the
    /// same identity refer to one timing observation, not additional elapsed time.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct InvocationTiming {
        pub id: String,
        pub elapsed_seconds: f64,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Runtime {
        pub wall_seconds: f64,
        pub compilation_seconds: f64,
        #[serde(default)]
        pub invocation: Option<InvocationTiming>,
        pub jobs: u32,
        pub resource_class: String,
        pub trial_launches: u64,
    }
}

impl Outcome {
    pub const fn is_complete(&self) -> bool {
        match self {
            Self::CompleteConverged { .. }
            | Self::CompleteUnconvergedCrashFree { .. }
            | Self::CompleteUnconvergedWithCoCrashes { .. } => true,
            Self::Truncated { .. } | Self::HarnessError { .. } => false,
        }
    }
    pub const fn observed_until_day(&self) -> f64 {
        match self {
            Self::CompleteConverged { observed_until_day }
            | Self::CompleteUnconvergedCrashFree { observed_until_day }
            | Self::CompleteUnconvergedWithCoCrashes { observed_until_day }
            | Self::Truncated {
                observed_until_day, ..
            }
            | Self::HarnessError {
                observed_until_day, ..
            } => *observed_until_day,
        }
    }
}

impl Record {
    /// Strict object decoding followed by representation/account validation.
    pub fn parse(bytes: &[u8]) -> Result<Self, ValidationError> {
        let record: Self = serde_json::from_slice(bytes)?;
        record.validate()?;
        Ok(record)
    }

    /// Returns unit depths only for uniform physical-core vectors. Never collapses readbacks.
    pub fn unit_depths(&self, profile: &PerCore<u8>) -> Result<Vec<u8>, ValidationError> {
        self.provenance
            .membership
            .iter()
            .map(|unit| {
                let first = unit
                    .cores
                    .first()
                    .and_then(|core| profile.get(*core))
                    .ok_or(ValidationError::Invalid("unit profile membership"))?;
                if unit
                    .cores
                    .iter()
                    .any(|core| profile.get(*core) != Some(first))
                {
                    return Err(ValidationError::Invalid("nonuniform unit profile"));
                }
                Ok(first)
            })
            .collect()
    }

    pub fn first_finish_day(&self) -> Option<f64> {
        self.units.first()?;
        self.units
            .iter()
            .map(|unit| unit.finish.as_ref().map(|finish| finish.day))
            .try_fold(1.0_f64, |day, next| next.map(|next| day.max(next)))
    }

    pub fn physical_core_mean_depth(profile: &PerCore<u8>) -> Option<f64> {
        let count = u32::try_from(profile.len()).ok()?;
        (count > 0).then(|| {
            profile
                .iter()
                .map(|(_, depth)| f64::from(depth))
                .sum::<f64>()
                / f64::from(count)
        })
    }
}
