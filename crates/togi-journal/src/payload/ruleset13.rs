//! Ruleset 13 evidence and night payloads (`docs/spec/journal.md`, Ruleset 13 events): exposure, CCD edge and
//! cap, night planning, qualification and reproduction, plus the identity and profile types the day payloads
//! share. Rates are validated finite on decode; an unknown or unbounded value is a tagged variant, never zero.

use std::fmt;

use serde::{Deserialize, Serialize};
use togi_machine::object_only;
use togi_machine::{
    Backend, BiosContext, BootId, CcdId, CcdProfile, ContentId, CoreId, CoreSet, CpuId, Digest,
    ObservedOffset, PerCore, Regime, Signal, TargetOffset, WorkloadId,
};

use super::domain::{
    Activity, FailureTime, Outcome, Placement, QualificationPurpose, Queue, ReproductionState,
    TaskStatus,
};
use super::session::Provenance;
use super::trial::TrialId;
use crate::{Seq, SessionId};

/// Decoders for core- and CCD-keyed maps inside internally tagged enums: serde buffers such an enum's fields, which
/// turns an integer key into text that the id types' own decoders refuse.
mod keyed {
    use std::collections::BTreeMap;

    use serde::de::Error as _;
    use serde::{Deserialize, Deserializer};
    use togi_machine::{CcdId, CcdProfile, CoreId, PerCore};

    pub(super) fn per_core<'de, D: Deserializer<'de>, T: Default + Deserialize<'de>>(
        deserializer: D,
    ) -> Result<PerCore<T>, D::Error> {
        let mut cores = PerCore::default();
        for (key, value) in BTreeMap::<String, T>::deserialize(deserializer)? {
            let number = key.parse::<i64>().map_err(D::Error::custom)?;
            cores.set(CoreId::new(number).map_err(D::Error::custom)?, value);
        }
        Ok(cores)
    }

    pub(super) fn ccd_profile<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<CcdProfile, D::Error> {
        let mut ccds = BTreeMap::new();
        for (key, value) in
            BTreeMap::<String, togi_machine::TargetOffset>::deserialize(deserializer)?
        {
            let number = key.parse::<i64>().map_err(D::Error::custom)?;
            ccds.insert(CcdId::new(number).map_err(D::Error::custom)?, value);
        }
        CcdProfile::try_from(ccds).map_err(D::Error::custom)
    }
}

/// A number that was not finite, or not nonnegative where it must be.
#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
pub enum InvalidNumber {
    #[error("{0} is not finite")]
    NotFinite(f64),
    #[error("{0} is negative")]
    Negative(f64),
}

/// A finite number of either sign, such as a slope's lower endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Finite(f64);

impl Finite {
    pub fn new(value: f64) -> Result<Self, InvalidNumber> {
        if value.is_finite() {
            Ok(Self(value))
        } else {
            Err(InvalidNumber::NotFinite(value))
        }
    }

    pub const fn get(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for Finite {
    type Error = InvalidNumber;
    fn try_from(value: f64) -> Result<Self, InvalidNumber> {
        Self::new(value)
    }
}

impl From<Finite> for f64 {
    fn from(value: Finite) -> Self {
        value.0
    }
}

/// A finite nonnegative rate per applied hour.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Rate(f64);

impl Rate {
    pub fn new(value: f64) -> Result<Self, InvalidNumber> {
        if !value.is_finite() {
            Err(InvalidNumber::NotFinite(value))
        } else if value < 0.0 {
            Err(InvalidNumber::Negative(value))
        } else {
            Ok(Self(value))
        }
    }

    pub const fn get(self) -> f64 {
        self.0
    }
}

impl TryFrom<f64> for Rate {
    type Error = InvalidNumber;
    fn try_from(value: f64) -> Result<Self, InvalidNumber> {
        Self::new(value)
    }
}

impl From<Rate> for f64 {
    fn from(value: Rate) -> Self {
        value.0
    }
}

/// A night's identity: the sequence of the `night.plan` that opened it. A night has no deadline; it stays open
/// across tuning restarts and crash reboots into tuning until the next `day.boot_start`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NightId(pub Seq);

impl fmt::Display for NightId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// A day decision's durable ordinal.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DecisionId(pub u64);

/// A day application's durable ordinal; retries of one application share it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ApplicationId(pub u64);

object_only! {
    /// A night task: its queue and its ordinal within the night.
    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct TaskId {
        pub queue: Queue,
        pub ordinal: u32,
    }
}

object_only! {
    /// A daily event's stable identity: the boot whose end or fault it is, the journal record (manual report or
    /// import) it came from, by original session and sequence, or the night whose monthly re-qualification of D
    /// failed.
    #[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum DayEventId {
        Boot { boot: BootId },
        Record { session: SessionId, seq: Seq },
        Requalification { night: NightId },
    }
}

object_only! {
    /// The complete CCD vector and its expansion to every physical core.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct CcdVector {
        #[serde(deserialize_with = "keyed::ccd_profile")]
        pub ccd: CcdProfile,
        #[serde(deserialize_with = "keyed::per_core")]
        pub cores: PerCore<TargetOffset>,
    }
}

