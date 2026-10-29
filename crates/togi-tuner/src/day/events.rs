use jiff::Timestamp;
use togi_journal::{DayAnswerKind, EndClass};
use togi_machine::BootId;

use super::profile::CoreProfile;

/// The stable identity of one daily event (`docs/spec/tuner.md`, Manual crash records): every
/// observation of one end, whatever its source, is one event and one escalation strike.
#[derive(
    Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub enum EventId {
    /// A crashed boot: its unclean end, its answers, a clean boot's machine checks and a manual
    /// record or import naming it.
    Boot(BootId),
    /// A manual record of a crash no observer saw, identified by the crash instant.
    Unobserved(Timestamp),
    /// The night's failed monthly re-qualification of D, identified by the sequence of the plan that
    /// opened the night.
    Requalification(togi_journal::Seq),
    /// An original-namespace identity retained when only a predecessor receipt is available.
    Imported(togi_journal::Provenance),
}

/// A machine check that is CPU evidence by itself, as the journal's `Mce` payload tells core-local
/// banks from the rest.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum MceKind {
    CoreLocal,
    Uncorrected,
}

/// One piece of raw evidence about an event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Witness {
    /// `day.unclean_end`: its journal class and, beyond what the class folds in, a core-local
    /// machine check found in the crashed or successor boot's logs.
    UncleanEnd {
        class: EndClass,
        mce: Option<MceKind>,
    },
    /// A cleanly ended boot that logged a core-local or uncorrected machine check.
    CleanBootMce(MceKind),
    /// `day.record`: a manually reported crash.
    Manual,
    /// A night's failed monthly re-qualification of the applied profile.
    FailedRequalification,
}

/// A raw observation of a daily event: its identity, the profile it actually ran at and one witness. Observations of one identity merge into one [`DailyEvent`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Observation {
    pub id: EventId,
    pub profile: CoreProfile,
    pub witness: Witness,
}

/// The user's answer about a crashed boot. Answers are given in journal order; the last one for a
/// boot wins.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Answer {
    pub boot: BootId,
    pub kind: DayAnswerKind,
}

/// What made an event CPU-suspect for certain.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CpuCause {
    /// A core-local or uncorrected machine check, which no answer erases.
    Mce(MceKind),
    /// The end's own evidence: an uncorrected machine check, sync flood or CPU shutdown.
    CpuFaultEnd,
    /// The user answered `frozen`.
    FrozenAnswer,
    /// A manual crash record.
    ManualRecord,
    /// A failed monthly re-qualification.
    FailedRequalification,
}

/// Why an unexplained end at a nonzero D is provisionally CPU-suspect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProvisionalCause {
    /// The user answered `unsure`.
    UnsureAnswer,
    /// An Unknown or CF9 end nobody answered.
    UnansweredUnknown,
}

/// The tuner's reading of an event (`docs/spec/tuner.md`, Classification and pending questions).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Classification {
    /// Definitely CPU-suspect: whole-profile veto, step back, reproduction.
    CpuSuspect(CpuCause),
    /// Provisionally CPU-suspect at a nonzero D: the same veto and step back until an answer
    /// reclassifies it.
    ProvisionalCpuSuspect(ProvisionalCause),
    /// Another cause; the fault stays visible, the hold stays.
    NonCpu,
    /// At CO 0: platform or background evidence, no veto, no CCD charged.
    Platform,
    /// An unexplained end whose question is unanswered and which is not Unknown or CF9: no
    /// classification, so an unresolved event.
    PendingQuestion,
}

impl Classification {
    /// Whether the event vetoes its profile and demands a step back.
    pub const fn is_cpu_suspect(self) -> bool {
        match self {
            Self::CpuSuspect(_) | Self::ProvisionalCpuSuspect(_) => true,
            Self::NonCpu | Self::Platform | Self::PendingQuestion => false,
        }
    }

    pub const fn certainty(self) -> Option<Certainty> {
        match self {
            Self::CpuSuspect(_) => Some(Certainty::Definite),
            Self::ProvisionalCpuSuspect(_) => Some(Certainty::Provisional),
            Self::NonCpu | Self::Platform | Self::PendingQuestion => None,
        }
    }

    /// How much an observation weighs when several witness one event: the strongest wins.
    const fn weight(self) -> u8 {
        match self {
            Self::CpuSuspect(_) => 4,
            Self::ProvisionalCpuSuspect(_) => 3,
            Self::PendingQuestion => 2,
            Self::NonCpu => 1,
            Self::Platform => 0,
        }
    }
}

