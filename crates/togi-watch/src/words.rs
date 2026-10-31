//! The words of the dashboard: the tag column, the tones, the regime and signal words and the one table that gives
//! each kind of journal event its words.
//!
//! [`words`] returns one [`Words`] per [`EventKind`], with a history entry, a tag and whether the log lists it, and
//! an explicit silent entry where a kind has none. The `match` has no wildcard arm, so a new kind does not
//! compile until it has its words. Sequence-aware context, such as merging passes or naming the core a failure
//! moved, stays outside the table: an entry that needs it says [`History::Contextual`]. The kinds of earlier
//! rulesets are still decoded; their history line is the message the journal recorded, never an interpretation.
//! Every text here is built from raw facts; the frames escape at output.

use std::borrow::Cow;
use std::time::Duration;

use togi_journal::{
    Attribution, Binding, CcdCap, CcdEdge, CcdVector, Certainty, Classification, DayApplied,
    DayEvent, DayEventId, DayFreeze, DayHold, DayStep, DayVeto, Edge, EventKind, Failure,
    FreezeChoice, HoldChange, Mce, NightPlan, NightQualification, NightReproduction,
    ObservedProfile, Payload, QualificationPurpose, QualificationState, ReproductionState,
    ReproductionTarget, Resolution, SessionImported, Slope, StepKind, TrialEnd, VetoState,
};
use togi_machine::{CoreId, CoreSet, Regime, Signal, WorkloadId};

/// The tag of a line of what happened: one short word per kind of event, in a column of its own, so the eye finds
/// a crash without reading the sentence beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Tag {
    /// The session started.
    Start,
    /// A trial passed.
    Pass,
    /// A trial failed.
    Fail,
    /// The machine crashed.
    Crash,
    /// A trial ended without a verdict.
    Unclear,
    /// A power loss, a reset or a reset command.
    Reset,
    /// A day step was proposed.
    Step,
    /// The session stopped.
    Stop,
    /// A machine-check error.
    Mce,
    /// A note.
    Note,
    /// The session resumed.
    Resume,
    /// A day hold changed.
    Hold,
    /// A night was planned.
    Night,
    /// A CCD's edge changed.
    Edge,
    /// A CCD's non-authorizing statistical diagnostic changed.
    Cap,
    /// A night qualification moved.
    Qualify,
    /// A night reproduction moved.
    Repro,
    /// A daily event.
    Day,
    /// A whole-profile veto.
    Veto,
    /// A day profile was applied.
    Apply,
    /// Promotion was frozen or unfrozen.
    Freeze,
    /// Records were imported.
    Import,
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
            Self::Reset => "reset",
            Self::Step => "step",
            Self::Stop => "stop",
            Self::Mce => "mce",
            Self::Note => "note",
            Self::Resume => "resume",
            Self::Hold => "hold",
            Self::Night => "night",
            Self::Edge => "edge",
            Self::Cap => "cap",
            Self::Qualify => "qualify",
            Self::Repro => "repro",
            Self::Day => "day",
            Self::Veto => "veto",
            Self::Apply => "apply",
            Self::Freeze => "freeze",
            Self::Import => "import",
        }
    }
}

/// How a line of what happened is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
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
        Regime::R8 => "light core beside heavy neighbours",
        Regime::R9 => "every thread, alternating with one thread",
    }
}

