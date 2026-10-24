use serde::{Deserialize, Serialize};
use togi_machine::object_only;
use togi_machine::{
    BankType, BiosContext, BootId, CoreId, CpuId, EndEvidence, KernelErrorKind, KernelFault,
    KernelTail, ObservedOffset, PerCore, ProcessFaultKind, ResetKind, ResetReason,
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
        /// How the sampled part of that active time divided among the load shapes, cumulative like `applied_secs`.
        /// A checkpoint written before the mix was sampled has none, which reads as zero.
        #[serde(default)]
        pub load_mix: LoadMix,
    }
}

object_only! {
    /// Milliseconds of sampled active time per load shape. Each sample interval counts toward exactly one shape,
    /// so the sum is the sampled time; an interval the sampler missed (a gap over three sample periods) counts
    /// toward none.
    #[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct LoadMix {
        /// A few cores busy, the rest idle or light: the shape single-thread boost needs.
        #[serde(default)]
        pub single_thread_ms: u64,
        /// At least half the cores busy and one lightly loaded, like regime R8.
        #[serde(default)]
        pub light_beside_busy_ms: u64,
        /// Every core busy, like regime R9.
        #[serde(default)]
        pub all_threads_ms: u64,
        /// Intervals in which the machine went from no core busy to some busy, or back.
        #[serde(default)]
        pub idle_transition_ms: u64,
        /// Intervals with no core busy, after an interval with none: idle or lightly used.
        #[serde(default)]
        pub quiet_ms: u64,
        /// Active intervals that fit none of the shapes above.
        #[serde(default)]
        pub other_ms: u64,
    }
}

impl LoadMix {
    /// The sampled time.
    pub fn total_ms(&self) -> u64 {
        [
            self.single_thread_ms,
            self.light_beside_busy_ms,
            self.all_threads_ms,
            self.idle_transition_ms,
            self.quiet_ms,
            self.other_ms,
        ]
        .into_iter()
        .fold(0, u64::saturating_add)
    }
}

/// What a userspace process was killed for, among the traps a flawed core can cause.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Trap {
    /// SIGILL: an invalid opcode.
    IllegalInstruction,
    /// SIGBUS.
    Bus,
    /// A general-protection fault in the process.
    GeneralProtection,
}

impl Trap {
    /// The trap a process fault is, if it is one the day service records; a segmentation fault or abort is not.
    pub const fn of(kind: ProcessFaultKind) -> Option<Self> {
        match kind {
            ProcessFaultKind::IllegalInstruction => Some(Self::IllegalInstruction),
            ProcessFaultKind::Bus => Some(Self::Bus),
            ProcessFaultKind::GeneralProtection => Some(Self::GeneralProtection),
            ProcessFaultKind::Segmentation | ProcessFaultKind::Abort => None,
        }
    }
}

object_only! {
    /// A fault the kernel logged while the machine ran normally.
    #[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
    pub enum LiveFault {
        MachineCheck {
            cpu: CpuId,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            core: Option<CoreId>,
            bank: u16,
            bank_type: BankType,
            corrected: bool,
        },
        KernelError { error: KernelErrorKind },
        Trap {
            trap: Trap,
            name: String,
            pid: u32,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            cpu: Option<CpuId>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            core: Option<CoreId>,
        },
    }
}

object_only! {
    /// A live fault, journaled once. The envelope's boot is the boot it happened in.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct DayFault {
        pub fault: LiveFault,
        /// When the kernel logged it, since boot.
        pub monotonic_ns: u64,
        /// The offsets last read back before the observer recorded the fault: the profile the machine ran.
        pub offsets: PerCore<ObservedOffset>,
        /// The log lines that reported it.
        pub lines: Vec<String>,
    }
}

impl DayFault {
    /// What identifies the fault within its boot, which is how the same log line is journaled once however often
    /// the observer restarts. A machine check or kernel error is told apart by its content and time. A trap is
    /// told apart by its kind, process name and pid alone: the kernel line and the core dump record of one
    /// killed process carry different times and CPU evidence, and are one fault.
    pub fn identity(&self) -> (LiveFault, u64) {
        match &self.fault {
            LiveFault::Trap {
                trap, name, pid, ..
            } => (
                LiveFault::Trap {
                    trap: *trap,
                    name: name.clone(),
                    pid: *pid,
                    cpu: None,
                    core: None,
                },
                0,
            ),
            LiveFault::MachineCheck { .. } | LiveFault::KernelError { .. } => {
                (self.fault.clone(), self.monotonic_ns)
            }
        }
    }
}

impl LiveFault {
    /// The core the fault names, when the log did.
    pub const fn core(&self) -> Option<CoreId> {
        match self {
            Self::MachineCheck { core, .. } | Self::Trap { core, .. } => *core,
            Self::KernelError { .. } => None,
        }
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
