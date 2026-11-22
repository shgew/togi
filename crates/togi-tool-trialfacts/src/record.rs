use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};
use togi_journal::{FixStamp, Seq, SessionId, TrialId};
use togi_machine::serde_object::Object;
use togi_machine::{
    BiosContext, CcdId, Condition, CoreId, CoreSet, PER_CORE_MAX, PerCore, Regime, Signal,
    TargetOffset, WorkloadId, object_only,
};

/// The offsets applied to every core in core-id order: at least one, at most [`PER_CORE_MAX`].
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Profile {
    offsets: [TargetOffset; PER_CORE_MAX],
    len: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum InvalidProfile {
    #[error("a profile has no offsets")]
    Empty,
    #[error("a profile has {0} offsets, at most {PER_CORE_MAX} fit")]
    TooLong(usize),
}

impl Profile {
    pub fn new(offsets: &[TargetOffset]) -> Result<Self, InvalidProfile> {
        if offsets.is_empty() {
            return Err(InvalidProfile::Empty);
        }
        if offsets.len() > PER_CORE_MAX {
            return Err(InvalidProfile::TooLong(offsets.len()));
        }
        let mut stored = [TargetOffset::ZERO; PER_CORE_MAX];
        stored[..offsets.len()].copy_from_slice(offsets);
        Ok(Self {
            offsets: stored,
            len: u8::try_from(offsets.len()).map_err(|_| InvalidProfile::TooLong(offsets.len()))?,
        })
    }

    pub fn as_slice(&self) -> &[TargetOffset] {
        &self.offsets[..usize::from(self.len)]
    }
}

impl<'de> Deserialize<'de> for Profile {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let offsets = Vec::<TargetOffset>::deserialize(deserializer)?;
        Self::new(&offsets).map_err(serde::de::Error::custom)
    }
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

/// What a fact is about: a decisive trial's class, or a failure outside any trial, which loaded the cores
/// of the idle cycle it struck.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Class {
    Trial(TrialClass),
    Idle { loaded: CoreSet },
}

impl Class {
    pub fn regime(&self) -> Regime {
        match self {
            Self::Trial(class) => class.workload.regime(),
            Self::Idle { .. } => Regime::R6,
        }
    }
}

/// The session activity that asked for a trial. `Confirmation` appears only in the committed Go-written
/// extract; the Rust journal names a confirmation trial's activity `checking` or `deepening`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum Phase {
    #[serde(rename = "search")]
    Search,
    #[serde(rename = "checking")]
    Checking,
    #[serde(rename = "confirmation")]
    Confirmation,
    #[serde(rename = "hunt")]
    Hunt,
    #[serde(rename = "deepening")]
    Deepening,
}

object_only! {
    /// The build stamp of the session that recorded a fact. `evidence` is absent from extracts the Go tool wrote.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct BuildStamp {
        pub version: String,
        /// Empty when the build recorded no revision.
        #[serde(default)]
        pub rev: String,
        pub ruleset: u32,
        pub schema: u32,
        #[serde(default = "FixStamp::none")]
        pub fixes: FixStamp,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub evidence: Option<u32>,
    }
}

/// One decisive fact of a session, as the privacy-safe extract keeps it. Only these fields survive: no event
/// time, boot id, host, path, message or machine-check bank.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(try_from = "Object<Wire>")]
pub struct TrialFact {
    pub session: SessionId,
    pub build: BuildStamp,
    pub context: Option<BiosContext>,
    pub seq: Seq,
    pub trial: Option<TrialId>,
    pub class: Class,
    pub condition: Condition,
    pub phase: Option<Phase>,
    /// Set only by facts the Go tool recorded from an observational partial.
    pub record_only: bool,
    pub profile: Profile,
    pub outcome: Decisive,
    /// What the trial measured: for a pass or a failure, the exposure to the intended duration; for a failure,
    /// the last-evidence lower bound, which can be zero.
    pub duration_s: u32,
    pub voltage_requests_v: PerCore<f64>,
    pub top_requesters: CoreSet,
    pub ccd_mhz: BTreeMap<CcdId, u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
enum Kind {
    #[serde(rename = "trial")]
    Trial,
    #[serde(rename = "idle")]
    Idle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize)]
