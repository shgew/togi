use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use togi_facts::{Decisive, TrialClass};
use togi_journal::{Activity, Build, Seq, SessionId, TrialId};
use togi_machine::{
    BiosContext, CcdId, Condition, CoreId, CoreSet, PER_CORE_MAX, PerCore, Regime, Signal,
    TargetOffset, WorkloadId,
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
    #[error("the profile has no offset for core {0} below its highest core")]
    Sparse(u8),
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

    /// A journal records the applied offset of the session's cores; the extract has no way to say a core
    /// below the highest one is missing, so a profile with such a hole is refused.
    pub fn from_per_core(profile: &PerCore<TargetOffset>) -> Result<Self, InvalidProfile> {
        let mut offsets = Vec::with_capacity(PER_CORE_MAX);
        let mut missing = None;
        for index in 0..PER_CORE_MAX {
            let core = i64::try_from(index)
                .ok()
                .and_then(|index| CoreId::new(index).ok());
            let Some(core) = core else { break };
            match profile.get(core) {
                Some(offset) => {
                    if let Some(hole) = missing {
                        return Err(InvalidProfile::Sparse(hole));
                    }
                    offsets.push(offset);
                }
                None => {
                    missing.get_or_insert(core.get());
                }
            }
        }
        Self::new(&offsets)
    }

    pub fn as_slice(&self) -> &[TargetOffset] {
        &self.offsets[..usize::from(self.len)]
    }
}

impl Serialize for Profile {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.as_slice())
    }
}

impl<'de> Deserialize<'de> for Profile {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let offsets = Vec::<TargetOffset>::deserialize(deserializer)?;
        Self::new(&offsets).map_err(serde::de::Error::custom)
    }
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

    pub const fn loaded(&self) -> CoreSet {
        match self {
            Self::Trial(class) => class.loaded,
            Self::Idle { loaded } => *loaded,
        }
    }

    pub const fn workload(&self) -> Option<WorkloadId> {
        match self {
            Self::Trial(class) => Some(class.workload),
            Self::Idle { .. } => None,
        }
    }

    /// The intended duration in seconds; an idle failure has none.
    pub const fn duration_s(&self) -> u32 {
        match self {
            Self::Trial(class) => class.duration_s,
            Self::Idle { .. } => 0,
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

impl From<Activity> for Phase {
    fn from(activity: Activity) -> Self {
        match activity {
            Activity::Search => Self::Search,
            Activity::Checking => Self::Checking,
            Activity::Hunt => Self::Hunt,
            Activity::Deepening => Self::Deepening,
        }
    }
}

/// The build stamp of the session that recorded a fact. `evidence` is absent from extracts the Go tool wrote.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildStamp {
    pub version: String,
    /// Empty when the build recorded no revision.
    #[serde(default)]
    pub rev: String,
    pub ruleset: u32,
    pub schema: u32,
    #[serde(default)]
    pub fixes: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<u32>,
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde passes skipped fields by reference"
)]
fn is_false(value: &bool) -> bool {
    !*value
}

impl From<&Build> for BuildStamp {
    fn from(build: &Build) -> Self {
        Self {
            version: build.version.clone(),
            rev: build.rev.clone().unwrap_or_default(),
            ruleset: build.ruleset,
            schema: build.schema,
            fixes: build.fixes,
            evidence: Some(build.evidence),
        }
    }
}

/// One decisive fact of a session, as the privacy-safe extract keeps it. Only these fields survive: no event
/// time, boot id, host, path, message or machine-check bank.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Wire", into = "Wire")]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum Kind {
    #[serde(rename = "trial")]
    Trial,
    #[serde(rename = "idle")]
    Idle,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
