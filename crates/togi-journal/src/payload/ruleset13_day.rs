//! Ruleset 13 day payloads and the session import commitment (`docs/spec/journal.md`, Ruleset 13 events and
//! Transitions). `day.record` is owned by the manual-record work and is not defined here.

use jiff::Timestamp;
use serde::Serialize;
use togi_machine::object_only;
use togi_machine::{BiosContext, BootId, CcdId, ObservedOffset, PerCore};

use super::EventKind;
use super::domain::{
    Certainty, FreezeChoice, ImportClass, IntervalState, MissingFact, ReproductionState, VetoState,
};
use super::ruleset13::{ApplicationId, CcdVector, DayEventId, DecisionId, ObservedProfile};
use super::session::{CarriedSource, Provenance};
use crate::{Build, Seq};

object_only! {
    /// How a CPU-suspect event steps the day profile back (`docs/spec/tuner.md`, Whole-profile veto and
    /// escalation): the first reverts the newest applied promotion; each further event at the stepped-back
    /// profile moves every CCD back one of its own steps.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum Escalation {
        RevertNewest { reverted: DecisionId },
        EveryCcd { strike: u32 },
    }
}

object_only! {
    /// One CCD's position in its history of applied day values, so a step back reverts that CCD's own newest step.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct CcdCursor {
        pub ccd: CcdId,
        pub depth: u32,
    }
}

object_only! {
    /// A daily event's classification. A provisional CPU-suspect event is an unexplained end at a nonzero day
    /// profile that has not been reclassified.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum Classification {
        CpuSuspect { certainty: Certainty },
        NonCpu {},
        Unexplained {},
    }
}

object_only! {
    /// Whether an event still blocks promotion. A CPU-suspect event resolves once its step back is applied and its
    /// reproduction task is closed.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum Resolution {
        Unresolved {},
        Resolved {
            /// The `day.applied` that stepped back; none for a non-CPU event.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            retreat: Option<Seq>,
        },
    }
}

object_only! {
    /// A daily event, classified from its raw witnesses. A revised classification supersedes this event's
    /// classification, never its raw source or history.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DayEvent {
        pub event: DayEventId,
        pub profile: ObservedProfile,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub boot: Option<BootId>,
        pub from: Timestamp,
        pub to: Timestamp,
        pub witnesses: Vec<Seq>,
        pub classification: Classification,
        pub resolution: Resolution,
        pub reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub predecessor: Option<DayEventId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub escalation: Option<Escalation>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub cursors: Vec<CcdCursor>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub consumed: Vec<DecisionId>,
        pub reproduction: ReproductionState,
    }
}

object_only! {
    /// A whole-profile veto of the exact original profile. Comparison and clearance belong to the tuner.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DayVeto {
        pub event: DayEventId,
        pub profile: ObservedProfile,
        pub state: VetoState,
        pub evidence: Vec<Seq>,
        pub reason: String,
    }
}

object_only! {
    /// Why a proposal holds the current day profile. `QualificationFailed` is the proposed profile a
    /// conclusive qualification failure durably blocks, with the raw failure witnesses.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum HoldReason {
        Incomplete { credited_s: u64, required_s: u64 },
        AtCap {},
        NotQualified {},
        Vetoed { veto: DayEventId },
        QuestionPending { boot: BootId },
        EventUnresolved { event: DayEventId },
        Frozen {},
        Paused {},
        Expired {},
        QualificationFailed { profile: CcdVector, witnesses: Vec<Seq> },
    }
}

object_only! {
    /// Why a retreat happens. `Completion` applies a retreat CO 0 was holding now that its profile is qualified;
    /// `DayStep.causes.events` cites the events it completes.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum RetreatCause {
        ShallowerCap {},
        Event { event: DayEventId, escalation: Escalation },
        Completion {},
    }
}