enum WireOutcome {
    #[serde(rename = "pass")]
    Pass,
    #[serde(rename = "failure")]
    Failure,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireClass {
    regime: Regime,
    #[serde(default)]
    workload: Option<WorkloadId>,
    cores: CoreSet,
    #[serde(default)]
    duration_s: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    session: SessionId,
    build: BuildStamp,
    ruleset: u32,
    context: Option<BiosContext>,
    seq: Seq,
    #[serde(default)]
    trial: Option<TrialId>,
    kind: Kind,
    class: Object<WireClass>,
    condition: Condition,
    #[serde(default)]
    phase: Option<Phase>,
    #[serde(default)]
    record_only: bool,
    profile: Profile,
    outcome: WireOutcome,
    #[serde(default)]
    signal: Option<Signal>,
    duration_s: u32,
    #[serde(default)]
    stalled_core: Option<CoreId>,
    #[serde(default)]
    core: Option<CoreId>,
    #[serde(default)]
    voltage_requests_v: PerCore<f64>,
    #[serde(default)]
    top_requesters: CoreSet,
    #[serde(default)]
    ccd_mhz: BTreeMap<CcdId, u32>,
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
enum InvalidFact {
    #[error("ruleset {ruleset} differs from the build's ruleset {build}")]
    Ruleset { ruleset: u32, build: u32 },
    #[error("a trial fact names no workload")]
    NoWorkload,
    #[error("a failure outside any trial names a workload")]
    IdleWorkload,
    #[error("regime {recorded} differs from {expected}, the regime of the fact's class")]
    Regime { recorded: Regime, expected: Regime },
    #[error("a failure outside any trial records a trial duration of {0}s")]
    IdleDuration(u32),
    #[error("a failure records no signal")]
    NoSignal,
    #[error("a pass records a signal, an attributed core or a stalled core")]
    PassWithFailureDetail,
    #[error("a failure outside any trial is not a failure")]
    IdlePass,
}

impl TryFrom<Object<Wire>> for TrialFact {
    type Error = InvalidFact;

    fn try_from(Object(wire): Object<Wire>) -> Result<Self, InvalidFact> {
        if wire.ruleset != wire.build.ruleset {
            return Err(InvalidFact::Ruleset {
                ruleset: wire.ruleset,
                build: wire.build.ruleset,
            });
        }
        let Object(class) = wire.class;
        let recorded = class.regime;
        let class = match (wire.kind, class.workload) {
            (Kind::Trial, Some(workload)) => Class::Trial(TrialClass {
                workload,
                loaded: class.cores,
                duration_s: class.duration_s,
            }),
            (Kind::Trial, None) => return Err(InvalidFact::NoWorkload),
            (Kind::Idle, None) if class.duration_s == 0 => Class::Idle {
                loaded: class.cores,
            },
            (Kind::Idle, None) => return Err(InvalidFact::IdleDuration(class.duration_s)),
            (Kind::Idle, Some(_)) => return Err(InvalidFact::IdleWorkload),
        };
        if recorded != class.regime() {
            return Err(InvalidFact::Regime {
                recorded,
                expected: class.regime(),
            });
        }
        let outcome = match (wire.outcome, wire.signal) {
            (WireOutcome::Pass, None) if wire.core.is_none() && wire.stalled_core.is_none() => {
                Decisive::Pass
            }
            (WireOutcome::Pass, _) => return Err(InvalidFact::PassWithFailureDetail),
            (WireOutcome::Failure, Some(signal)) => Decisive::Failure {
                signal,
                core: wire.core,
                stalled: wire.stalled_core,
            },
            (WireOutcome::Failure, None) => return Err(InvalidFact::NoSignal),
        };
        if matches!((class, outcome), (Class::Idle { .. }, Decisive::Pass)) {
            return Err(InvalidFact::IdlePass);
        }
        Ok(Self {
            session: wire.session,
            build: wire.build,
            context: wire.context,
            seq: wire.seq,
            trial: wire.trial,
            class,
            condition: wire.condition,
            phase: wire.phase,
            record_only: wire.record_only,
            profile: wire.profile,
            outcome,
            duration_s: wire.duration_s,
            voltage_requests_v: wire.voltage_requests_v,
            top_requesters: wire.top_requesters,
            ccd_mhz: wire.ccd_mhz,
        })
    }
}
