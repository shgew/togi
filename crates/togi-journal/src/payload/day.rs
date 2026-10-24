use serde::{Deserialize, Serialize};
use togi_machine::object_only;
use togi_machine::{
    BiosContext, BootId, EndEvidence, KernelFault, KernelTail, ObservedOffset, PerCore, ResetKind,
    ResetReason, WatchdogState,
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
        /// Whether PID 1's hardware watchdog was armed as the observer started. Absent in a record written
        /// before the observer checked, which reads as unknown.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub watchdog: Option<WatchdogState>,
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
    /// The hardware watchdog expired while the crashed boot's log ended at a suspend or hibernation entry: the
    /// machine was going to sleep, which is not a CPU hang.
    Sleep,
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
        if by_reset == EndClass::Watchdog && self.evidence.asleep.is_some() {
            return EndClass::Sleep;
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
