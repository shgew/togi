use std::collections::BTreeMap;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use togi_machine::object_only;
use togi_machine::{
    BiosContext, BootId, CoreId, EndEvidence, InvalidTarget, KernelFault, KernelTail,
    ObservedOffset, PerCore, ResetKind, ResetReason, TargetOffset,
};

use crate::{Build, Event, Seq};

use super::Payload;

object_only! {
    /// A `togi day` observer began watching this boot. The envelope's boot is the observed one.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DayBootStart {
        pub build: Build,
        pub bios: BiosContext,
        /// The offsets read back as the observer started.
        pub offsets: PerCore<ObservedOffset>,
    }
}

object_only! {
    /// Periodic proof the observer is still running.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DayCheckpoint {
        /// Whole seconds (floor) the observer has been active this boot, cumulative across its `day.boot_start`
        /// records; downtime between restarts is not credited.
        pub applied_secs: u64,
    }
}

object_only! {
    /// The offsets read back differ from the last ones the observer recorded.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DayOffsetsChanged {
        pub offsets: PerCore<ObservedOffset>,
    }
}

object_only! {
    /// The observer stopped on request; its boot did not end.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DayCleanStop {}
}

object_only! {
    /// The previous boot's observer never recorded a clean stop. The envelope's boot is the current one.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DayUncleanEnd {
        /// The predecessor boot whose observer ended without `day.clean_stop`.
        pub boot: BootId,
        /// That boot's last `day.checkpoint`, if it wrote one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub last_checkpoint: Option<Seq>,
        /// The offsets that boot last recorded.
        pub offsets: PerCore<ObservedOffset>,
        pub reset_reason: ResetReason,
        /// What the kernel logs of the crashed boot and of its successor held. Whatever could not be read is
        /// unknown, never an absence of errors; a record written before the evidence was gathered has none, which
        /// reads as unknown.
        #[serde(default = "EndEvidence::unknown")]
        pub evidence: EndEvidence,
    }
}

/// What an unclean end most likely was, from the evidence alone and before anyone answers. Cause is ordered
/// by how specifically the evidence names it: the first rule that fits wins.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndClass {
    /// The CPU overheated: a thermal trip reset.
    Thermal,
    /// An uncorrected machine check, a sync flood or a CPU shutdown: the CPU reported a fault.
    CpuFault,
    /// A graphics driver reported a reset or timeout and the kernel log went on after it: the machine kept
    /// running while graphics died.
    GraphicsSurvived,
    /// A graphics driver reported a reset or timeout and the log ended with it.
    GraphicsLast,
    /// An oops, hung task or other kernel error with no graphics or CPU evidence.
    KernelFault,
    /// The hardware watchdog expired: the machine hung, with no more specific evidence.
    Watchdog,
    /// The power button was held.
    PowerButton,
    /// Power was lost.
    PowerLoss,
    /// Nothing recognizable: a reset by software or a CF9 write, an unrecognized reason, or no evidence readable.
    Unknown,
}

impl DayUncleanEnd {
    /// The classification of the evidence. A tail or count that could not be read never makes `CpuFault`. Past the
    /// CPU evidence, the last error the tail logged decides between graphics and another kernel fault.
    pub fn class(&self) -> EndClass {
        let by_reset = match self.reset_reason.kind {
            Some(ResetKind::ThermalTrip) => EndClass::Thermal,
            Some(ResetKind::SyncFlood | ResetKind::CpuShutdown) => EndClass::CpuFault,
            Some(ResetKind::Watchdog) => EndClass::Watchdog,
            Some(ResetKind::PowerButton) => EndClass::PowerButton,
            Some(ResetKind::PowerLoss) => EndClass::PowerLoss,
            Some(ResetKind::Unknown) | None => EndClass::Unknown,
        };
        if by_reset == EndClass::Thermal {
            return by_reset;
        }
        let machine_check = self
            .evidence
            .tail
            .faults()
            .contains(&KernelFault::MachineCheck {});
        if by_reset == EndClass::CpuFault || self.evidence.has_uncorrected_mce() || machine_check {
            return EndClass::CpuFault;
        }
        match &self.evidence.tail {
            KernelTail::Faults {
                logging_continued, ..
            } => match self.evidence.tail.last_fault() {
                Some(last) if last.is_graphics() && *logging_continued => {
                    EndClass::GraphicsSurvived
                }
                Some(last) if last.is_graphics() => EndClass::GraphicsLast,
                Some(_) | None => EndClass::KernelFault,
            },
            KernelTail::Missing {} | KernelTail::Unreadable {} | KernelTail::Quiet {} => by_reset,
        }
    }

    /// What togi saw, as the journal's `msg` for this event reads it.
    pub fn summary(&self) -> String {
        super::message::day_unclean_end(self)
    }
}