/// Whether a CPU-suspect event stands on definite evidence or on an unexplained end.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Certainty {
    Definite,
    Provisional,
}

/// The state of the question an unclean end opens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum QuestionState {
    /// The event came from no unclean end, so nothing was asked.
    NotAsked,
    /// Unanswered and blocking promotion.
    Pending,
    /// Unanswered, but its provisional event has formed: still answerable, no longer pending.
    Parked,
    Answered(DayAnswerKind),
}

/// One daily event: every observation of one end, its answer and its classification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DailyEvent {
    pub id: EventId,
    /// The actual failed profile: the unclean end's read-back when there is one, else the first
    /// record's.
    pub profile: CoreProfile,
    pub witnesses: Vec<Witness>,
    pub question: QuestionState,
    pub class: Classification,
}

/// Whether an end's evidence leaves room for an answer to decide.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Base {
    Cpu,
    Explained,
    Unexplained(Unanswered),
}

/// What an unanswered unexplained end becomes at a nonzero D.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Unanswered {
    Provisional,
    QuestionOnly,
}

/// The classification an end's evidence gives before anyone answers, one explicit arm per
/// [`EndClass`].
const fn base(class: EndClass) -> Base {
    match class {
        // "Other faults remain visible and reset the hold only if classified CPU-suspect": a
        // thermal trip is cooling, not the Curve Optimizer, and a frozen answer still overrides it
        // below because "a `frozen` answer is CPU-suspect".
        EndClass::Thermal => Base::Explained,
        // A watchdog reset at a suspend or hibernation entry is a sleep that bypassed the watchdog
        // unit, not a CPU hang (`docs/spec/runtime.md`, Day watchdog): explained like a thermal
        // trip, and a `frozen` answer still overrides it.
        EndClass::Sleep => Base::Explained,
        // "Classify a core-local or uncorrected MCE as CPU-suspect" and "an answer cannot erase
        // independent core-local/uncorrected MCE evidence": the end's CPU fault (uncorrected machine
        // check, sync flood, CPU shutdown) is not "otherwise unexplained", so no answer
        // reclassifies it.
        EndClass::CpuFault => Base::Cpu,
        // The machine kept running while graphics died: the recognized graphics fault explains the
        // end, which `graphics` or `power_off` only confirm.
        EndClass::GraphicsSurvived | EndClass::GraphicsLast => Base::Explained,
        // An oops or hung task with no graphics or CPU evidence does not exclude the CPU, so only
        // an answer decides ("`graphics` or `power_off` reclassifies an otherwise unexplained
        // end"). Unanswered it stays a pending question: the provisional rule names only Unknown
        // and CF9 ends.
        EndClass::KernelFault => Base::Unexplained(Unanswered::QuestionOnly),
        // A hung machine with no more specific evidence, a held power button (how a frozen machine
        // is usually ended) and lost power are likewise unexplained and answer-decided; only
        // Unknown and CF9 ends become provisional when unanswered ("an Unknown or CF9 end left
        // unanswered").
        EndClass::Watchdog | EndClass::PowerButton | EndClass::PowerLoss => {
            Base::Unexplained(Unanswered::QuestionOnly)
        }
        // "An unanswered Unknown or CF9 end at nonzero D creates a provisional retreat of the
        // newest step."
        EndClass::Unknown => Base::Unexplained(Unanswered::Provisional),
    }
}

/// The classification of one unclean end, ignoring CO 0.
const fn classify_end(class: EndClass, answer: Option<DayAnswerKind>) -> Classification {
    match (base(class), answer) {
        (Base::Cpu, _) => Classification::CpuSuspect(CpuCause::CpuFaultEnd),
        // "A `frozen` answer is CPU-suspect."
        (_, Some(DayAnswerKind::Frozen)) => Classification::CpuSuspect(CpuCause::FrozenAnswer),
        // "`graphics` or `power_off` reclassifies an otherwise unexplained end as non-CPU."
        (_, Some(DayAnswerKind::Graphics | DayAnswerKind::PowerOff)) => Classification::NonCpu,
        // "`unsure` supplies no definitive cause": an explained end keeps its own non-CPU reading.
        (Base::Explained, Some(DayAnswerKind::Unsure) | None) => Classification::NonCpu,
        // "An `unsure` answer ... becomes a provisional CPU-suspect event" at nonzero D.
        (Base::Unexplained(_), Some(DayAnswerKind::Unsure)) => {
            Classification::ProvisionalCpuSuspect(ProvisionalCause::UnsureAnswer)
        }
        (Base::Unexplained(Unanswered::Provisional), None) => {
            Classification::ProvisionalCpuSuspect(ProvisionalCause::UnansweredUnknown)
        }
        (Base::Unexplained(Unanswered::QuestionOnly), None) => Classification::PendingQuestion,
    }
}

