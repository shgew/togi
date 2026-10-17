//! The words of the dashboard: the tag column, the tones, the regime and signal words, the phase word and the one
//! table that gives each kind of journal event its words.
//!
//! [`words`] returns one [`Words`] per [`EventKind`], with a history entry, a tag, a compact entry and a detailed
//! entry, and an explicit silent entry where a kind has none. The `match` has no wildcard arm, so a new kind does
//! not compile until it has its words. Sequence-aware context, such as merging neighbouring lines or naming the
//! core a failure moved, stays outside the table: an entry that needs it says [`History::Contextual`] or
//! [`Phrase::Contextual`]. Every text here is built from raw facts; the frames escape at output.

use std::borrow::Cow;
use std::time::Duration;

use togi_journal::{
    Activity, Attribution, CheckingChain, CheckingCycle, CheckingStep, Combination, CorePhase,
    CoreTransition, CycleEvent, DeadEnd, Decision, DeepeningRound, EventKind, Failure, HuntEnd,
    HuntGroup, HuntOutcome, HuntSkipped, HuntStart, Inference, Mce, Payload, RoundEvent, TrialEnd,
    TunerDecision,
};
use togi_machine::{CoreId, Regime, Signal, TargetOffset, WorkloadId};
use togi_tuner::{DecisionPayload, GroupState};

/// The tag of a line of what happened: one short word per kind of event, in a column of its own, so the eye finds
/// a crash without reading the sentence beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tag {
    /// The session started.
    Start,
    /// A trial passed.
    Pass,
    /// A trial or a core failed.
    Fail,
    /// The machine crashed.
    Crash,
    /// A trial ended without a verdict.
    Unclear,
    /// Something was skipped.
    Skip,
    /// A power loss, a reset or a reset command.
    Reset,
    /// A search step or a checking step.
    Step,
    /// A solo limit was found.
    Limit,
    /// A core has room to go deeper.
    Room,
    /// A core backed off.
    Backoff,
    /// A checking cycle.
    Cycle,
    /// A hunt.
    Hunt,
    /// A combination.
    Combo,
    /// A deepening round.
    Round,
    /// The session stopped.
    Stop,
    /// A machine-check error.
    Mce,
    /// A note.
    Note,
    /// A hunt group.
    Group,
    /// A member probe.
    Probe,
    /// A solo limit is being confirmed.
    Confirm,
    /// The session resumed.
    Resume,
    /// A failure was held and nothing moved.
    Hold,
}

impl Tag {
    /// The width of the tag column.
    pub const WIDTH: usize = 7;

    /// The tag as the screen writes it.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Crash => "crash",
            Self::Unclear => "unclear",
            Self::Skip => "skip",
            Self::Reset => "reset",
            Self::Step => "step",
            Self::Limit => "limit",
            Self::Room => "room",
            Self::Backoff => "backoff",
            Self::Cycle => "cycle",
            Self::Hunt => "hunt",
            Self::Combo => "combo",
            Self::Round => "round",
            Self::Stop => "stop",
            Self::Mce => "mce",
            Self::Note => "note",
            Self::Group => "group",
            Self::Probe => "probe",
            Self::Confirm => "confirm",
            Self::Resume => "resume",
            Self::Hold => "hold",
        }
    }
}

/// How a line of what happened is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tone {
    /// Ordinary text.
    #[default]
    Plain,
    /// Good news.
    Good,
    /// Bad news.
    Bad,
    /// A warning.
    Warn,
    /// A hunt.
    Hunt,
    /// A combination.
    Combo,
}

/// A line of what happened as one event words it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The tag column.
    pub tag: Tag,
    /// The sentence.
    pub text: String,
    /// How the line is drawn.
    pub tone: Tone,
}

/// What a state word names: a core in a phase of the search, or the session in an activity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// A core's phase.
    Core(CorePhase),
    /// The session's activity.
    Activity(Activity),
}

impl From<CorePhase> for Phase {
    fn from(phase: CorePhase) -> Self {
        Self::Core(phase)
    }
}

impl From<Activity> for Phase {
    fn from(activity: Activity) -> Self {
        Self::Activity(activity)
    }
}

/// The state word the dashboard and `togi status` show for a core in a phase. An activity has no core word and
/// comes back as its journal value.
pub fn phase_word(phase: impl Into<Phase>) -> &'static str {
    match phase.into() {
        Phase::Core(CorePhase::Search) => "SEARCH",
        Phase::Core(CorePhase::HasRoom) => "HAS ROOM",
        Phase::Core(CorePhase::AtLimit) => "AT LIMIT",
        Phase::Activity(activity) => activity.as_str(),
    }
}

