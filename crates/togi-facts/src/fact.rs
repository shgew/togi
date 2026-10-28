use std::collections::BTreeMap;
use std::num::NonZeroU32;

use jiff::Timestamp;
use togi_journal::{Activity, Build, Seq, SessionId, TrialId};
use togi_machine::{
    BiosContext, BootId, CcdId, Condition, CoreId, CoreSet, PerCore, Regime, Signal, TargetOffset,
    WorkloadId,
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
    /// The BIOS context in force when the trial opened or the idle failure was recorded. `None`
    /// when no `session.context` preceded it, and for copies carried from another session, whose
    /// archive never recorded one.
    pub context: Option<BiosContext>,
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
}
