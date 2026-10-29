//! Policy-owned observations. These types deliberately contain no simulator truth.

use serde::{Deserialize, Serialize};
use togi_machine::{CoreId, PerCore, object_only};

use super::{Phase, UnitId};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FinishReason {
    Ceiling,
    CpuSuspectStop,
    AllowanceExhausted,
    InheritedStop,
    FirmwareRestorationStop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Classification {
    CpuSuspect,
    NonCpu,
    Unanswered,
    Unexplained,
    /// Metric-side absence: the policy did not report a classification.
    Unreported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnitPhase {
    Coarse,
    Core,
}

object_only! {
    /// A blocked/unqualified unit has no finish; it is not a ceiling completion.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct UnitFinish {
        pub unit: UnitId,
        pub reason: FinishReason,
        /// One-based calendar day, including a fractional day when necessary.
        pub day: f64,
        /// Include the exact triggering event id for a crash-induced finish. That event
        /// remains learning even when the finish and immediate retreat share its timestamp.
        pub witnesses: Vec<String>,
    }

    /// Consumption is uncapped. Reclassification never refunds a charge.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct AllowanceLedger {
        pub limit: u64,
        pub consumed: u64,
        pub provisional: u64,
        pub reclassified: u64,
        pub exhausted_day: Option<f64>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    #[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
    pub enum StepIdentity {
        Unit { unit: UnitId, proposal: String },
        Restoration { restoration: String, units: Vec<UnitId> },
    }

    /// A single identity survives provisional classification and later reclassification.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct LearningEvent {
        pub id: String,
        pub day: f64,
        pub classification: Classification,
        pub provisional: bool,
        pub reclassified_from: Option<Classification>,
        pub charged: bool,
        pub phase: Phase,
        pub unit_phase: UnitPhase,
        pub newest_step: Option<StepIdentity>,
    }

    /// The runner may ask a policy for observations, never for hidden truth or a gate verdict.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct PolicyReport {
        pub finishes: Vec<UnitFinish>,
        /// Absent when a policy does not define a machine-wide campaign allowance.
        /// This is not an allowance with zero consumption or zero limit.
        pub allowance: Option<AllowanceLedger>,
        pub events: Vec<LearningEvent>,
        pub stopped_units: Vec<UnitId>,
        /// Coarse-stop witnesses inherited by refinement; ids use the coarse namespace.
        #[serde(default)]
        pub inherited_stops: Vec<UnitFinish>,
        pub newest_step: Option<StepIdentity>,
        /// Exact physical-core readback associated with the report, when available.
        #[serde(default, deserialize_with = "super::profile::deserialize_optional")]
        pub readback: Option<PerCore<u8>>,
        pub unit_membership: Vec<PolicyUnit>,
    }

    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct PolicyUnit {
        pub unit: UnitId,
        pub cores: Vec<CoreId>,
        pub kind: UnitPhase,
        /// The coarse-unit namespace, never the current core-unit namespace.
        pub parent: Option<UnitId>,
    }
}

impl AllowanceLedger {
    pub const fn remaining(&self) -> u64 {
        self.limit.saturating_sub(self.consumed)
    }

    pub const fn overspend(&self) -> u64 {
        self.consumed.saturating_sub(self.limit)
    }

    pub const fn exhausted(&self) -> bool {
        self.consumed >= self.limit
    }
}