object_only! {
    /// The proposal itself. A proposal is not an application. A `Retreat`'s `wanted` is the unqualified retreat
    /// profile it falls back to CO 0 for.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum StepKind {
        Hold { blocked_by: Vec<HoldReason> },
        Promote { changed: CcdId },
        Retreat {
            changed: Vec<CcdId>,
            cause: RetreatCause,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            wanted: Option<CcdVector>,
        },
        FirmwareZero { changed: Vec<CcdId> },
    }
}

object_only! {
    /// The evidence a proposal cites.
    #[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct StepCauses {
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub caps: Vec<Seq>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub qualification: Option<Seq>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub vetoes: Vec<Seq>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub events: Vec<Seq>,
    }
}

object_only! {
    /// A cold-boot day proposal.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DayStep {
        pub decision: DecisionId,
        pub cold_boot: BootId,
        pub old: CcdVector,
        /// The complete proposed D; equal to `old` for a hold.
        pub proposed: CcdVector,
        pub step: StepKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub predecessor: Option<DecisionId>,
        pub hours: Vec<Seq>,
        pub hold_s: u64,
        pub causes: StepCauses,
        pub reason: String,
    }
}

object_only! {
    /// Why a day profile was applied. Reapplying the unchanged committed D preserves its hold; a step or a
    /// tuner-directed reset starts one.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum ApplyCause {
        Step { decision: DecisionId },
        Reapplication {},
        Reset { cause: Seq },
    }
}

object_only! {
    /// A day profile applied and verified by readback; only this commits D.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DayApplied {
        pub application: ApplicationId,
        pub cause: ApplyCause,
        pub target: CcdVector,
        pub readbacks: PerCore<ObservedOffset>,
        pub transactions: Vec<Seq>,
        pub boot: BootId,
        pub mono_ns: u64,
    }
}

object_only! {
    /// How a hold's credit changed.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum HoldChange {
        Credited {},
        Reset { cause: Seq },
        Recomputed { cause: Seq },
    }
}

object_only! {
    /// The applied-hour hold at the exact current D: a derived commitment, not an exposure source.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DayHold {
        pub profile: CcdVector,
        pub application: ApplicationId,
        pub credited: Vec<Seq>,
        pub eligible_s: u64,
        pub change: HoldChange,
    }
}

object_only! {
    /// Who chose a freeze.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum FreezeSource {
        Command { uid: u32 },
        Imported { from: Provenance },
    }
}

object_only! {
    /// A freeze or unfreeze. It changes promotion permission only.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DayFreeze {
        pub choice: FreezeChoice,
        pub source: FreezeSource,
    }
}

object_only! {
    /// A classified span of a day boot.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DayInterval {
        pub boot: BootId,
        pub start_ns: u64,
        pub end_ns: u64,
        pub profile: ObservedProfile,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub application: Option<ApplicationId>,
        pub state: IntervalState,
        pub checkpoints: Vec<Seq>,
        pub reason: String,
    }
}

object_only! {
    /// A pre-transition day record copied into the new session with its original identity and raw payload.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DayImported {
        pub original: Provenance,
        pub original_kind: EventKind,
        /// The original payload exactly as its journal line carried it (the JSON text of `data`), so an import
        /// never reshapes what it could not interpret.
        pub raw: String,
        pub time: Timestamp,
        pub boot: BootId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub build: Option<Build>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub firmware: Option<BiosContext>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub profile: Option<ObservedProfile>,
        pub class: ImportClass,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        pub missing: Vec<MissingFact>,
    }
}

object_only! {
    /// A source record the import named without importing it.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ImportedRecordRef {
        pub original: Provenance,
        pub kind: String,
    }
}

object_only! {
    /// What an import selected and what it left behind.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct ImportCounts {
        pub selected: u64,
        pub imported: u64,
        pub duplicates: u64,
        pub unknown: u64,
        pub excluded: u64,
    }
}

object_only! {
    /// The commitment that closes a fresh-transition import.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct SessionImported {
        pub sources: Vec<CarriedSource>,
        pub selected: Vec<Provenance>,
        pub counts: ImportCounts,
        pub unknown: Vec<ImportedRecordRef>,
        pub excluded: Vec<ImportedRecordRef>,
        pub reason: String,
    }
}