/// How a hunt group's outcome reads in the dashboard and `togi status`.
pub const fn group_word(state: GroupState) -> &'static str {
    match state {
        GroupState::Running => "running",
        GroupState::Passed => "passed",
        GroupState::Failed => "failed",
        GroupState::Skipped => "skipped",
    }
}

/// What a phase-2 candidate does next: it moves, or its limit is checked again in place.
pub const fn candidate_word(moving: bool) -> &'static str {
    if moving {
        "moving"
    } else {
        "checked again in place"
    }
}

/// What a regime loads, in a sentence: "light load", "all-core load".
pub const fn regime_words(regime: Regime) -> &'static str {
    match regime {
        Regime::R1 => "light load",
        Regime::R2 => "heavy vector load",
        Regime::R3 => "load steps",
        Regime::R4 => "partial load",
        Regime::R5 => "both threads",
        Regime::R6 => "idle with short bursts",
        Regime::R7 => "all-core load",
    }
}

/// What a regime loads, short, as it stands in a heading; R6 is "idle + bursts" here.
pub const fn load_words(regime: Regime) -> &'static str {
    match regime {
        Regime::R6 => "idle + bursts",
        Regime::R1 | Regime::R2 | Regime::R3 | Regime::R4 | Regime::R5 | Regime::R7 => {
            regime_words(regime)
        }
    }
}

/// The short name of a kind of load, as it follows its regime code.
pub const fn kind_words(regime: Regime) -> &'static str {
    match regime {
        Regime::R1 => "light",
        Regime::R2 => "heavy vector",
        Regime::R3 => "load steps",
        Regime::R4 => "partial load",
        Regime::R5 => "both threads",
        Regime::R6 => "idle + bursts",
        Regime::R7 => "all-core",
    }
}

/// How a failure signal reads.
pub const fn signal_words(signal: Signal) -> &'static str {
    match signal {
        Signal::ComputationError => "wrong result",
        Signal::UnexpectedExit => "the backend quit",
        Signal::Stall => "the backend stalled",
        Signal::CorrectedMce => "corrected hardware error",
        Signal::UncorrectedMce => "uncorrected hardware error",
        Signal::Crash => "crash",
    }
}

/// The name of a workload as the screen shows it: its base workload's label when it is a variant, else its own.
pub fn workload_display(workload: WorkloadId) -> &'static str {
    let entry = workload.workload();
    togi_machine::workload_by_id(entry.base).map_or(entry.label, |base| base.label)
}

/// `word` in the number `n` takes, without the number.
pub fn noun(n: usize, word: &str) -> String {
    match n {
        1 => word.to_owned(),
        _ if word.ends_with("sh") => format!("{word}es"),
        _ => format!("{word}s"),
    }
}

/// `n` and `word` in the number `n` takes: "1 step", "3 groups".
pub fn plural(n: usize, word: &str) -> String {
    format!("{n} {}", noun(n, word))
}

/// Cores named compactly: a run of three or more consecutive cores becomes a range.
pub fn core_list(cores: impl IntoIterator<Item = CoreId>) -> String {
    let mut sorted: Vec<u8> = cores.into_iter().map(CoreId::get).collect();
    sorted.sort_unstable();
    sorted.dedup();
    if sorted.is_empty() {
        return "no cores".to_owned();
    }
    let mut parts = Vec::new();
    let mut i = 0;
    while i < sorted.len() {
        let mut j = i;
        while j + 1 < sorted.len() && sorted[j + 1] == sorted[j] + 1 {
            j += 1;
        }
        if j - i >= 2 {
            parts.push(format!("{:02}-{:02}", sorted[i], sorted[j]));
        } else {
            parts.extend(sorted[i..=j].iter().map(|core| format!("{core:02}")));
        }
        i = j + 1;
    }
    let prefix = if sorted.len() == 1 { "core" } else { "cores" };
    format!("{prefix} {}", parts.join(", "))
}

/// Cores named without the word: "00-02 05".
pub fn core_ids(cores: impl IntoIterator<Item = CoreId>) -> String {
    let text = core_list(cores);
    let text = text
        .strip_prefix("cores ")
        .or_else(|| text.strip_prefix("core "))
        .unwrap_or(&text);
    text.replace(", ", " ")
}

/// Cores named as a trial's load: "core 03", "all 8 cores" when they are every core, else the list.
pub fn cores_text(cores: &[CoreId], total: usize) -> String {
    match cores {
        [core] => format!("core {:02}", core.get()),
        _ if cores.len() == total && total > 1 => format!("all {total} cores"),
        _ => core_list(cores.iter().copied()),
    }
}

