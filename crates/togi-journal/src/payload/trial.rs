use std::collections::BTreeMap;
use std::num::NonZeroU32;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use togi_machine::{
    Backend, BankType, BootId, CcdId, Condition, CoreId, CoreSet, CpuId, KernelCursor,
    LoadSchedule, PerCore, PstoreRecord, Regime, ResetKind, Signal, TargetOffset, WorkloadId,
};

use super::domain::{Activity, Attribution, Outcome};
use super::session::{is_false, is_zero};
use crate::{Build, Seq, SessionId};

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TrialId(String);

impl TrialId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for TrialId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl std::fmt::Display for TrialId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct TrialIntent {
    pub trial: TrialId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_cursor: Option<KernelCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_error: Option<String>,
    pub profile: PerCore<TargetOffset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core: Option<CoreId>,
    #[serde(default, skip_serializing_if = "CoreSet::is_empty")]
    pub cores: CoreSet,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<TargetOffset>,
    pub regime: Regime,
    pub workload: WorkloadId,
    pub duration_s: u32,
    pub condition: Condition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<Activity>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub retry: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cycle: Option<NonZeroU32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<NonZeroU32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hunt: Option<NonZeroU32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<NonZeroU32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<NonZeroU32>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub rerun: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct TrialInstance {
    pub core: CoreId,
    pub cpus: Vec<CpuId>,
    pub pid: u32,
    pub scope: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct TrialStart {
    pub trial: TrialId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_start_ns: Option<u64>,
    pub scope: String,
    pub pid: u32,
    pub cpus: Vec<CpuId>,
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub instances: Vec<TrialInstance>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct TrialProgress {
    pub trial: TrialId,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<Signal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core: Option<CoreId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct TrialSignal {
    pub trial: TrialId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schedule: Option<LoadSchedule>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub stops: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub conts: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct TrialSample {
    pub trial: TrialId,
    pub warning: String,
    pub pid: u32,
    pub tid: u32,
    pub cpu: CpuId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct TrialEnd {
    pub trial: TrialId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_cursor: Option<KernelCursor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernel_error: Option<String>,
    pub outcome: Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<Signal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core: Option<CoreId>,
    pub duration_s: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tctl_max_c: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voltage_request_median_v: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voltage_request_min_v: Option<f64>,
    #[serde(default, skip_serializing_if = "PerCore::is_empty")]
    pub voltage_requests_v: PerCore<f64>,
    #[serde(default, skip_serializing_if = "CoreSet::is_empty")]
    pub top_requesters: CoreSet,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ccd_mhz: BTreeMap<CcdId, u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sample_s: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sample_tctl_c: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sample_min_mhz: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sample_max_mhz: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stalled_core: Option<CoreId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_stalled_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub interrupted: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub last_evidence: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub escaped: Vec<CpuId>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub backend_missing: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub containment_error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct Failure {
    pub signal: Signal,
    pub attribution: Attribution,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core: Option<CoreId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<TargetOffset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial: Option<TrialId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regime: Option<Regime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<Condition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<PerCore<TargetOffset>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_failure: Option<Seq>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<NonZeroU32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct Mce {
    pub cpu: CpuId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core: Option<CoreId>,
    pub bank: u16,
    pub bank_type: BankType,
    pub corrected: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monotonic_ns: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_boot: Option<BootId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial: Option<TrialId>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub between_trials: bool,
    pub lines: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct CrashDetected {
    pub previous_boot: BootId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_flight: Option<Seq>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub stray: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<Condition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_reason: Option<ResetKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reset_reason_raw: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub inconclusive: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pstore: Option<PstoreRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct FactSource {
    pub session: SessionId,
    pub seq: Seq,
    pub build: Build,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial: Option<TrialId>,
    pub time: Timestamp,
    pub boot: BootId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<BackendIdentity>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct BackendIdentity {
    pub backend: Backend,
    pub path: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct TrialClass {
    pub regime: Regime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload: Option<WorkloadId>,
    pub cores: CoreSet,
    pub duration_s: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct TrialCarried {
    pub source: FactSource,
    pub class: TrialClass,
    pub condition: Condition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<Activity>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub rerun: bool,
    pub profile: PerCore<TargetOffset>,
    pub outcome: Outcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<Signal>,
    pub duration_s: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub core: Option<CoreId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stalled_core: Option<CoreId>,
    #[serde(default, skip_serializing_if = "PerCore::is_empty")]
    pub voltage_requests_v: PerCore<f64>,
    #[serde(default, skip_serializing_if = "CoreSet::is_empty")]
    pub top_requesters: CoreSet,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub ccd_mhz: BTreeMap<CcdId, u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
pub struct FailureCarried {
    pub source: FactSource,
    pub class: TrialClass,
    pub failure: Failure,
}

togi_machine::object_only!(
    TrialIntent,
    TrialInstance,
    TrialStart,
    TrialProgress,
    TrialSignal,
    TrialSample,
    TrialEnd,
    Failure,
    Mce,
    CrashDetected,
    FactSource,
    BackendIdentity,
    TrialClass,
    TrialCarried,
    FailureCarried,
);
