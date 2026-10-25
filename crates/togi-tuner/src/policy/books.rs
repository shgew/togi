//! The folded records the policy keeps: everything is raw journal fact or one of the policy's own
//! commitments, with the seq that carries it.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use jiff::Timestamp;
use togi_journal::{
    CcdCap, CcdEdge, DayApplied, DayEvent, DayEventId, DayFault, DayHold, DayInterval, DayRecord,
    DayStep, DayUncleanEnd, DayVeto, ExposureObserved, Mce, NightId, NightPlan, NightQualification,
    NightReproduction, Seq, SessionId, TaskId, TrialEnd, TrialId, TrialIntent, TrialStart,
};
use togi_machine::{
    BiosContext, BootId, CcdId, ContentId, CoreInfo, ObservedOffset, PerCore, WorkloadId,
};

use crate::Topology;
use crate::day::{DayHistory, Freeze};

/// A journal payload with the position and time that committed it.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Committed<T> {
    pub(super) seq: Seq,
    pub(super) time: Timestamp,
    /// The events the commitment cites; a qualification freezes its reuse evidence at the latest.
    pub(super) causes: Vec<Seq>,
    pub(super) payload: T,
}

/// What `session.start`, `session.context` and the day observer say about the machine.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Machine {
    pub(super) session: Option<SessionId>,
    pub(super) cores: Vec<CoreInfo>,
    pub(super) topology: Option<Topology>,
    /// The latest firmware context any record reported.
    pub(super) firmware: Option<BiosContext>,
    /// Every firmware context seen, in order of first appearance.
    pub(super) contexts: Vec<BiosContext>,
    /// The context the applied profile was last set under; a different current context owes a firmware
    /// zero.
    pub(super) anchor: Option<BiosContext>,
    /// The event that switched the latest firmware context in.
    pub(super) changed_at: Option<Seq>,
    /// How many times the firmware context or a workload's content changed: the durable ordinal of the
    /// controls the current evidence context owes.
    pub(super) evidence_changes: u32,
    /// The frozen content identity of each workload, from the latest intent or qualification naming it.
    #[serde(with = "super::wire::pairs")]
    pub(super) contents: BTreeMap<WorkloadId, ContentId>,
}

impl Machine {
    /// Freezes a workload's content; a different content than the one known starts a new evidence context.
    pub(super) fn set_content(&mut self, workload: WorkloadId, content: &ContentId) {
        let changed = self
            .contents
            .insert(workload, content.clone())
            .is_some_and(|known| known != *content);
        if changed {
            self.evidence_changes = self.evidence_changes.saturating_add(1);
        }
    }

    /// Records the latest context and remembers each distinct one.
    pub(super) fn set_firmware(&mut self, seq: Seq, context: BiosContext) {
        if !self.contexts.contains(&context) {
            self.contexts.push(context.clone());
        }
        if self
            .firmware
            .as_ref()
            .is_some_and(|known| *known != context)
        {
            self.changed_at = Some(seq);
            self.evidence_changes = self.evidence_changes.saturating_add(1);
        }
        self.firmware = Some(context);
    }
}

/// One Ruleset 13 trial: its intent and, as the journal delivers them, its start, end and failure.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Trial {
    pub(super) intent_seq: Seq,
    pub(super) intent_time: Timestamp,
    pub(super) boot: BootId,
    /// The firmware context in force when the intent was journaled.
    pub(super) firmware: Option<BiosContext>,
    pub(super) intent: TrialIntent,
    pub(super) start: Option<(Seq, TrialStart)>,
    pub(super) end: Option<Ended>,
    /// The `failure` event that attributes the end.
    pub(super) failure: Option<Seq>,
    /// The `tuner.warning` that flagged the end.
    pub(super) warned: bool,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Ended {
    pub(super) seq: Seq,
    pub(super) time: Timestamp,
    pub(super) end: TrialEnd,
}

impl Trial {
    pub(super) fn task(&self) -> Option<TaskId> {
        self.intent.task
    }