/// Where a load runs: "on core 03", "on 00-07".
pub fn on_cores(cores: &[CoreId]) -> String {
    match cores {
        [core] => format!("on core {:02}", core.get()),
        _ => format!("on {}", core_ids(cores.iter().copied())),
    }
}

/// A span of time as the history writes it: "42s", "3m05s", "2h10m".
pub fn age(span: Duration) -> String {
    let secs = span.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, secs / 60 % 60)
    }
}

/// How far into its planned length a crashed trial got, from its last sample: "early in", "midway through" or
/// "late in"; "during" when either is unknown.
pub fn lateness(last_sample: Option<Duration>, planned: Option<Duration>) -> &'static str {
    let (Some(sample), Some(planned)) = (last_sample, planned) else {
        return "during";
    };
    if planned.is_zero() {
        return "during";
    }
    let fraction = sample.as_secs_f64() / planned.as_secs_f64();
    if fraction < 1.0 / 3.0 {
        "early in"
    } else if fraction < 2.0 / 3.0 {
        "midway through"
    } else {
        "late in"
    }
}

/// The offset a core passed alone, the solo limit a search exit records. A search exit's offset is the margin one
/// count shallower, so the pass is the limit; an exit with no pass has no solo limit.
pub fn solo_limit(transition: &CoreTransition) -> Option<i64> {
    transition.pass.map(TargetOffset::get)
}

/// The words of a tuner decision as the history tells it.
pub fn decision_line(d: &TunerDecision) -> Line {
    let core = d.core.get();
    let (from, to) = (d.from_offset.get(), d.to_offset.get());
    match d.decision {
        Decision::StepDeeper => Line {
            tag: Tag::Step,
            text: format!("core {core:02} passed {from}; searches {to} on its next turn"),
            tone: Tone::Plain,
        },
        Decision::CheckSoloLimit => Line {
            tag: Tag::Confirm,
            text: format!("core {core:02} confirming {to} as its solo limit"),
            tone: Tone::Plain,
        },
        Decision::Deepen => Line {
            tag: Tag::Room,
            text: format!("core {core:02} {from} → {to}"),
            tone: Tone::Good,
        },
        Decision::Yield => Line {
            tag: Tag::Room,
            text: format!("core {core:02} yields {from} → {to}; no later round moves it"),
            tone: Tone::Plain,
        },
        Decision::Hold => Line {
            tag: Tag::Hold,
            text: format!("core {core:02} failure held; tuning stays at {from}"),
            tone: Tone::Warn,
        },
        Decision::Backoff => Line {
            tag: Tag::Backoff,
            text: format!("core {core:02} {from} → {to}"),
            tone: Tone::Warn,
        },
    }
}

/// When a core finds its solo limit, has room again or starts its search over; the other changes show on its row.
pub fn phase_line(d: &CoreTransition) -> Option<Line> {
    let core = d.core.get();
    match (d.from, d.to) {
        (Some(_), CorePhase::Search) => Some(Line {
            tag: Tag::Step,
            text: format!("core {core:02} starts over from {}", d.offset.get()),
            tone: Tone::Warn,
        }),
        (Some(CorePhase::Search), CorePhase::HasRoom | CorePhase::AtLimit) => {
            solo_limit(d).map(|limit| Line {
                tag: Tag::Limit,
                text: format!("core {core:02} solo limit {limit}"),
                tone: Tone::Good,
            })
        }
        (Some(CorePhase::AtLimit), CorePhase::HasRoom) => Some(Line {
            tag: Tag::Room,
            text: format!("core {core:02} has room again at {}", d.offset.get()),
            tone: Tone::Plain,
        }),
        (None, CorePhase::Search | CorePhase::HasRoom | CorePhase::AtLimit)
        | (Some(CorePhase::HasRoom), CorePhase::HasRoom | CorePhase::AtLimit)
        | (Some(CorePhase::AtLimit), CorePhase::AtLimit) => None,
    }
}

/// How a failure reads in the history: a core failing at an offset, or an idle failure; nothing for the rest.
pub fn failure_line(d: &Failure) -> Option<Line> {
    if d.attribution == Attribution::Attributed {
        if let (Some(core), Some(offset)) = (d.core, d.offset) {
            return Some(Line {
                tag: Tag::Fail,
                text: format!("core {:02} fails at {}", core.get(), offset.get()),
                tone: Tone::Warn,
            });
        }
    }
    if d.trial.is_none() && d.signal != Signal::Crash {
        return Some(Line {
            tag: Tag::Fail,
            text: format!("idle with the offsets applied, {}", signal_words(d.signal)),
            tone: Tone::Bad,
        });
    }
    None
}