/// What a regime loads, short, as it stands in a heading; R6 is "idle + bursts" here.
pub const fn load_words(regime: Regime) -> &'static str {
    match regime {
        Regime::R6 => "idle + bursts",
        Regime::R1
        | Regime::R2
        | Regime::R3
        | Regime::R4
        | Regime::R5
        | Regime::R7
        | Regime::R8
        | Regime::R9 => regime_words(regime),
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
        Regime::R8 => "light core + heavy neighbours",
        Regime::R9 => "all threads",
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

/// The one core of `cores`, if it holds exactly one.
fn only(cores: CoreSet) -> Option<CoreId> {
    cores.first().filter(|_| cores.len() == 1)
}

/// Cores named as a trial's load: "core 03", "all 8 cores" when they are every core, else the list.
pub fn cores_text(cores: CoreSet, total: usize) -> String {
    match only(cores) {
        Some(core) => format!("core {:02}", core.get()),
        None if cores.len() == total && total > 1 => format!("all {total} cores"),
        None => core_list(cores),
    }
}

/// Where a load runs: "on core 03", "on 00-07".
pub fn on_cores(cores: CoreSet) -> String {
    match only(cores) {
        Some(core) => format!("on core {:02}", core.get()),
        None => format!("on {}", core_ids(cores)),
    }
}

/// Cores named with the word: "core 03", "cores 00-02 05".
pub fn cores_named(cores: CoreSet) -> String {
    match only(cores) {
        Some(core) => format!("core {:02}", core.get()),
        None => format!("cores {}", core_ids(cores)),
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
        || format!("logical CPU {}", d.cpu.get()),
        |core| format!("core {:02}", core.get()),
    );
    Some(Line {
        tag: Tag::Mce,
        text: format!("{who} {kind} error between trials (recorded only)"),
        tone: Tone::Warn,
    })
}

fn line(tag: Tag, tone: Tone, text: String) -> Line {
    Line { tag, text, tone }
}

fn offset_text(offset: togi_machine::TargetOffset) -> String {
    offset.get().to_string()
}

/// A CCD vector as "CCD0 -10, CCD1 -5".
pub fn ccd_vector(profile: &CcdVector) -> String {
    profile
        .ccd()
        .iter()
        .map(|(ccd, at)| format!("CCD{ccd} {}", offset_text(at)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn ccd_names(ccds: &[togi_machine::CcdId]) -> String {
    ccds.iter()
        .map(|ccd| format!("CCD{ccd}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn observed(profile: &ObservedProfile) -> String {
    match profile {
        ObservedProfile::Ccd { ccd, .. } => ccd
            .iter()
            .map(|(ccd, at)| format!("CCD{ccd} {}", offset_text(at)))
            .collect::<Vec<_>>()
            .join(", "),
        ObservedProfile::PerCore { cores } => {
            let parts: Vec<String> = cores
                .iter()
                .map(|(core, at)| format!("core {:02}: {}", core.get(), at.0))
                .collect();
            format!("per-core [{}]", parts.join(", "))
        }
    }
}

fn event_id(id: &DayEventId) -> String {
    match id {
        DayEventId::Boot { boot } => {
            let boot = boot.as_str();
            format!("boot {}", boot.get(..8).unwrap_or(boot))
        }
        DayEventId::Record { session, seq } => format!("record {session}#{seq}"),
        DayEventId::Requalification { night } => format!("re-qualification of night {night}"),
    }
}

/// An edge U as the dashboard says it: "unknown", "fails at -23", "censored at -50".
pub fn edge_words(edge: &Edge) -> String {
    match edge {
        Edge::Unknown {} => "unknown".to_owned(),
        Edge::Observed { offset, .. } => format!("fails at {}", offset.get()),
        Edge::Censored { limit, .. } => format!("censored at {}", limit.get()),
    }
}

fn edge_line(d: &CcdEdge) -> Line {
    let tone = match d.edge {
        Edge::Observed { .. } => Tone::Warn,
        Edge::Unknown {} | Edge::Censored { .. } => Tone::Plain,
    };
    line(
        Tag::Edge,
        tone,
        format!(
            "CCD{} edge {} → {}",
            d.ccd,
            edge_words(&d.old),
            edge_words(&d.edge)
        ),
    )
}

/// How a cap's slope reads: "fallback slope" or "measured slope".
pub const fn slope_words(slope: &Slope) -> &'static str {
    match slope {
        Slope::Fallback { .. } => "fallback slope",
        Slope::Measured { .. } => "measured slope",
    }
}

/// What fixed the historical statistical diagnostic, not permission for D.
pub fn binding_words(binding: &Binding) -> String {
    match binding {
        Binding::Rate { class } => format!("rate of {} {}", class.regime, kind_words(class.regime)),
        Binding::EdgeMargin {} => "edge margin".to_owned(),
        Binding::Baseline {} => "baseline diagnostic".to_owned(),
    }
}

fn cap_line(d: &CcdCap) -> Line {
    line(
        Tag::Cap,
        Tone::Plain,
        format!(
            "CCD{} S {} → {} · diagnostic only, not permission · {} · {}",
            d.ccd,
            offset_text(d.old),
            offset_text(d.new),
            binding_words(&d.binding),
            slope_words(&d.slope)
        ),
    )
}

/// A night plan: the opening one, or a replan with the time the night has observed so far.
fn plan_line(d: &NightPlan) -> Line {
    let tasks = plural(d.tasks.len(), "task");
    let text = if d.night.is_none() {
        format!("night opened · {tasks}")
    } else {
        let elapsed: u64 = d.budgets.iter().map(|budget| budget.consumed_s).sum();
        format!(
            "night replanned · {} elapsed · {tasks}",
            age(Duration::from_secs(elapsed))
        )
    };
    line(Tag::Night, Tone::Plain, text)
}

/// A qualification's purpose, in words.
pub const fn purpose_words(purpose: QualificationPurpose) -> &'static str {
    match purpose {
        QualificationPurpose::NextStep => "next step",
        QualificationPurpose::Requalification => "re-qualification",
        QualificationPurpose::Firmware => "firmware",
    }
}

fn qualification_line(d: &NightQualification) -> Option<Line> {
    let of = format!("{} ({})", ccd_vector(&d.profile), purpose_words(d.purpose));
    match &d.state {
        QualificationState::Start { outstanding } => Some(line(
            Tag::Qualify,
            Tone::Plain,
            format!(
                "qualifying {of} · {} to witness",
                plural(outstanding.len(), "cell")
            ),
        )),
        QualificationState::Progress { .. } => None,
        QualificationState::Complete { witnesses } => Some(line(
            Tag::Qualify,
            Tone::Good,
            format!(
                "qualified {of} · {} witnessed",
                plural(witnesses.len(), "cell")
            ),
        )),
        QualificationState::Failed { outstanding, .. } => Some(line(
            Tag::Qualify,
            Tone::Bad,
            format!(
                "qualification of {of} failed · {} outstanding",
                plural(outstanding.len(), "cell")
            ),
        )),
    }
}

fn reproduction_line(d: &NightReproduction) -> Option<Line> {
    let target = match &d.target {
        Some(ReproductionTarget::Exact { profile }) => {
            format!("exact profile {}", ccd_vector(profile))
        }
        Some(ReproductionTarget::Isolated { ccd, offset }) => {
            format!("CCD{ccd} alone at {}", offset_text(*offset))
        }
        None => "no profile a night writes".to_owned(),
    };
    let source = event_id(&d.source);
    match d.state {
        ReproductionState::Pending => None,
        ReproductionState::Reproduced => Some(line(
            Tag::Repro,
            Tone::Bad,
            format!("{source} reproduced on {target}"),
        )),
        ReproductionState::Unreproduced => Some(line(
            Tag::Repro,
            Tone::Good,
            format!("{source} not reproduced on {target}"),
        )),
        ReproductionState::Untestable => Some(line(
            Tag::Repro,
            Tone::Warn,
            format!("{source} cannot be retested: {target}"),
        )),
    }
}

fn event_line(d: &DayEvent) -> Line {
    let (kind, tone) = match d.classification {
        Classification::CpuSuspect {
            certainty: Certainty::Definite,
        } => ("CPU-suspect", Tone::Bad),
        Classification::CpuSuspect {
            certainty: Certainty::Provisional,
        } => ("provisionally CPU-suspect", Tone::Bad),
        Classification::NonCpu {} => ("not CPU", Tone::Plain),
        Classification::Unexplained {} => ("unexplained", Tone::Warn),
    };
    let resolution = match d.resolution {
        Resolution::Unresolved {} => "unresolved",
        Resolution::Resolved { .. } => "resolved",
    };
    line(
        Tag::Day,
        tone,
        format!(
            "{} at {} · {kind}, {resolution}",
            event_id(&d.event),
            observed(&d.profile)
        ),
    )
}

fn veto_line(d: &DayVeto) -> Line {
    match d.state {
        VetoState::Active => line(
            Tag::Veto,
            Tone::Warn,
            format!(
                "veto on {} from {}",
                observed(&d.profile),
                event_id(&d.event)
            ),
        ),
        VetoState::Cleared => line(
            Tag::Veto,
            Tone::Plain,
            format!(
                "veto on {} cleared, from {}",
                observed(&d.profile),
                event_id(&d.event)
            ),
        ),
    }
}

fn step_line(d: &DayStep) -> Option<Line> {
    match &d.step {
        StepKind::Hold { .. } => None,
        StepKind::Promote { changed } => Some(line(
            Tag::Step,
            Tone::Good,
            format!("promote CCD{changed} to {}", ccd_vector(&d.proposed)),
        )),
        StepKind::Retreat { changed, .. } => Some(line(
            Tag::Step,
            Tone::Warn,
            format!(
                "retreat {} to {}",
                ccd_names(changed),
                ccd_vector(&d.proposed)
            ),
        )),
        StepKind::FirmwareZero { changed } => Some(line(
            Tag::Step,
            Tone::Warn,
            format!(
                "firmware changed: {} to {}",
                ccd_names(changed),
                ccd_vector(&d.proposed)
            ),
        )),
    }
}

fn applied_line(d: &DayApplied) -> Line {
    line(
        Tag::Apply,
        Tone::Plain,
        format!("applied day profile {}", ccd_vector(&d.target)),
    )
}

fn hold_line(d: &DayHold) -> Option<Line> {
    let what = match d.change {
        HoldChange::Credited {} => return None,
        HoldChange::Reset { .. } => "reset",
        HoldChange::Recomputed { .. } => "recomputed",
    };
    Some(line(
        Tag::Hold,
        Tone::Plain,
        format!(
            "hold at {} {what} · {} eligible",
            ccd_vector(&d.profile),
            age(Duration::from_secs(d.eligible_s))
        ),
    ))
}

fn freeze_line(d: &DayFreeze) -> Line {
    let what = match d.choice {
        FreezeChoice::Freeze => "frozen",
        FreezeChoice::Unfreeze => "unfrozen",
    };
    line(Tag::Freeze, Tone::Plain, format!("promotion {what}"))
}

fn import_line(d: &SessionImported) -> Line {
    line(
        Tag::Import,
        Tone::Plain,
        format!(
            "imported {} of {} selected records from {}",
            d.counts.imported,
            d.counts.selected,
            plural(d.sources.len(), "session")
        ),
    )
}

/// How a kind of event reads in the history.
#[derive(Clone, Copy, Debug)]
pub enum History {
    /// The kind never has a history line.
    Silent,
    /// The line one event of this kind words by itself; `None` when this event has none.
    Line(fn(&Payload) -> Option<Line>),
    /// The message the journal recorded for the event, as the log shows it, under one tag and tone.
    Recorded(Tag, Tone),
    /// The line depends on earlier events, or changes the lines they began; the history builds it.
    Contextual(Context),
}

/// The events whose history line needs what came before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    /// The session start.
    Start,
    /// A configuration load, which is a resume after the first.
    Config,
    /// A trial end, which merges passes and replaces the reboot it reports.
    TrialEnd,
    /// A crash, which replaces a resume line.
    Crash,
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

/// Everything one kind of journal event says.
#[derive(Clone, Copy, Debug)]
pub struct Words {
    /// Whether the log view lists events of this kind.
    pub listed: bool,
    /// Its history line.
    pub history: History,
    /// Its tag in the log.
    pub tag: Tagging,
}

impl Words {
    /// A kind with no words of its own: listed in the log under its kind name, and never in the history.
    pub const SILENT: Self = Self {
        listed: true,
        history: History::Silent,
        tag: Tagging::Kind,
    };

    /// A kind the log does not list and that has no words.
    pub const UNLISTED: Self = Self {
        listed: false,
        ..Self::SILENT
    };

    /// A kind whose history line one event words by itself, tagged in the log as its line is.
    const fn said(tag: Tag, line: fn(&Payload) -> Option<Line>) -> Self {
        Self {
            history: History::Line(line),
            tag: Tagging::Fixed(tag),
            ..Self::SILENT
        }
    }

    /// A kind of an earlier ruleset: its recorded message is its history line, and the log names its kind.
    const HISTORICAL: Self = Self {
        history: History::Recorded(Tag::Note, Tone::Plain),
        ..Self::SILENT
    };
}

macro_rules! of {
    ($variant:ident, |$d:ident| $body:expr) => {
        |payload: &Payload| {
            if let Payload::$variant($d) = payload {
                $body
            } else {
                None
            }
        }
    };
}

/// The words of a kind of journal event.
pub fn words(kind: EventKind) -> Words {
    match kind {
        EventKind::SessionStart => Words {
            history: History::Contextual(Context::Start),
            tag: Tagging::Fixed(Tag::Start),
            ..Words::SILENT
        },
        EventKind::SessionContext
        | EventKind::SessionBaseline
        | EventKind::SessionNotice
        | EventKind::SessionArchived
        | EventKind::SmuError
        | EventKind::ProfileApplied
        | EventKind::ProfileRestored
        | EventKind::TrialStart
        | EventKind::TrialProgress
        | EventKind::TrialSignal
        | EventKind::TrialSample
        | EventKind::TrialIntent
        | EventKind::TaskStart
        | EventKind::TunerWarning
        | EventKind::DefectAnswered
        | EventKind::BootSavedEntry
        | EventKind::BootLeaveReason
        | EventKind::JournalTorn
        | EventKind::DayBootStart
        | EventKind::DayCheckpoint
        | EventKind::DayOffsetsChanged
        | EventKind::DayCleanStop
        | EventKind::DayAnswer
        | EventKind::DayRecord
        | EventKind::DayFault
        | EventKind::DayUncleanEnd
        | EventKind::ExposureObserved
        | EventKind::DayInterval
        | EventKind::DayImported
        | EventKind::StateRebuilt
        | EventKind::ProfileChange
        | EventKind::TrialCarried
        | EventKind::FailureCarried
        | EventKind::SessionCarried
        | EventKind::HostRanking => Words::SILENT,
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
        EventKind::ConfigLoaded => Words {
            history: History::Contextual(Context::Config),
            ..Words::SILENT
        },
        EventKind::TrialEnd => Words {
            history: History::Contextual(Context::TrialEnd),
            tag: Tagging::By(|payload| {
                if let Payload::TrialEnd(d) = payload {
                    trial_end_tag(d)
                } else {
                    Tag::Unclear
                }
            }),
            ..Words::SILENT
        },
        EventKind::Failure => Words::said(Tag::Fail, of!(Failure, |d| failure_line(d))),
        EventKind::Mce => Words {
            history: History::Line(of!(Mce, |d| mce_line(d))),
            ..Words::SILENT
        },
        EventKind::CrashDetected => Words {
            history: History::Contextual(Context::Crash),
            tag: Tagging::Fixed(Tag::Crash),
            ..Words::SILENT
        },
        EventKind::TunerDecision
        | EventKind::CorePhase
        | EventKind::CheckingCycle
        | EventKind::CheckingStep
        | EventKind::CheckingChain
        | EventKind::HuntStart
        | EventKind::HuntGroup
        | EventKind::HuntEnd
        | EventKind::HuntSkipped
        | EventKind::Combination
        | EventKind::DeepeningRound => Words::HISTORICAL,
        EventKind::CommandReset => Words {
            history: History::Line(|payload| {
                Some(line(Tag::Reset, Tone::Plain, payload.message()))
            }),
            ..Words::SILENT
        },
        EventKind::UnitMapping | EventKind::CampaignStart | EventKind::SuccessorImported => Words {
            history: History::Recorded(Tag::Note, Tone::Plain),
            ..Words::SILENT
        },
        EventKind::UnitEdge => Words {
            history: History::Recorded(Tag::Edge, Tone::Warn),
            tag: Tagging::Fixed(Tag::Edge),
            ..Words::SILENT
        },
        EventKind::ClimbDecision => Words {
            history: History::Recorded(Tag::Step, Tone::Warn),
            tag: Tagging::Fixed(Tag::Step),
            ..Words::SILENT
        },
        EventKind::UnitStop => Words {
            history: History::Recorded(Tag::Stop, Tone::Warn),
            tag: Tagging::Fixed(Tag::Stop),
            ..Words::SILENT
        },
        EventKind::CampaignCharge | EventKind::CampaignReclassified => Words {
            history: History::Recorded(Tag::Day, Tone::Warn),
            tag: Tagging::Fixed(Tag::Day),
            ..Words::SILENT
        },
        EventKind::FirmwareRestoration => Words {
            history: History::Recorded(Tag::Step, Tone::Warn),
            tag: Tagging::Fixed(Tag::Step),
            ..Words::SILENT
        },
        EventKind::CcdEdge => Words::said(Tag::Edge, of!(CcdEdge, |d| Some(edge_line(d)))),
        EventKind::CcdCap => Words::said(Tag::Cap, of!(CcdCap, |d| Some(cap_line(d)))),
        EventKind::NightPlan => Words::said(Tag::Night, of!(NightPlan, |d| Some(plan_line(d)))),
        EventKind::NightQualification => Words::said(
            Tag::Qualify,
            of!(NightQualification, |d| qualification_line(d)),
        ),
        EventKind::NightReproduction => {
            Words::said(Tag::Repro, of!(NightReproduction, |d| reproduction_line(d)))
        }
        EventKind::DayEvent => Words::said(Tag::Day, of!(DayEvent, |d| Some(event_line(d)))),
        EventKind::DayVeto => Words::said(Tag::Veto, of!(DayVeto, |d| Some(veto_line(d)))),
        EventKind::DayStep => Words::said(Tag::Step, of!(DayStep, |d| step_line(d))),
        EventKind::DayApplied => {
            Words::said(Tag::Apply, of!(DayApplied, |d| Some(applied_line(d))))
        }
        EventKind::DayHold => Words::said(Tag::Hold, of!(DayHold, |d| hold_line(d))),
        EventKind::DayFreeze => Words::said(Tag::Freeze, of!(DayFreeze, |d| Some(freeze_line(d)))),
        EventKind::DayPause => Words::said(Tag::Day, |payload| {
            if let Payload::DayPause(_) = payload {
                Some(line(Tag::Day, Tone::Plain, payload.message()))
            } else {
                None
            }
        }),
        EventKind::StockAttested => Words::said(Tag::Note, |payload| {
            if let Payload::StockAttested(_) = payload {
                Some(line(Tag::Note, Tone::Plain, payload.message()))
            } else {
                None
            }
        }),
        EventKind::SessionImported => {
            Words::said(Tag::Import, of!(SessionImported, |d| Some(import_line(d))))
        }
        EventKind::DeadEnd => Words {
            history: History::Line(|payload| {
                if let Payload::DeadEnd(d) = payload {
                    Some(Line {
                        tag: Tag::Stop,
                        text: format!("dead end: {}", d.detail),
                        tone: Tone::Bad,
                    })
                } else {
                    None
                }
            }),
            tag: Tagging::Fixed(Tag::Stop),
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