    pub(super) fn night(&self) -> Option<NightId> {
        self.intent.night
    }
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Trials {
    pub(super) all: Vec<Trial>,
    #[serde(with = "super::wire::pairs")]
    pub(super) index: BTreeMap<TrialId, usize>,
    /// The trial indexes of each night.
    #[serde(with = "super::wire::pairs")]
    pub(super) by_night: BTreeMap<NightId, Vec<usize>>,
    /// The trial indexes of each task.
    #[serde(with = "togi_machine::serde_pairs")]
    pub(super) by_task: HashMap<TaskId, Vec<usize>>,
    /// The filler trial indexes across every night, in intent order: a filler trial's place here is the
    /// durable cursor that rotated its cell.
    pub(super) fillers: Vec<usize>,
    /// The ended trials in the order they ended.
    pub(super) ended_order: Vec<usize>,
    /// The R7 trials that ended carrying measured requests, in the order they ended.
    pub(super) loads: Vec<usize>,
    /// Ended failing trials still without their `failure`.
    pub(super) unreported_failures: BTreeSet<usize>,
    /// Ended trials still without their `exposure.observed`.
    pub(super) unobserved: BTreeSet<usize>,
    /// Ended failing isolated trials without a `tuner.warning`.
    pub(super) unwarned: BTreeSet<usize>,
}

impl Trials {
    /// The durable filler cursor of the next filler trial: how many filler trials came before it.
    pub(super) fn filler_cursor(&self) -> u32 {
        u32::try_from(self.fillers.len()).unwrap_or(u32::MAX)
    }

    pub(super) fn get(&self, id: &TrialId) -> Option<&Trial> {
        self.index.get(id).map(|&at| &self.all[at])
    }

    pub(super) fn get_mut(&mut self, id: &TrialId) -> Option<&mut Trial> {
        let at = *self.index.get(id)?;
        self.all.get_mut(at)
    }
}

/// A folded `exposure.observed` with the trial context its source names.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Exposure {
    pub(super) seq: Seq,
    pub(super) observed: ExposureObserved,
    pub(super) task: Option<TaskId>,
    pub(super) night: Option<NightId>,
    /// The durable filler cursor of a filler trial's exposure: the matrix ordinal and rotation that chose
    /// its cell.
    pub(super) filler: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Ledger {
    pub(super) records: Vec<Exposure>,
    /// The aggregates of the records, one group per evidence context.
    pub(super) groups: Vec<super::evidence::Group>,
    /// The ledger indexes of each task's records.
    #[serde(with = "togi_machine::serde_pairs")]
    pub(super) by_task: HashMap<TaskId, Vec<usize>>,
    /// The original identities already folded: each counts once.
    #[serde(serialize_with = "togi_machine::serde_pairs::serialize_set")]
    pub(super) seen: HashSet<(SessionId, Seq)>,
}

/// The night side's own commitments.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Nights {
    /// Each night's first plan commitment and its latest.
    #[serde(with = "super::wire::pairs")]
    pub(super) plans: BTreeMap<NightId, PlanBook>,
    /// The open night: opened by its first plan, closed by a later boot's `day.boot_start`, the operator's stop
    /// (`shutdown` by signal) or a session reset.
    pub(super) open: Option<NightId>,
    #[serde(with = "super::wire::pairs")]
    pub(super) edges: BTreeMap<CcdId, Committed<CcdEdge>>,
    #[serde(with = "super::wire::pairs")]
    pub(super) caps: BTreeMap<CcdId, Committed<CcdCap>>,
    pub(super) qualifications: Vec<Committed<NightQualification>>,
    pub(super) reproductions: Vec<Committed<NightReproduction>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct PlanBook {
    pub(super) first: Committed<NightPlan>,
    pub(super) latest: Committed<NightPlan>,
    /// The boot of the plan that opened the night: a crash reboot back into tuning continues it, and only a
    /// later boot's `day.boot_start` closes it.
    pub(super) opened_in: BootId,
    /// The event that closed the night: a later boot's `day.boot_start`, a `shutdown` by signal or a session
    /// reset. `None` while it is open.
    pub(super) closed_by: Option<Seq>,
}

/// What a boot's observer records say, in journal order.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) enum Observed {
    Start { offsets: PerCore<ObservedOffset> },
    Checkpoint { applied_secs: u64 },
    Change { offsets: PerCore<ObservedOffset> },
    CleanStop,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Record {
    pub(super) seq: Seq,
    pub(super) time: Timestamp,
    pub(super) mono_ns: Option<u64>,
    pub(super) what: Observed,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct BootRecords {
    pub(super) records: Vec<Record>,
}

/// A daily event's own commitments.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct EventBook {
    pub(super) first: Seq,
    pub(super) latest: Committed<DayEvent>,
}

/// A raw record that is evidence of a daily event.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Unclean {
    pub(super) seq: Seq,
    pub(super) time: Timestamp,
    pub(super) end: DayUncleanEnd,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct DayMce {
    pub(super) seq: Seq,
    pub(super) time: Timestamp,
    pub(super) boot: BootId,
    pub(super) mce: Mce,
}

/// A machine check the day observer journaled live, in the boot it happened in.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct LiveMce {
    pub(super) seq: Seq,
    pub(super) time: Timestamp,
    pub(super) boot: BootId,
    pub(super) fault: DayFault,
}