object_only! {
    /// A profile as the day observer recorded or a person reported it: a CCD vector with its expansion, or, for a
    /// nonuniform profile, only its per-core shape.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum ObservedProfile {
        Ccd {
            #[serde(deserialize_with = "keyed::ccd_profile")]
            ccd: CcdProfile,
            #[serde(deserialize_with = "keyed::per_core")]
            cores: PerCore<ObservedOffset>,
        },
        PerCore {
            #[serde(deserialize_with = "keyed::per_core")]
            cores: PerCore<ObservedOffset>,
        },
    }
}

object_only! {
    /// A workload and the content identity it ran with.
    #[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct WorkloadContent {
        pub workload: WorkloadId,
        #[serde(deserialize_with = "togi_machine::serde_object::deserialize")]
        pub content: ContentId,
    }
}

object_only! {
    /// What a trial tests: an isolated CCD at an offset, or the exact whole profile.
    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum Tested {
        Isolated { ccd: CcdId, offset: TargetOffset },
        Exact {},
    }
}

object_only! {
    /// The light or serial position an R8 or R9 class names.
    #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum Position {
        Light { core: CoreId },
        Serial { cpu: CpuId },
    }
}

object_only! {
    /// A trial class as the exposure ledger keys it.
    #[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ExposureClass {
        pub regime: Regime,
        pub workloads: Vec<WorkloadContent>,
        pub cores: CoreSet,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub position: Option<Position>,
        pub duration_s: u32,
        pub placement: Placement,
    }
}

object_only! {
    /// Where an exposure came from: this session's trial, or an import with its first provenance.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum ExposureOrigin {
        Live {},
        Imported {},
    }
}

object_only! {
    /// The original identity of an observation: copies add no second observation.
    #[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct OriginalSource {
        pub session: SessionId,
        pub seq: Seq,
        pub boot: BootId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub trial: Option<TrialId>,
    }
}

object_only! {
    /// Why a trial's passing credit is withheld.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum CreditExclusion {
        HostCore { core: CoreId },
        PlacementWithheld {},
    }
}

object_only! {
    /// One failure of a trial, retained once.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ExposureFailure {
        pub signal: Signal,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub core: Option<CoreId>,
        pub time: FailureTime,
    }
}

object_only! {
    /// One raw exposure observation of an isolated or exact trial.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ExposureObserved {
        pub source: OriginalSource,
        pub origin: ExposureOrigin,
        pub firmware: BiosContext,
        pub epoch: u32,
        pub class: ExposureClass,
        pub profile: CcdVector,
        /// What the trial tested: an isolated CCD at an offset, or the exact whole profile.
        pub tested: Tested,
        /// The purpose the trial ran for; only mapping exposure enters a cap's rate projection.
        pub activity: Activity,
        pub outcome: Outcome,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub failures: Vec<ExposureFailure>,
        pub eligible_s: u64,
        pub uninterrupted_s: u64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub exclusions: Vec<CreditExclusion>,
    }
}

object_only! {
    /// A CCD's edge U.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum Edge {
        Unknown {},
        Observed { offset: TargetOffset, witness: Seq },
        Censored { limit: TargetOffset, witnesses: Vec<Seq> },
    }
}

object_only! {
    /// A CCD's edge changed.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct CcdEdge {
        pub ccd: CcdId,
        pub edge: Edge,
        pub old: Edge,
        pub reason: String,
    }
}

object_only! {
    /// An upper rate bound; infinite exposure-free cells are unbounded, never a number.
    #[derive(Clone, Copy, Debug, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum RateBound {
        Finite { per_hour: Rate },
        Unbounded {},
    }
}

object_only! {
    /// A class's deepest adequately exposed cell.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct RateCell {
        pub class: ExposureClass,
        pub offset: TargetOffset,
        pub eligible_s: u64,
        pub failures: u32,
        pub bound: RateBound,
    }
}