/// A checking cycle in the history.
pub fn cycle_line(d: &CheckingCycle) -> Line {
    let cycle = d.cycle.get();
    let (text, tone) = if d.event == CycleEvent::Start {
        (
            format!("cycle {cycle} started · {}", plural(d.steps.len(), "step")),
            Tone::Plain,
        )
    } else if d.passed && d.full {
        (format!("cycle {cycle} passed, a full cycle"), Tone::Good)
    } else if d.passed {
        (
            format!("cycle {cycle} passed but missing {}", d.missing.join(", ")),
            Tone::Warn,
        )
    } else {
        (
            format!(
                "cycle {cycle} ended early: {}",
                d.reason.as_deref().unwrap_or_default()
            ),
            Tone::Warn,
        )
    };
    Line {
        tag: Tag::Cycle,
        text,
        tone,
    }
}

/// A combination: its members' offsets when a hunt showed them failing together, its cores when it is an
/// unresolved fallback.
pub fn combination_text(d: &Combination) -> String {
    let id = d.combination.get();
    if d.fallback {
        return format!(
            "C{id} over {} · hunt {} unresolved",
            core_ids(d.members.iter().map(|m| m.core)),
            d.hunt.get()
        );
    }
    let members: Vec<String> = d
        .members
        .iter()
        .map(|m| format!("{:02} {}", m.core.get(), m.offset.get()))
        .collect();
    format!("C{id}: {}", members.join("  "))
}

/// The tag of a trial end.
pub fn trial_end_tag(d: &TrialEnd) -> Tag {
    match (d.outcome, d.signal) {
        (togi_journal::Outcome::Pass, _) => Tag::Pass,
        (
            togi_journal::Outcome::Failure | togi_journal::Outcome::Inconclusive,
            Some(Signal::Crash),
        ) => Tag::Crash,
        (togi_journal::Outcome::Failure, _) => Tag::Fail,
        (togi_journal::Outcome::Inconclusive, _) => Tag::Unclear,
    }
}

fn mce_line(d: &Mce) -> Option<Line> {
    if !d.between_trials {
        return None;
    }
    let kind = if d.corrected {
        "corrected"
    } else {
        "uncorrected"
    };
    let who = d.core.map_or_else(
        || format!("logical CPU {}", d.cpu.0),
        |core| format!("core {:02}", core.get()),
    );
    Some(Line {
        tag: Tag::Mce,
        text: format!("{who} {kind} error between trials (recorded only)"),
        tone: Tone::Warn,
    })
}

/// How a hunt ends, in the detailed story; `named` is whether the branch assumes a named core, which the hunt
/// that ends `direct` or `culprit` then calls "the hunt".
pub fn hunt_end_phrase(d: &HuntEnd, named: bool) -> (String, bool) {
    let hunt = d.hunt.get();
    let cores = core_ids(d.cores);
    match d.result {
        HuntOutcome::Direct | HuntOutcome::Culprit if named => ("the hunt ends".to_owned(), false),
        HuntOutcome::Direct | HuntOutcome::Culprit => {
            (format!("hunt {hunt} ends: {cores} named"), false)
        }
        HuntOutcome::Fallback => (format!("hunt {hunt} unresolved"), true),
        HuntOutcome::Combination => (format!("hunt {hunt} keeps {cores} together"), false),
        HuntOutcome::Cancelled => (format!("hunt {hunt} cancelled"), false),
        HuntOutcome::Loaded => (
            format!("hunt {hunt} keeps the failure on loaded {cores}"),
            false,
        ),
    }
}

/// A move of one core in the detailed story. `failed` is the core a failure in the same branch named, whose
/// backoff records its failure point.
pub fn decision_phrase(d: &TunerDecision, failed: Option<CoreId>) -> String {
    let core = d.core.get();
    let (from, to) = (d.from_offset.get(), d.to_offset.get());
    match d.decision {
        Decision::Backoff => match (failed, d.failure_point) {
            (Some(failed), Some(point)) if failed == d.core => {
                format!("core {core:02} fails at {}, {from} → {to}", point.get())
            }
            _ => format!("core {core:02} {from} → {to}"),
        },
        Decision::StepDeeper => format!("core {core:02} searches {to} on its next turn"),
        Decision::CheckSoloLimit => {
            format!("core {core:02} starts confirming {to} as its solo limit")
        }
        Decision::Deepen => format!("core {core:02} deepens {from} → {to}"),
        Decision::Yield => {
            format!("core {core:02} yields {from} → {to}, back to its round baseline")
        }
        Decision::Hold => format!("core {core:02} failure held at {from}; no tuning move"),
    }
}