/// What the user says happened when a boot ended.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DayAnswerKind {
    /// The machine froze.
    Frozen,
    /// Graphics died but sound went on.
    Graphics,
    /// The user turned it off while it was working normally.
    PowerOff,
    /// The user is not sure.
    Unsure,
}

impl DayAnswerKind {
    pub const ALL: [Self; 4] = [Self::Frozen, Self::Graphics, Self::PowerOff, Self::Unsure];

    /// The answer as the journal and command line name it: `power_off` on the wire, `power-off` as a command
    /// line argument.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Frozen => "frozen",
            Self::Graphics => "graphics",
            Self::PowerOff => "power_off",
            Self::Unsure => "unsure",
        }
    }

    /// What the answer says, as a clause.
    pub const fn meaning(self) -> &'static str {
        match self {
            Self::Frozen => "the machine froze",
            Self::Graphics => "graphics died but sound went on",
            Self::PowerOff => "the user turned it off while it was working normally",
            Self::Unsure => "the user is not sure what happened",
        }
    }
}

object_only! {
    /// The user's answer about how a previous boot ended. A later answer for the same boot supersedes it.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DayAnswer {
        /// The crashed boot the answer is about; the envelope's boot is the one it was given in.
        pub boot: BootId,
        /// The offsets that boot last recorded, which identify the profile the answer is about.
        pub offsets: PerCore<ObservedOffset>,
        pub answer: DayAnswerKind,
        /// The numeric user id of the caller, as the kernel reported it.
        pub uid: u32,
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Question {
    end: DayUncleanEnd,
    answer: Option<DayAnswer>,
}

/// The unclean ends of previous boots and the answers given about them, folded from the journal.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DayQuestions {
    questions: Vec<Question>,
}

impl DayQuestions {
    pub fn of(events: &[Event]) -> Self {
        let mut questions = Self::default();
        for event in events {
            questions.apply(event);
        }
        questions
    }

    /// Folds one event, in journal order. An unclean end opens a question once per boot; an answer fills it and a
    /// later answer replaces the earlier one. An answer is ignored unless it names a recorded unclean end's boot
    /// and that end's offsets, the profile it is about.
    pub fn apply(&mut self, event: &Event) {
        if let Payload::DayUncleanEnd(end) = &event.payload
            && self.find(&end.boot).is_none()
        {
            self.questions.push(Question {
                end: end.clone(),
                answer: None,
            });
        }
        if let Payload::DayAnswer(answer) = &event.payload
            && let Some(question) = self.questions.iter_mut().find(|question| {
                question.end.boot == answer.boot && question.end.offsets == answer.offsets
            })
        {
            question.answer = Some(answer.clone());
        }
    }

    fn find(&self, boot: &BootId) -> Option<&Question> {
        self.questions
            .iter()
            .find(|question| question.end.boot == *boot)
    }

    /// The unclean ends nobody has answered yet, in journal order.
    pub fn pending(&self) -> impl Iterator<Item = &DayUncleanEnd> {
        self.questions
            .iter()
            .filter(|question| question.answer.is_none())
            .map(|question| &question.end)
    }

    /// The unclean end of `boot`, answered or not.
    pub fn get(&self, boot: &BootId) -> Option<&DayUncleanEnd> {
        self.find(boot).map(|question| &question.end)
    }

    /// The latest answer about `boot`.
    pub fn answer(&self, boot: &BootId) -> Option<&DayAnswer> {
        self.find(boot)
            .and_then(|question| question.answer.as_ref())
    }
}

/// Where the offsets of a recorded crash came from, and when.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OffsetSource {
    /// The user entered them in firmware setup at `at`.
    Entered { at: Timestamp },
    /// A day observer read them back from the CPU at `at`, its last reading of the crashed boot.
    ReadBack { at: Timestamp },
}

/// Which boot a recorded crash ended.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CrashedBoot {
    /// A boot whose `day.unclean_end` the journal holds.
    UncleanEnd { boot: BootId },
    /// A crash the journal holds no unclean end for, such as one before the day observer ran. `at` is when it
    /// happened, as the user gives it.
    Unobserved { at: Timestamp },
}

/// What was running when a recorded crash happened, as a short category.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Running {
    /// Nothing in particular: the machine sat idle.
    Idle,
    /// Desktop start, browsing, editing and other light interactive use.
    Desktop,
    /// Compiling, building or running tests.
    Build,
    /// A game or another graphics load.
    Game,
    /// Something the other categories do not name; the note says what.
    Other,
    /// The user does not know.
    Unknown,
}

impl Running {
    pub const ALL: [Self; 6] = [
        Self::Idle,
        Self::Desktop,
        Self::Build,
        Self::Game,
        Self::Other,
        Self::Unknown,
    ];