object_only! {
    /// The measured slope's lower 95% profile-likelihood endpoint.
    #[derive(Clone, Copy, Debug, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum LowerEndpoint {
        Bounded { beta: Finite },
        Unbounded {},
    }
}

object_only! {
    /// The slope behind a cap.
    #[derive(Clone, Copy, Debug, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum Slope {
        Fallback { beta: Finite },
        Measured {
            lower: LowerEndpoint,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            historical_min: Option<Finite>,
        },
    }
}

object_only! {
    /// The deepest offset at which a class's projected bound stays within the budget.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Candidate {
        pub class: ExposureClass,
        pub offset: TargetOffset,
    }
}

object_only! {
    /// What fixed the cap.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum Binding {
        Rate { class: ExposureClass },
        EdgeMargin {},
        Baseline {},
    }
}

object_only! {
    /// A CCD's cap S changed or was recomputed.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct CcdCap {
        pub ccd: CcdId,
        pub old: TargetOffset,
        pub new: TargetOffset,
        pub budget: Rate,
        pub edge: Edge,
        pub adequate: Vec<ExposureClass>,
        pub uncovered: Vec<ExposureClass>,
        pub cells: Vec<RateCell>,
        pub slope: Slope,
        pub candidates: Vec<Candidate>,
        pub binding: Binding,
    }
}

object_only! {
    /// One queue's share of a night's elapsed observed time and what it consumed. The filler has no share: it
    /// runs only while no queue has due work, so its `share_pct` is 0.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Budget {
        pub queue: Queue,
        pub share_pct: u8,
        pub consumed_s: u64,
    }
}

object_only! {
    /// A planned task and its status.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct PlannedTask {
        pub task: TaskId,
        pub status: TaskStatus,
    }
}

object_only! {
    /// The night's plan. The plan without `night` opens a night, whose identity is that plan's sequence; every
    /// later plan of the night names it. A restart continues the open night and its consumption.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct NightPlan {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub night: Option<NightId>,
        pub budgets: Vec<Budget>,
        pub tasks: Vec<PlannedTask>,
        /// The backends the night runs no cell on, each with the events that excluded it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub unavailable: Vec<UnavailableBackend>,
    }
}

object_only! {
    /// A backend excluded from the night and the evidence that excluded it, its inconclusive streak.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct UnavailableBackend {
        pub backend: Backend,
        pub causes: Vec<Seq>,
    }
}

object_only! {
    /// How a certificate cell was witnessed.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum CellEvidence {
        Fresh { exposure: Seq },
        Reused { exposure: Seq },
    }
}

object_only! {
    /// A matrix cell and what satisfied it.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct CellWitness {
        pub cell: ExposureClass,
        pub evidence: CellEvidence,
    }
}

object_only! {
    /// Where a qualification stands. Only `complete` confers qualification.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum QualificationState {
        Start { outstanding: Vec<ExposureClass> },
        Progress { witnesses: Vec<CellWitness>, outstanding: Vec<ExposureClass> },
        Complete { witnesses: Vec<CellWitness> },
        Failed { failure: Seq, witnesses: Vec<CellWitness>, outstanding: Vec<ExposureClass> },
    }
}

object_only! {
    /// A night's exact-profile qualification against its frozen matrix.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct NightQualification {
        pub profile: CcdVector,
        pub firmware: BiosContext,
        pub epoch: u32,
        pub contents: Vec<WorkloadContent>,
        pub matrix: Digest,
        pub purpose: QualificationPurpose,
        pub state: QualificationState,
    }
}

object_only! {
    /// What a reproduction retests: the exact failed profile or one CCD alone.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum ReproductionTarget {
        Exact { profile: CcdVector },
        Isolated { ccd: CcdId, offset: TargetOffset },
    }
}

object_only! {
    /// A daily event's one-night reproduction task. It closes only the task, never the veto.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct NightReproduction {
        pub source: DayEventId,
        /// What the task retests; absent exactly when `state` is `untestable`, because the failed profile
        /// was nonuniform and no night writes one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub target: Option<ReproductionTarget>,
        pub night: NightId,
        pub consumed_s: u64,
        pub remaining: Vec<ExposureClass>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub failures: Vec<Seq>,
        pub state: ReproductionState,
    }
}

impl From<Provenance> for DayEventId {
    fn from(from: Provenance) -> Self {
        Self::Record {
            session: from.session,
            seq: from.seq,
        }
    }
}
