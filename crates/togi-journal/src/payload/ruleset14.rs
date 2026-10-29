//! Additive Ruleset 14 commitments. Released CCD payloads retain their original meanings.

use serde::{Deserialize, Serialize};
use togi_machine::{
    BiosContext, BootId, CcdId, CoreId, CoreSet, PerCore, TargetOffset, object_only,
};

use super::{
    ApplicationId, CarriedSource, Classification, DayEventId, Edge, ImportCounts, Provenance,
};
use crate::Seq;

/// A topology identity, ordered CCDs first, then physical cores. The wire is `{ccd: n}` or `{core: n}`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum UnitId {
    Ccd(CcdId),
    Core(CoreId),
}

impl UnitId {
    pub const fn ccd(self) -> Option<CcdId> {
        match self {
            Self::Ccd(ccd) => Some(ccd),
            Self::Core(_) => None,
        }
    }
}

impl std::fmt::Display for UnitId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ccd(ccd) => ccd.fmt(f),
            Self::Core(core) => write!(f, "core {core}"),
        }
    }
}

impl<'de> Deserialize<'de> for UnitId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        // A one-field object keeps unknown-field diagnostics usable by the lenient reader;
        // serde's externally tagged enum decoder reports only a syntax error on extra members.
        object_only! {
            #[derive(Serialize)]
            #[serde(deny_unknown_fields)]
            struct Fields {
                #[serde(default, deserialize_with = "present")]
                ccd: Option<CcdId>,
                #[serde(default, deserialize_with = "present")]
                core: Option<CoreId>,
            }
        }
        let fields = Fields::deserialize(deserializer)?;
        match (fields.ccd, fields.core) {
            (Some(ccd), None) => Ok(Self::Ccd(ccd)),
            (None, Some(core)) => Ok(Self::Core(core)),
            _ => Err(D::Error::custom("expected exactly one of ccd or core")),
        }
    }
}

fn present<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitPhase {
    Coarse,
    Core,
}

object_only! {
    /// Physical membership and the coarse ancestor of a refinement unit.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct UnitMembership {
        pub unit: UnitId,
        pub cores: CoreSet,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub parent: Option<UnitId>,
    }

    /// The complete unit table in ascending unit order.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct UnitMapping {
        pub phase: UnitPhase,
        pub units: Vec<UnitMembership>,
    }

    /// The full physical-core vector and its mapping, never an averaged CCD approximation.
    /// Coarse offsets are recorded on every member core uniformly. As with `CcdVector`, decoding
    /// retains recorded discrepancies; topology and application validation belong to the consumer.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct UnitVector {
        #[serde(deserialize_with = "per_core")]
        pub cores: PerCore<TargetOffset>,
        pub mapping: UnitMapping,
    }

    /// A unit's contextual edge and the derived hard ceiling, not statistical S.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct UnitEdge {
        pub unit: UnitId,
        pub firmware: BiosContext,
        pub epoch: u32,
        pub old: Edge,
        pub new: Edge,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub ceiling: Option<TargetOffset>,
        pub margin: u32,
    }

    /// Exact-profile applied-hour credit and its original interval witnesses.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ClimbHold {
        pub application: ApplicationId,
        pub credited: Vec<Seq>,
        pub eligible_s: u64,
    }

    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum ClimbBlocker {
        Hold { credited_s: u64, required_s: u64 },
        Certificate {},
        CertificateFailed { profile: UnitVector, witnesses: Vec<Seq> },
        QuestionPending { boot: BootId },
        EventUnresolved { event: DayEventId },
        Veto { event: DayEventId },
        FirmwareInvalid {},
        Frozen {},
        Paused {},
        AllowanceExhausted {},
        UnknownEdge { unit: UnitId },
        Finished { unit: UnitId },
    }

    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum ClimbRetreatCause {
        AdverseCeiling { edges: Vec<Seq> },
        Event { event: DayEventId },
        Veto { event: DayEventId },
        Completion {},
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestorationKind {
    Firmware,
    Reclassified,
}

object_only! {
    /// A proposal, never an application or a consumed history step.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum ClimbAction {
        Hold { blocked_by: Vec<ClimbBlocker> },
        Promote { unit: UnitId, from: TargetOffset, to: TargetOffset, step: u32 },
        Retreat {
            units: Vec<UnitId>,
            cause: ClimbRetreatCause,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            wanted: Option<UnitVector>,
        },
        Restore { restoration: RestorationKind, units: Vec<UnitId> },
        FirmwareZero {},
    }

    /// A cold-boot decision with exact vectors and local journal evidence sequences.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ClimbDecision {
        pub decision: ClimbAction,
        pub cold_boot: BootId,
        pub before: UnitVector,
        pub proposed: UnitVector,
        pub hold: ClimbHold,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub certificate: Option<Seq>,
        pub ceilings: Vec<Seq>,
        pub events: Vec<Seq>,
    }

    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum UnitStopReason {
        Ceiling {},
        FirstBackstep { event: DayEventId, application: ApplicationId },
        AllowanceExhausted {},
        RestorationBackstep { event: DayEventId },
    }

    /// Sticky completion: the envelope records time; witnesses retain the edge/application history.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct UnitStop {
        pub unit: UnitId,
        pub reason: UnitStopReason,
        pub profile: UnitVector,
        pub last_clean: TargetOffset,
        pub witnesses: Vec<Seq>,
    }
}

