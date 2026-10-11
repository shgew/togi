use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};
use togi_machine::{CcdId, CoreId, CoreSet, PerCore, Regime, TargetOffset, WorkloadId};

use super::domain::{
    Activity, CorePhase, CycleEvent, Decision, HuntOutcome, HuntStage, Inference, RetryTarget,
    RoundEvent, WarningKind,
};
use super::session::is_false;
use super::trial::TrialId;
use crate::Seq;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TunerDecision {
    pub core: CoreId,
    pub activity: Activity,
    pub decision: Decision,
    pub from_offset: TargetOffset,
    pub to_offset: TargetOffset,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass: Option<TargetOffset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_point: Option<TargetOffset>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workloads: Vec<WorkloadId>,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreTransition {
    pub core: CoreId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<CorePhase>,
    pub to: CorePhase,
    pub offset: TargetOffset,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass: Option<TargetOffset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_point: Option<TargetOffset>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub check_solo_limit: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workloads: Vec<WorkloadId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleared_combination: Vec<NonZeroU32>,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckingCycle {
    pub cycle: NonZeroU32,
    pub event: CycleEvent,
    #[serde(default, skip_serializing_if = "is_false")]
    pub passed: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub full: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Regime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckingStep {
    pub cycle: NonZeroU32,
    pub step: NonZeroU32,
    pub profile: PerCore<TargetOffset>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rederived {
    Unchanged,
    Changed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckingChain {
    pub cycle: NonZeroU32,
    pub step: NonZeroU32,
    pub ccd: CcdId,
    pub workload: WorkloadId,
    pub groups: Vec<CoreSet>,
    pub cores: CoreSet,
    pub source_seqs: Vec<Seq>,
    pub profile: PerCore<TargetOffset>,
    pub part: NonZeroU32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rederived: Option<Rederived>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostRanking {
    pub ranking: Vec<CoreId>,
    #[serde(default, skip_serializing_if = "PerCore::is_empty")]
    pub values: PerCore<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub core: CoreId,
    pub offset: TargetOffset,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HuntStart {
    pub hunt: NonZeroU32,
    pub failure: Seq,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial: Option<TrialId>,
    pub regime: Regime,
    pub workload: WorkloadId,
    pub cores: CoreSet,
    pub duration_s: u32,
    pub failing: PerCore<TargetOffset>,
    pub parked: PerCore<TargetOffset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parked_seq: Option<Seq>,
    pub candidates: CoreSet,
    pub trials: u32,
    pub trial_s: u32,
    pub miss: f64,
    pub rate: f64,
    pub ranking: Vec<CoreId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(clippy::struct_excessive_bools, reason = "each flag is a distinct recorded fact")]
pub struct HuntGroup {
    pub hunt: NonZeroU32,
    pub group: NonZeroU32,
    pub cores: CoreSet,
    pub profile: PerCore<TargetOffset>,
    pub set: CoreSet,
    pub granularity: u32,
    pub stage: HuntStage,
    pub index: u32,
    pub duration_s: u32,
    #[serde(default, skip_serializing_if = "is_false")]
    pub escalated: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub full_checked: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub any_failed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<Member>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub held: Vec<Member>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inferred: Option<Inference>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub skipped: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HuntEnd {
    pub hunt: NonZeroU32,
    pub result: HuntOutcome,
    #[serde(default, skip_serializing_if = "CoreSet::is_empty")]
    pub cores: CoreSet,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<Member>,
    pub groups: u32,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HuntSkipped {
    pub failure: Seq,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Combination {
    pub combination: NonZeroU32,
    pub members: Vec<Member>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub fallback: bool,
    pub hunt: NonZeroU32,
    pub reason: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeepeningRound {
    pub round: NonZeroU32,
    pub event: RoundEvent,
    #[serde(default, skip_serializing_if = "PerCore::is_empty")]
    pub base: PerCore<TargetOffset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_seq: Option<Seq>,
    #[serde(default, skip_serializing_if = "PerCore::is_empty")]
    pub target: PerCore<TargetOffset>,
    #[serde(default, skip_serializing_if = "PerCore::is_empty")]
    pub profile: PerCore<TargetOffset>,
    #[serde(default, skip_serializing_if = "CoreSet::is_empty")]
    pub cores: CoreSet,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ranking: Vec<CoreId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trials: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_s: Option<u32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TunerWarning {
    pub warning: WarningKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial: Option<TrialId>,
    pub passes: Vec<Seq>,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendRetry {
    pub backend: RetryTarget,
    pub attempt: u8,
    pub wait_s: u32,
    pub reason: String,
}