/// The tag of a core's move to `phase` that has no history line of its own, such as a core entering its search
/// at the start of a session.
const fn phase_tag(phase: CorePhase) -> Tag {
    match phase {
        CorePhase::Search => Tag::Step,
        CorePhase::HasRoom => Tag::Room,
        CorePhase::AtLimit => Tag::Limit,
    }
}

fn core_phase_phrase(d: &CoreTransition) -> Option<String> {
    let core = d.core.get();
    match (d.from, d.to) {
        (Some(CorePhase::Search), CorePhase::HasRoom | CorePhase::AtLimit) => {
            solo_limit(d).map(|limit| format!("core {core:02} solo limit {limit}"))
        }
        (Some(_), CorePhase::Search) => Some(format!("core {core:02} starts its search over")),
        (None, CorePhase::Search) => Some(format!("core {core:02} starts its search")),
        (
            None | Some(CorePhase::HasRoom | CorePhase::AtLimit),
            CorePhase::HasRoom | CorePhase::AtLimit,
        ) => None,
    }
}

fn chain_phrase(d: &CheckingChain) -> String {
    if d.cores.is_empty() {
        format!("CCD {} partials end", d.ccd.0)
    } else {
        format!("CCD {} partial on {}", d.ccd.0, core_ids(d.cores))
    }
}

/// The history line of a chain note: the cycle and step, the CCD, and the cores the next partial loads, or that
/// the chain ends. The request groups, their provenance and the profile stay in the journal message.
fn chain_summary(d: &CheckingChain) -> String {
    let (cycle, step, ccd) = (d.cycle.get(), d.step.get(), d.ccd.0);
    if d.cores.is_empty() {
        format!("cycle {cycle} step {step} · CCD {ccd} partials end")
    } else {
        format!(
            "cycle {cycle} step {step} · CCD {ccd} partial {} loads {}",
            d.part,
            core_ids(d.cores)
        )
    }
}

fn cycle_phrase(d: &CheckingCycle) -> String {
    let cycle = d.cycle.get();
    if d.event == CycleEvent::Start {
        format!("cycle {cycle} starts")
    } else if d.passed && d.full {
        format!("cycle {cycle} passed, a full cycle")
    } else if d.passed {
        format!("cycle {cycle} passed")
    } else {
        format!("cycle {cycle} ends")
    }
}

fn round_phrase(d: &DeepeningRound) -> String {
    let round = d.round.get();
    if d.event == RoundEvent::Start {
        format!("deepening round {round} starts")
    } else if d.passed {
        format!("deepening round {round} passed")
    } else {
        format!("deepening round {round} ends")
    }
}

fn dead_end_phrase(d: &DeadEnd) -> String {
    format!("tuning stops: {}", d.condition.as_str().replace('_', " "))
}

fn hunt_start_phrase(d: &HuntStart) -> String {
    format!(
        "hunt {} starts over {}",
        d.hunt.get(),
        core_ids(d.candidates)
    )
}

fn backs_off(d: &TunerDecision) -> Option<String> {
    (d.decision == Decision::Backoff).then(|| "backs off".to_owned())
}

fn hunt_brief(d: &HuntStart) -> String {
    format!("hunt {}", d.hunt.get())
}

fn combination_brief(d: &Combination) -> String {
    format!("C{}", d.combination.get())
}

fn combination_phrase(d: &Combination) -> String {
    format!(
        "C{} over {}",
        d.combination.get(),
        core_ids(d.members.iter().map(|m| m.core))
    )
}

const HUNT_ENDS: &str = "hunt ends";
const TUNING_STOPS: &str = "tuning stops";
const NO_HUNT: &str = "no hunt";

/// A skipped hunt with the reason the tuner recorded, which may be a known failure, a held failure or nothing.
fn skip_text(d: &HuntSkipped, lead: &str) -> String {
    if d.reason.is_empty() {
        lead.to_owned()
    } else {
        format!("{lead}: {}", d.reason)
    }
}

fn step_phrase(d: &CheckingStep) -> Option<String> {
    (d.step.get() > 1).then(|| format!("step {} is done", d.step.get() - 1))
}