enum WireOutcome {
    #[serde(rename = "pass")]
    Pass,
    #[serde(rename = "failure")]
    Failure,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireClass {
    regime: Regime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workload: Option<WorkloadId>,
    cores: CoreSet,
    #[serde(default)]
    duration_s: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireContext {
    bios_version: String,
    board: String,
    cpu_model: String,
    microcode: String,
    boost_limit_mhz: u32,
}

impl From<WireContext> for BiosContext {
    fn from(context: WireContext) -> Self {
        Self {
            bios_version: context.bios_version,
            board: context.board,
            cpu_model: context.cpu_model,
            microcode: context.microcode,
            boost_limit_mhz: context.boost_limit_mhz,
        }
    }
}

impl From<BiosContext> for WireContext {
    fn from(context: BiosContext) -> Self {
        Self {
            bios_version: context.bios_version,
            board: context.board,
            cpu_model: context.cpu_model,
            microcode: context.microcode,
            boost_limit_mhz: context.boost_limit_mhz,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    session: SessionId,
    build: BuildStamp,
    ruleset: u32,
    context: Option<WireContext>,
    seq: Seq,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trial: Option<TrialId>,
    kind: Kind,
    class: WireClass,
    condition: Condition,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    phase: Option<Phase>,
    #[serde(default, skip_serializing_if = "is_false")]
    record_only: bool,
    profile: Profile,
    outcome: WireOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signal: Option<Signal>,
    duration_s: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stalled_core: Option<CoreId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    core: Option<CoreId>,
    #[serde(default, skip_serializing_if = "PerCore::is_empty")]
    voltage_requests_v: PerCore<f64>,
    #[serde(default, skip_serializing_if = "CoreSet::is_empty")]
    top_requesters: CoreSet,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    ccd_mhz: BTreeMap<CcdId, u32>,
}

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum InvalidFact {
    #[error("ruleset {ruleset} differs from the build's ruleset {build}")]
    Ruleset { ruleset: u32, build: u32 },
    #[error("a trial fact names no workload")]
    NoWorkload,
    #[error("a failure outside any trial names a workload")]
    IdleWorkload,
    #[error("regime {recorded} differs from {expected}, the regime of the fact's class")]
    Regime { recorded: Regime, expected: Regime },
    #[error("a failure records no signal")]
    NoSignal,
    #[error("a pass records a signal, an attributed core or a stalled core")]
    PassWithFailureDetail,
    #[error("a failure outside any trial is not a failure")]
    IdlePass,
}

impl TryFrom<Wire> for TrialFact {
    type Error = InvalidFact;

    fn try_from(wire: Wire) -> Result<Self, InvalidFact> {
        if wire.ruleset != wire.build.ruleset {
            return Err(InvalidFact::Ruleset {
                ruleset: wire.ruleset,
                build: wire.build.ruleset,
            });
        }
        let class = match (wire.kind, wire.class.workload) {
            (Kind::Trial, Some(workload)) => Class::Trial(TrialClass {
                workload,
                loaded: wire.class.cores,
                duration_s: wire.class.duration_s,
            }),
            (Kind::Trial, None) => return Err(InvalidFact::NoWorkload),
            (Kind::Idle, None) => Class::Idle {
                loaded: wire.class.cores,
            },
            (Kind::Idle, Some(_)) => return Err(InvalidFact::IdleWorkload),
        };
        if wire.class.regime != class.regime() {
            return Err(InvalidFact::Regime {
                recorded: wire.class.regime,
                expected: class.regime(),
            });
        }
        let outcome = match (wire.outcome, wire.signal) {
            (WireOutcome::Pass, None) if wire.core.is_none() && wire.stalled_core.is_none() => {
                if wire.kind == Kind::Idle {
                    return Err(InvalidFact::IdlePass);
                }
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
        Ok(Self {
            session: wire.session,
            build: wire.build,
            context: wire.context.map(BiosContext::from),
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

impl From<TrialFact> for Wire {
    fn from(fact: TrialFact) -> Self {
        let (outcome, signal, core, stalled_core) = match fact.outcome {
            Decisive::Pass => (WireOutcome::Pass, None, None, None),
            Decisive::Failure {
                signal,
                core,
                stalled,
            } => (WireOutcome::Failure, Some(signal), core, stalled),
        };
        Self {
            session: fact.session,
            ruleset: fact.build.ruleset,
            build: fact.build,
            context: fact.context.map(WireContext::from),
            seq: fact.seq,
            trial: fact.trial,
            kind: match fact.class {
                Class::Trial(_) => Kind::Trial,
                Class::Idle { .. } => Kind::Idle,
            },
            class: WireClass {
                regime: fact.class.regime(),
                workload: fact.class.workload(),
                cores: fact.class.loaded(),
                duration_s: fact.class.duration_s(),
            },
            condition: fact.condition,
            phase: fact.phase,
            record_only: fact.record_only,
            profile: fact.profile,
            outcome,
            signal,
            duration_s: fact.duration_s,
            stalled_core,
            core,
            voltage_requests_v: fact.voltage_requests_v,
            top_requesters: fact.top_requesters,
            ccd_mhz: fact.ccd_mhz,
        }
    }
}
