use std::collections::BTreeMap;
use std::num::NonZeroU32;

use super::domain::Placement;
use super::ruleset13::{NightId, TaskId, Tested, WorkloadContent};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use togi_machine::object_only;
use togi_machine::{
    Backend, BankType, BootId, CcdId, Condition, CoreId, CoreSet, CpuId, KernelCursor,
    LoadSchedule, PerCore, PstoreRecord, Regime, ResetKind, Signal, TargetOffset, TrialRole,
    WorkloadId,
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

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
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
        /// The night this trial belongs to, by its `until`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub night: Option<NightId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub task: Option<TaskId>,
        /// Every workload the roles run with its frozen content identity.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub content: Vec<WorkloadContent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub placement: Option<Placement>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub tested: Option<Tested>,
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
}

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct TrialInstance {
        pub core: CoreId,
        pub cpus: Vec<CpuId>,
        pub pid: u32,
        pub scope: String,
    }
}

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ControllerEvidence {
        pub pid: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub threads: Vec<u32>,
        /// Worker TID to owning leader PID, captured before the trial wait.
        #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
        pub workers: std::collections::BTreeMap<u32, u32>,
        pub tested_cpus: Vec<CpuId>,
    }
}

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ProcessFaultEvidence {
        pub pid: u32,
        /// The saved controller or worker leader owning this PID/TID.
        pub process_pid: u32,
        pub name: String,
        pub kind: togi_machine::ProcessFaultKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub scope: Option<String>,
        pub monotonic_ns: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub cpu: Option<CpuId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub core: Option<CoreId>,
        /// The worker instance that died, distinct from the CPU where the fault executed.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub worker_core: Option<CoreId>,
        pub lines: Vec<String>,
    }
}

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
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
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub roles: Vec<TrialRole>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub controller: Option<ControllerEvidence>,
    }
}

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct TrialProgress {
        pub trial: TrialId,
        pub detail: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub signal: Option<Signal>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub core: Option<CoreId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub pid: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub fault: Option<ProcessFaultEvidence>,
    }
}

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct TrialSignal {
        pub trial: TrialId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub schedule: Option<LoadSchedule>,
        #[serde(default, skip_serializing_if = "is_zero")]
        pub stops: u32,
        #[serde(default, skip_serializing_if = "is_zero")]
        pub conts: u32,
    }
}

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct TrialSample {
        pub trial: TrialId,
        pub warning: String,
        pub pid: u32,
        pub tid: u32,
        pub cpu: CpuId,
    }
}

object_only! {
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
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
        /// Cores withheld from exposure credit, even when the workload passed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub excluded: Vec<CoreId>,
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
}

impl TrialEnd {
    /// A pass with an excluded controller host does not prove the intended whole load.
    /// Per-core ledgers may still credit tested, nonexcluded cores; class evidence and
    /// coverage must treat that pass as inconclusive instead of crediting the host.
    pub fn evidence_outcome(&self) -> Outcome {
        match self.outcome {
            Outcome::Pass if !self.excluded.is_empty() => Outcome::Inconclusive,
            Outcome::Pass => Outcome::Pass,
            Outcome::Failure => Outcome::Failure,
            Outcome::Inconclusive => Outcome::Inconclusive,
        }
    }
}

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
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
}

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
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
}

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
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
}

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
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
}

object_only! {
    #[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct BackendIdentity {
        pub backend: Backend,
        pub path: String,
    }
}

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct TrialClass {
        pub regime: Regime,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub workload: Option<WorkloadId>,
        pub cores: CoreSet,
        pub duration_s: u32,
    }
}

object_only! {
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
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
}

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct FailureCarried {
        pub source: FactSource,
        pub class: TrialClass,
        pub failure: Failure,
    }

}