const fn classify_witness(witness: Witness, answer: Option<DayAnswerKind>) -> Classification {
    match witness {
        Witness::UncleanEnd {
            mce: Some(kind), ..
        }
        | Witness::CleanBootMce(kind) => Classification::CpuSuspect(CpuCause::Mce(kind)),
        Witness::UncleanEnd { class, mce: None } => classify_end(class, answer),
        Witness::Manual => Classification::CpuSuspect(CpuCause::ManualRecord),
        Witness::FailedRequalification => {
            Classification::CpuSuspect(CpuCause::FailedRequalification)
        }
    }
}

/// Classifies an event from all its witnesses and the last answer about its boot. The strongest
/// witness decides, so an answer never erases independent machine-check evidence, and an event at
/// CO 0 is platform evidence whatever its witnesses say.
pub fn classify(
    profile: &CoreProfile,
    first: Witness,
    rest: &[Witness],
    answer: Option<DayAnswerKind>,
) -> Classification {
    if profile.is_zero() {
        return Classification::Platform;
    }
    rest.iter()
        .map(|&witness| classify_witness(witness, answer))
        .fold(classify_witness(first, answer), |best, next| {
            if next.weight() > best.weight() {
                next
            } else {
                best
            }
        })
}

struct Building {
    id: EventId,
    profile: CoreProfile,
    profile_from_end: bool,
    first: Witness,
    rest: Vec<Witness>,
}

impl Building {
    fn has_end(&self) -> bool {
        self.witnesses()
            .any(|witness| matches!(witness, Witness::UncleanEnd { .. }))
    }

    fn witnesses(&self) -> impl Iterator<Item = Witness> + '_ {
        std::iter::once(self.first).chain(self.rest.iter().copied())
    }
}

/// Merges raw observations (in journal order) and answers into classified events, ordered by first
/// observation. Observations of one identity are one event; an answer applies to the event of its
/// boot that has an unclean end, the last answer winning.
pub fn daily_events(observations: &[Observation], answers: &[Answer]) -> Vec<DailyEvent> {
    let mut building: Vec<Building> = Vec::new();
    for observation in observations {
        let from_end = matches!(observation.witness, Witness::UncleanEnd { .. });
        match building.iter_mut().find(|event| event.id == observation.id) {
            Some(event) => {
                event.rest.push(observation.witness);
                if from_end && !event.profile_from_end {
                    event.profile = observation.profile;
                    event.profile_from_end = true;
                }
            }
            None => building.push(Building {
                id: observation.id.clone(),
                profile: observation.profile,
                profile_from_end: from_end,
                first: observation.witness,
                rest: Vec::new(),
            }),
        }
    }
    building
        .into_iter()
        .map(|event| {
            let answer = match &event.id {
                EventId::Boot(boot) if event.has_end() => answers
                    .iter()
                    .rev()
                    .find(|answer| &answer.boot == boot)
                    .map(|answer| answer.kind),
                EventId::Boot(_)
                | EventId::Unobserved(_)
                | EventId::Requalification(_)
                | EventId::Imported(_) => None,
            };
            let class = classify(&event.profile, event.first, &event.rest, answer);
            let question = match (event.has_end(), answer) {
                (false, _) => QuestionState::NotAsked,
                (true, Some(kind)) => QuestionState::Answered(kind),
                (true, None) => match class {
                    Classification::ProvisionalCpuSuspect(_) => QuestionState::Parked,
                    Classification::CpuSuspect(_)
                    | Classification::NonCpu
                    | Classification::Platform
                    | Classification::PendingQuestion => QuestionState::Pending,
                },
            };
            DailyEvent {
                id: event.id,
                profile: event.profile,
                witnesses: std::iter::once(event.first).chain(event.rest).collect(),
                question,
                class,
            }
        })
        .collect()
}