    /// The category as the journal and the command line name it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Desktop => "desktop",
            Self::Build => "build",
            Self::Game => "game",
            Self::Other => "other",
            Self::Unknown => "unknown",
        }
    }

    /// What the category says, as a phrase.
    pub const fn meaning(self) -> &'static str {
        match self {
            Self::Idle => "the machine was idle",
            Self::Desktop => "desktop or other light interactive use",
            Self::Build => "builds or tests",
            Self::Game => "a game or another graphics load",
            Self::Other => "something else",
            Self::Unknown => "unknown",
        }
    }
}

/// Why a set of offsets cannot be a crash record's.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum OffsetsError {
    #[error("a crash record must name the offsets that crashed")]
    Empty,
    #[error("core {core:02}")]
    OutOfRange {
        core: CoreId,
        #[source]
        source: InvalidTarget,
    },
}

/// The offsets of a recorded crash: at least one core, each within the range togi applies.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "PerCore<ObservedOffset>", into = "PerCore<ObservedOffset>")]
pub struct RecordedOffsets(PerCore<ObservedOffset>);

impl RecordedOffsets {
    /// # Errors
    /// Refuses offsets that name no core, and an offset outside the range togi applies.
    pub fn new(offsets: PerCore<ObservedOffset>) -> Result<Self, OffsetsError> {
        if offsets.is_empty() {
            return Err(OffsetsError::Empty);
        }
        for (core, offset) in offsets.iter() {
            TargetOffset::new(offset.0)
                .map_err(|source| OffsetsError::OutOfRange { core, source })?;
        }
        Ok(Self(offsets))
    }

    pub const fn get(&self) -> &PerCore<ObservedOffset> {
        &self.0
    }
}

impl TryFrom<PerCore<ObservedOffset>> for RecordedOffsets {
    type Error = OffsetsError;

    fn try_from(offsets: PerCore<ObservedOffset>) -> Result<Self, OffsetsError> {
        Self::new(offsets)
    }
}

impl From<RecordedOffsets> for PerCore<ObservedOffset> {
    fn from(offsets: RecordedOffsets) -> Self {
        offsets.0
    }
}

object_only! {
    /// A crash togi did not observe, recorded with `togi day record`. The envelope's boot is the one it was
    /// recorded in.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DayRecord {
        pub boot: CrashedBoot,
        /// The offsets the crashed boot ran.
        pub offsets: RecordedOffsets,
        pub source: OffsetSource,
        pub running: Running,
        /// Free text about what was running.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub note: Option<String>,
    }
}

impl DayRecord {
    /// What Ruleset 13 will do with the record, in words, so the user reads it before it is appended.
    pub fn effect(&self) -> String {
        super::message::day_record_effect(self)
    }
}

/// An unclean end a crash record can name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordableEnd {
    pub end: DayUncleanEnd,
    /// When the observer of that boot last read its offsets back: its last `day.boot_start`,
    /// `day.offsets_changed` or `day.checkpoint`. A checkpoint counts because the observer read the offsets
    /// and found them unchanged.
    pub read_back: Option<Timestamp>,
    /// The first `day.record` that names this boot.
    pub recorded: Option<Seq>,
}

/// The unclean ends a crash record can name, folded from the journal.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DayCrashes {
    ends: Vec<RecordableEnd>,
    last_read: BTreeMap<BootId, Timestamp>,
}

impl DayCrashes {
    pub fn of(events: &[Event]) -> Self {
        let mut crashes = Self::default();
        for event in events {
            crashes.apply(event);
        }
        crashes
    }

    /// Folds one event, in journal order. An unclean end opens one entry per boot; the first record naming it
    /// marks it recorded.
    pub fn apply(&mut self, event: &Event) {
        if matches!(
            event.payload,
            Payload::DayBootStart(_) | Payload::DayOffsetsChanged(_) | Payload::DayCheckpoint(_)
        ) {
            self.last_read.insert(event.boot.clone(), event.time);
        }
        if let Payload::DayUncleanEnd(end) = &event.payload
            && self.get(&end.boot).is_none()
        {
            self.ends.push(RecordableEnd {
                end: end.clone(),
                read_back: self.last_read.get(&end.boot).copied(),
                recorded: None,
            });
        }
        if let Payload::DayRecord(record) = &event.payload
            && let CrashedBoot::UncleanEnd { boot } = &record.boot
            && let Some(named) = self.ends.iter_mut().find(|named| named.end.boot == *boot)
            && named.recorded.is_none()
        {
            named.recorded = Some(event.seq);
        }
    }

    /// Every unclean end, in journal order.
    pub fn ends(&self) -> &[RecordableEnd] {
        &self.ends
    }

    /// The unclean end of `boot`.
    pub fn get(&self, boot: &BootId) -> Option<&RecordableEnd> {
        self.ends.iter().find(|named| named.end.boot == *boot)
    }

    /// The newest unclean end no record names yet.
    pub fn newest_unrecorded(&self) -> Option<&RecordableEnd> {
        self.ends
            .iter()
            .rev()
            .find(|named| named.recorded.is_none())
    }
}