/// The compact entry of a forecast decision: the [`Words::compact`] of the event it would be, without making one.
#[must_use]
pub fn decision_brief(decision: &DecisionPayload) -> Option<String> {
    match decision {
        DecisionPayload::TunerDecision(d) => backs_off(d),
        DecisionPayload::HuntStart(d) => Some(hunt_brief(d)),
        DecisionPayload::HuntEnd(_) => Some(HUNT_ENDS.to_owned()),
        DecisionPayload::Combination(d) => Some(combination_brief(d)),
        DecisionPayload::DeadEnd(_) => Some(TUNING_STOPS.to_owned()),
        DecisionPayload::HuntSkipped(_) => Some(NO_HUNT.to_owned()),
        DecisionPayload::Failure(_)
        | DecisionPayload::TunerWarning(_)
        | DecisionPayload::CorePhase(_)
        | DecisionPayload::ProfileChange(_)
        | DecisionPayload::CheckingCycle(_)
        | DecisionPayload::CheckingStep(_)
        | DecisionPayload::CheckingChain(_)
        | DecisionPayload::HuntGroup(_)
        | DecisionPayload::DeepeningRound(_) => None,
    }
}

/// The detailed entry of a forecast decision whose words need no context: the [`Words::detailed`] of the event it
/// would be, without making one, and `None` where the table says [`Phrase::Contextual`].
#[must_use]
pub fn decision_detail(decision: &DecisionPayload) -> Option<String> {
    match decision {
        DecisionPayload::TunerDecision(d) => Some(decision_phrase(d, None)),
        DecisionPayload::CorePhase(d) => core_phase_phrase(d),
        DecisionPayload::CheckingStep(d) => step_phrase(d),
        DecisionPayload::CheckingCycle(d) => Some(cycle_phrase(d)),
        DecisionPayload::CheckingChain(d) => Some(chain_phrase(d)),
        DecisionPayload::HuntStart(d) => Some(hunt_start_phrase(d)),
        DecisionPayload::HuntSkipped(d) => Some(skip_text(d, NO_HUNT)),
        DecisionPayload::Combination(d) => Some(combination_phrase(d)),
        DecisionPayload::DeepeningRound(d) => Some(round_phrase(d)),
        DecisionPayload::DeadEnd(d) => Some(dead_end_phrase(d)),
        DecisionPayload::Failure(_)
        | DecisionPayload::TunerWarning(_)
        | DecisionPayload::ProfileChange(_)
        | DecisionPayload::HuntGroup(_)
        | DecisionPayload::HuntEnd(_) => None,
    }
}

fn group_tag(d: &HuntGroup) -> Tag {
    if d.skipped || d.inferred.is_some() {
        Tag::Skip
    } else if d.probe.is_some() {
        Tag::Probe
    } else {
        Tag::Group
    }
}

/// How a kind of event reads in the history.
#[derive(Clone, Copy, Debug)]
pub enum History {
    /// The kind never has a history line.
    Silent,
    /// The line one event of this kind words by itself; `None` when this event has none.
    Line(fn(&Payload) -> Option<Line>),
    /// The line depends on earlier events, or changes the lines they began; the history builds it.
    Contextual,
}

/// How a kind of event is tagged in the log.
#[derive(Clone, Copy, Debug)]
pub enum Tagging {
    /// The kind's own name with dots as spaces.
    Kind,
    /// One tag for every event of the kind.
    Fixed(Tag),
    /// The tag depends on the payload.
    By(fn(&Payload) -> Tag),
}

/// How a kind of event reads in a compact outcome, as the first decision a forecast branch records.
#[derive(Clone, Copy, Debug)]
pub enum Brief {
    /// The kind never names a compact outcome.
    Silent,
    /// The compact words of this event.
    Of(fn(&Payload) -> Option<String>),
}

/// How a kind of event reads in a detailed outcome phrase.
#[derive(Clone, Copy, Debug)]
pub enum Phrase {
    /// The kind never has a phrase.
    Silent,
    /// The phrase of one event by itself. The consumer still drops what the branch makes redundant: a branch
    /// that assumes a named core speaks of "it" and omits the starts-over phrase.
    Of(fn(&Payload) -> Option<String>),
    /// The phrase depends on the events around it: known failures, carried hunt groups, how a hunt ends.
    Contextual,
}

/// Everything one kind of journal event says.
#[derive(Clone, Copy, Debug)]
pub struct Words {
    /// Whether the log view lists events of this kind.
    pub listed: bool,
    /// Its history line.
    pub history: History,
    /// Its tag in the log.
    pub tag: Tagging,
    /// Its compact outcome words.
    pub compact: Brief,
    /// Its detailed outcome phrase.
    pub detailed: Phrase,
}

impl Words {
    /// A kind with no words of its own: listed in the log under its kind name, and never in the history or a
    /// story.
    pub const SILENT: Self = Self {
        listed: true,
        history: History::Silent,
        tag: Tagging::Kind,
        compact: Brief::Silent,
        detailed: Phrase::Silent,
    };