/// A crash the user recorded with `togi day record`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct ManualRecord {
    pub(super) seq: Seq,
    pub(super) time: Timestamp,
    pub(super) record: DayRecord,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Answered {
    pub(super) boot: BootId,
    pub(super) answer: togi_journal::DayAnswerKind,
}

/// The day side: raw observer records, the policy's commitments and the applier's applications.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(super) struct Days {
    pub(super) history: Option<DayHistory>,
    #[serde(with = "super::wire::pairs")]
    pub(super) steps: BTreeMap<u64, Committed<DayStep>>,
    pub(super) applications: Vec<Committed<DayApplied>>,
    #[serde(with = "togi_machine::serde_pairs")]
    pub(super) events: HashMap<DayEventId, EventBook>,
    /// Every `day.event` commitment by seq, for what a step cited.
    #[serde(with = "super::wire::pairs")]
    pub(super) event_commits: BTreeMap<Seq, DayEvent>,
    #[serde(with = "togi_machine::serde_pairs")]
    pub(super) vetoes: HashMap<DayEventId, Committed<DayVeto>>,
    pub(super) intervals: Vec<Committed<DayInterval>>,
    pub(super) holds: Vec<Committed<DayHold>>,
    pub(super) freeze: Freeze,
    #[serde(with = "super::wire::pairs")]
    pub(super) boots: BTreeMap<BootId, BootRecords>,
    pub(super) unclean: Vec<Unclean>,
    pub(super) answers: Vec<Answered>,
    pub(super) mces: Vec<DayMce>,
    /// Live machine checks, once per boot and fault identity.
    pub(super) live_mces: Vec<LiveMce>,
    /// Manual crash records, the first per crash identity.
    pub(super) manual: Vec<ManualRecord>,
    /// Original identities of imported day records already folded.
    #[serde(serialize_with = "togi_machine::serde_pairs::serialize_set")]
    pub(super) imported: HashSet<(SessionId, Seq)>,
    /// The seq of the latest `day.event` that changed an event's classification.
    pub(super) reclassified: Option<Seq>,
}

impl Default for Days {
    fn default() -> Self {
        Self {
            history: None,
            steps: BTreeMap::new(),
            applications: Vec::new(),
            events: HashMap::new(),
            event_commits: BTreeMap::new(),
            vetoes: HashMap::new(),
            intervals: Vec::new(),
            holds: Vec::new(),
            freeze: Freeze::Off,
            boots: BTreeMap::new(),
            unclean: Vec::new(),
            answers: Vec::new(),
            mces: Vec::new(),
            live_mces: Vec::new(),
            manual: Vec::new(),
            imported: HashSet::new(),
            reclassified: None,
        }
    }
}
