use std::collections::BTreeMap;
use std::num::NonZeroU32;

use jiff::Timestamp;
use togi_journal::{
    Activity, Attribution, Build, Failure, FailureCarried, Outcome, Payload, Seq, SessionId,
    TrialCarried, TrialId,
};
use togi_machine::{
    BootId, CcdId, Condition, CoreId, CoreSet, PerCore, Regime, Signal, TargetOffset, WorkloadId,
};

pub use togi_journal::BackendIdentity;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FactId {
    pub session: SessionId,
    pub seq: Seq,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactSource {
    pub session: SessionId,
    pub seq: Seq,
    pub time: Timestamp,
    pub build: Build,
    pub epoch: u32,
    pub boot: BootId,
    pub trial: Option<TrialId>,
    pub backend: Option<BackendIdentity>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TrialClass {
    pub workload: WorkloadId,
    pub loaded: CoreSet,
    pub duration_s: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Decisive {
    Pass,
    Failure {
        signal: Signal,
        core: Option<CoreId>,
        stalled: Option<CoreId>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Telemetry {
    pub voltage_requests_v: PerCore<f64>,
    pub top_requesters: CoreSet,
    pub ccd_mhz: BTreeMap<CcdId, u32>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Observation {
    Trial {
        class: TrialClass,
        condition: Condition,
        activity: Option<Activity>,
        rerun: bool,
        profile: PerCore<TargetOffset>,
        outcome: Decisive,
        duration_s: u32,
        telemetry: Option<Telemetry>,
    },
    IdleFailure {
        loaded: CoreSet,
        condition: Condition,
        profile: PerCore<TargetOffset>,
        signal: Signal,
        core: Option<CoreId>,
        offset: Option<TargetOffset>,
        regime: Option<Regime>,
        reason: Option<String>,
        round: Option<NonZeroU32>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fact {
    pub source: FactSource,
    pub local: Seq,
    pub observation: Observation,
}

impl Fact {
    pub fn id(&self) -> FactId {
        FactId {
            session: self.source.session,
            seq: self.source.seq,
        }
    }

    pub fn regime(&self) -> Regime {
        match &self.observation {
            Observation::Trial { class, .. } => class.workload.regime(),
            Observation::IdleFailure { .. } => Regime::R6,
        }
    }

    pub fn loaded(&self) -> CoreSet {
        match &self.observation {
            Observation::Trial { class, .. } => class.loaded,
            Observation::IdleFailure { loaded, .. } => *loaded,
        }
    }

    pub fn profile(&self) -> &PerCore<TargetOffset> {
        match &self.observation {
            Observation::Trial { profile, .. } | Observation::IdleFailure { profile, .. } => {
                profile
            }
        }
    }

    pub fn is_failure(&self) -> bool {
        match &self.observation {
            Observation::Trial { outcome, .. } => matches!(outcome, Decisive::Failure { .. }),
            Observation::IdleFailure { .. } => true,
        }
    }

    pub fn to_payload(&self) -> Payload {
        let source = togi_journal::FactSource {
            session: self.source.session,
            seq: self.source.seq,
            build: self.source.build.clone(),
            trial: self.source.trial.clone(),
            time: self.source.time,
            boot: self.source.boot.clone(),
            backend: self.source.backend.clone(),
        };
        match &self.observation {
            Observation::Trial {
                class,
                condition,
                activity,
                rerun,
                profile,
                outcome,
                duration_s,
                telemetry,
            } => {
                let (outcome, signal, core, stalled_core) = match *outcome {
                    Decisive::Pass => (Outcome::Pass, None, None, None),
                    Decisive::Failure {
                        signal,
                        core,
                        stalled,
                    } => (Outcome::Failure, Some(signal), core, stalled),
                };
                let telemetry = telemetry.clone().unwrap_or_else(|| Telemetry {
                    voltage_requests_v: PerCore::default(),
                    top_requesters: CoreSet::default(),
                    ccd_mhz: BTreeMap::new(),
                });
                Payload::TrialCarried(TrialCarried {
                    source,
                    class: togi_journal::TrialClass {
                        regime: class.workload.regime(),
                        workload: Some(class.workload),
                        cores: class.loaded,
                        duration_s: class.duration_s,
                    },
                    condition: *condition,
                    activity: *activity,
                    rerun: *rerun,
                    profile: *profile,
                    outcome,
                    signal,
                    duration_s: *duration_s,
                    core,
                    stalled_core,
                    voltage_requests_v: telemetry.voltage_requests_v,
                    top_requesters: telemetry.top_requesters,
                    ccd_mhz: telemetry.ccd_mhz,
                })
            }
            Observation::IdleFailure {
                loaded,
                condition,
                profile,
                signal,
                core,
                offset,
                regime,
                reason,
                round,
            } => Payload::FailureCarried(FailureCarried {
                source,
                class: togi_journal::TrialClass {
                    regime: Regime::R6,
                    workload: None,
                    cores: *loaded,
                    duration_s: 0,
                },
                failure: Failure {
                    signal: *signal,
                    attribution: Attribution::Unattributed,
                    core: *core,
                    offset: *offset,
                    trial: None,
                    regime: *regime,
                    condition: Some(*condition),
                    profile: Some(*profile),
                    known_failure: None,
                    reason: reason.clone(),
                    round: *round,
                },
            }),
        }
    }
}