    /// A kind the log does not list and that has no words.
    pub const UNLISTED: Self = Self {
        listed: false,
        ..Self::SILENT
    };
}

macro_rules! of {
    ($variant:ident, |$d:ident| $body:expr) => {
        |payload: &Payload| match payload {
            Payload::$variant($d) => $body,
            _ => None,
        }
    };
}

/// The words of a kind of journal event.
pub fn words(kind: EventKind) -> Words {
    match kind {
        EventKind::SessionStart => Words {
            history: History::Contextual,
            tag: Tagging::Fixed(Tag::Start),
            ..Words::SILENT
        },
        EventKind::SessionContext
        | EventKind::SessionBaseline
        | EventKind::SessionNotice
        | EventKind::SessionArchived
        | EventKind::SmuError
        | EventKind::ProfileApplied
        | EventKind::ProfileChange
        | EventKind::ProfileRestored
        | EventKind::TrialStart
        | EventKind::TrialProgress
        | EventKind::TrialSignal
        | EventKind::TrialSample
        | EventKind::TrialCarried
        | EventKind::FailureCarried
        | EventKind::HostRanking
        | EventKind::TunerWarning
        | EventKind::DefectAnswered
        | EventKind::BootSavedEntry
        | EventKind::BootLeaveReason
        | EventKind::JournalTorn
        | EventKind::StateRebuilt => Words::SILENT,
        EventKind::PreflightCheck
        | EventKind::SmuIntent
        | EventKind::SmuWrite
        | EventKind::SmuReadback => Words::UNLISTED,
        EventKind::SessionWarning | EventKind::DefectFound | EventKind::BackendRetry => Words {
            history: History::Line(|payload| {
                Some(Line {
                    tag: Tag::Note,
                    text: payload.message(),
                    tone: Tone::Warn,
                })
            }),
            ..Words::SILENT
        },
        EventKind::SessionCarried | EventKind::ConfigLoaded | EventKind::TrialIntent => Words {
            history: History::Contextual,
            ..Words::SILENT
        },
        EventKind::TrialEnd => Words {
            history: History::Contextual,
            tag: Tagging::By(|payload| match payload {
                Payload::TrialEnd(d) => trial_end_tag(d),
                _ => Tag::Unclear,
            }),
            ..Words::SILENT
        },
        EventKind::Failure => Words {
            history: History::Contextual,
            tag: Tagging::By(|payload| match payload {
                Payload::Failure(d) if d.known_failure.is_some() => Tag::Skip,
                _ => Tag::Fail,
            }),
            detailed: Phrase::Contextual,
            ..Words::SILENT
        },
        EventKind::Mce => Words {
            history: History::Line(of!(Mce, |d| mce_line(d))),
            ..Words::SILENT
        },
        EventKind::CrashDetected => Words {
            history: History::Contextual,
            tag: Tagging::Fixed(Tag::Crash),
            ..Words::SILENT
        },
        EventKind::TunerDecision => Words {
            history: History::Line(of!(TunerDecision, |d| Some(decision_line(d)))),
            tag: Tagging::By(|payload| match payload {
                Payload::TunerDecision(d) => decision_line(d).tag,
                _ => Tag::Backoff,
            }),
            compact: Brief::Of(of!(TunerDecision, |d| backs_off(d))),
            detailed: Phrase::Of(of!(TunerDecision, |d| Some(decision_phrase(d, None)))),
            ..Words::SILENT
        },
        EventKind::CorePhase => Words {
            history: History::Contextual,
            tag: Tagging::By(|payload| match payload {
                Payload::CorePhase(d) => {
                    phase_line(d).map_or_else(|| phase_tag(d.to), |line| line.tag)
                }
                _ => Tag::Limit,
            }),
            detailed: Phrase::Of(of!(CorePhase, |d| core_phase_phrase(d))),
            ..Words::SILENT
        },
        EventKind::CheckingCycle => Words {
            history: History::Line(of!(CheckingCycle, |d| Some(cycle_line(d)))),
            tag: Tagging::Fixed(Tag::Cycle),
            detailed: Phrase::Of(of!(CheckingCycle, |d| Some(cycle_phrase(d)))),
            ..Words::SILENT
        },
        EventKind::CheckingStep => Words {
            history: History::Contextual,
            tag: Tagging::Fixed(Tag::Step),
            detailed: Phrase::Of(of!(CheckingStep, |d| step_phrase(d))),
            ..Words::SILENT
        },
        EventKind::CheckingChain => Words {
            history: History::Line(of!(CheckingChain, |d| Some(Line {
                tag: Tag::Note,
                text: chain_summary(d),
                tone: Tone::Plain,
            }))),
            detailed: Phrase::Of(of!(CheckingChain, |d| Some(chain_phrase(d)))),
            ..Words::SILENT
        },
        EventKind::HuntStart => Words {
            history: History::Contextual,
            tag: Tagging::Fixed(Tag::Hunt),
            compact: Brief::Of(of!(HuntStart, |d| Some(hunt_brief(d)))),
            detailed: Phrase::Of(of!(HuntStart, |d| Some(hunt_start_phrase(d)))),
            ..Words::SILENT
        },
        EventKind::HuntGroup => Words {
            history: History::Contextual,
            tag: Tagging::By(|payload| match payload {
                Payload::HuntGroup(d) => group_tag(d),
                _ => Tag::Group,
            }),
            detailed: Phrase::Contextual,
            ..Words::SILENT
        },
        EventKind::HuntEnd => Words {
            history: History::Contextual,
            tag: Tagging::Fixed(Tag::Hunt),
            compact: Brief::Of(|_| Some(HUNT_ENDS.to_owned())),
            detailed: Phrase::Contextual,
            ..Words::SILENT
        },
        EventKind::HuntSkipped => Words {
            history: History::Line(of!(HuntSkipped, |d| Some(Line {
                tag: Tag::Hunt,
                text: skip_text(d, "not hunted"),
                tone: Tone::Plain,
            }))),
            tag: Tagging::Fixed(Tag::Hunt),
            compact: Brief::Of(|_| Some(NO_HUNT.to_owned())),
            detailed: Phrase::Of(of!(HuntSkipped, |d| Some(skip_text(d, NO_HUNT)))),
            ..Words::SILENT
        },
        EventKind::Combination => Words {
            history: History::Line(of!(Combination, |d| Some(Line {
                tag: Tag::Combo,
                text: combination_text(d),
                tone: Tone::Combo,
            }))),
            tag: Tagging::Fixed(Tag::Combo),
            compact: Brief::Of(of!(Combination, |d| Some(combination_brief(d)))),
            detailed: Phrase::Of(of!(Combination, |d| Some(combination_phrase(d)))),
            ..Words::SILENT
        },
        EventKind::DeepeningRound => Words {
            history: History::Contextual,
            tag: Tagging::Fixed(Tag::Round),
            detailed: Phrase::Of(of!(DeepeningRound, |d| Some(round_phrase(d)))),
            ..Words::SILENT
        },
        EventKind::CommandReset => Words {
            history: History::Line(|_| {
                Some(Line {
                    tag: Tag::Reset,
                    text: "requested from the command line".to_owned(),
                    tone: Tone::Plain,
                })
            }),
            ..Words::SILENT
        },
        EventKind::DeadEnd => Words {
            history: History::Line(of!(DeadEnd, |d| Some(Line {
                tag: Tag::Stop,
                text: format!("dead end: {}", d.detail),
                tone: Tone::Bad,
            }))),
            tag: Tagging::Fixed(Tag::Stop),
            compact: Brief::Of(|_| Some(TUNING_STOPS.to_owned())),
            detailed: Phrase::Of(of!(DeadEnd, |d| Some(dead_end_phrase(d)))),
            ..Words::SILENT
        },
        EventKind::Shutdown => Words {
            history: History::Line(|_| {
                Some(Line {
                    tag: Tag::Stop,
                    text: "stopped".to_owned(),
                    tone: Tone::Plain,
                })
            }),
            tag: Tagging::Fixed(Tag::Stop),
            ..Words::SILENT
        },
    }
}

/// The tag column of an event's line in the log: the tag word, or the kind's name with dots as spaces.
pub fn tag_word(payload: &Payload) -> Cow<'static, str> {
    let kind = payload.kind();
    match words(kind).tag {
        Tagging::Kind => Cow::Owned(kind.as_str().replace('.', " ")),
        Tagging::Fixed(tag) => Cow::Borrowed(tag.word()),
        Tagging::By(of) => Cow::Borrowed(of(payload).word()),
    }
}

/// The phrase of a run of hunt groups answered by carried trials, and whether it is a minor one.
pub fn carried_groups(first: u32, last: u32, inferred: Inference) -> (String, bool) {
    let groups = if last > first {
        format!("groups {first}-{last}")
    } else {
        format!("group {first}")
    };
    if inferred == Inference::Pass {
        (format!("{groups} answered by existing evidence"), true)
    } else {
        (format!("{groups} failed on existing evidence"), false)
    }
}