/// Campaign identity survives session transitions; it names the original campaign start.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CampaignId(pub Provenance);

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum CampaignOrigin {
        Fresh {},
        Successor { sources: Vec<CarriedSource> },
    }

    /// Missing history is not zero consumption and cannot grant a fresh allowance.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum CampaignHistory {
        Complete {},
        Unreconstructible { reason: String },
    }

    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct CampaignStart {
        pub campaign: CampaignId,
        pub allowance: u32,
        pub origin: CampaignOrigin,
        pub history: CampaignHistory,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CampaignEventPhase {
    Learning,
    PostFinish,
}

object_only! {
    /// One canonical event charge, retaining aliases without multiplying events or replenishing allowance.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct CampaignCharge {
        pub campaign: CampaignId,
        pub event: DayEventId,
        pub aliases: Vec<DayEventId>,
        pub profile: UnitVector,
        pub phase: CampaignEventPhase,
        pub unit_phase: UnitPhase,
        pub provisional: bool,
        pub consumed: u32,
        pub remaining: u32,
        pub exhausted: bool,
    }

    /// Classification history does not refund the historical consumed count.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct CampaignReclassified {
        pub campaign: CampaignId,
        pub event: DayEventId,
        pub from: Classification,
        pub to: Classification,
    }

    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum FirmwareRestorationState {
        Saved {},
        Qualified { certificate: Seq },
        Held { hold: ClimbHold },
        Restored { application: ApplicationId },
        SteppedBack { event: DayEventId },
    }

    /// The exact pre-change vector remains history, not current-context permission.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct FirmwareRestoration {
        pub saved: UnitVector,
        pub state: FirmwareRestorationState,
    }

    /// A successor's raw import manifest. Original identities refer to immutable source sessions;
    /// they are not local journal causes or carried qualification permission.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct SuccessorImported {
        pub sources: Vec<CarriedSource>,
        pub counts: ImportCounts,
        pub vetoes: Vec<Provenance>,
        pub night_observations: Vec<Provenance>,
        pub campaign: CampaignHistory,
    }
}

// Internally tagged parents buffer JSON keys as text. Decode those keys explicitly, just as
// the released CCD vector does, so nested full vectors accept the same core identifiers.
fn per_core<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<PerCore<TargetOffset>, D::Error> {
    use serde::de::Error as _;
    let mut cores = PerCore::default();
    for (key, value) in
        std::collections::BTreeMap::<String, TargetOffset>::deserialize(deserializer)?
    {
        let number = key.parse::<i64>().map_err(D::Error::custom)?;
        cores.set(CoreId::new(number).map_err(D::Error::custom)?, value);
    }
    Ok(cores)
}
