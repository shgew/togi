//! The multi-core R7 failures of a journal and the voltage-targeted backoffs each was charged, attributed to
//! the tuner's phase, the failed trial's requirement and the offset counts moved ([#490]).
//!
//! [#490]: https://github.com/shgew/togi/issues/490

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::num::NonZeroU32;

use jiff::Timestamp;
use serde::Serialize;
use togi_journal::{
    Decision, Event, Outcome, Payload, Seq, TrialCarried, TrialId, TrialIntent, TunerDecision,
};
use togi_machine::{CoreId, CoreSet, PerCore, Regime, TargetOffset, WorkloadId};
use togi_render::escape_text;
use togi_requests::top;
use togi_tuner::{Load, PhaseNo, Requirement, State, TrialContext};

use crate::evidence::event_at;
use crate::project::{Class, Projection, core_list};
use crate::report::selected;
use crate::table::{Table, row};

/// Where the tuner stood: in phase 1, in phase 2 (its rounds and its confirmation cycle), or after phase 2
/// concluded. A reset returns a session to phase 1.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub(crate) enum Phase {
    #[serde(rename = "phase_1")]
    One,
    #[serde(rename = "phase_2")]
    Two,
    #[serde(rename = "concluded")]
    Concluded,
}

impl Phase {
    /// The phase of the events folded so far; none before `session.start` or once the fold failed.
    fn of(state: &State) -> Option<Self> {
        state.phase_plan().map(|plan| match plan.phase {
            PhaseNo::One => Self::One,
            PhaseNo::Two => Self::Two,
            PhaseNo::Concluded => Self::Concluded,
        })
    }

    const fn label(phase: Option<Self>) -> &'static str {
        match phase {
            Some(Self::One) => "phase 1",
            Some(Self::Two) => "phase 2",
            Some(Self::Concluded) => "concluded",
            None => "unknown",
        }
    }
}

/// The requirement a failed trial was scheduled for, as the tuner held it once the trial's intent folded.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct Required {
    pub(crate) kind: &'static str,
    /// Passes of the trial's class the requirement needed.
    pub(crate) needed: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) step: Option<NonZeroU32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) part: Option<NonZeroU32>,
}

impl Required {
    fn of(requirement: Requirement) -> Self {
        let (kind, step, part) = match requirement.context {
            TrialContext::Search { .. } => ("search", None, None),
            TrialContext::SoloLimit { .. } => ("solo_limit", None, None),
            TrialContext::Cycle { step, part, .. } => ("cycle", Some(step.get()), Some(part.get())),
            TrialContext::Rerun { .. } => ("rerun", None, None),
            TrialContext::ZeroRerun { .. } => ("zero_rerun", None, None),
            TrialContext::Round { .. } => ("round", None, None),
            TrialContext::Hunt { .. } => ("hunt", None, None),
        };
        Self {
            kind,
            needed: requirement.needed.get(),
            step,
            part,
        }
    }

    /// `cycle 2.3 x3`: the kind, the step and part when it has them, and the passes needed.
    fn label(self) -> String {
        let mut label = self.kind.to_owned();
        if let (Some(step), Some(part)) = (self.step, self.part) {
            let _ = write!(label, " {step}.{part}");
        }
        let _ = write!(label, " x{}", self.needed);
        label
    }
}

/// A voltage-targeted backoff of a multi-core R7 failure.
#[derive(Debug, PartialEq, Serialize)]
pub(crate) struct Backoff<'a> {
    pub(crate) seq: Seq,
    #[serde(skip)]
    pub(crate) decision: &'a TunerDecision,
    #[serde(skip)]
    pub(crate) time: Timestamp,
    /// Hours from the session's first event.
    pub(crate) hours: f64,
    pub(crate) phase: Option<Phase>,
    pub(crate) core: CoreId,
    pub(crate) from: TargetOffset,
    pub(crate) to: TargetOffset,
    /// The offset counts it moved its core.
    pub(crate) counts: i64,
    pub(crate) causes: &'a [Seq],
}

/// Where a failure was observed: a trial of this journal, or a failure carried into it from another session.
#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub(crate) enum Origin<'a> {
    Trial {
        trial: &'a TrialId,
        #[serde(skip_serializing_if = "Option::is_none")]
        cycle: Option<NonZeroU32>,
        /// None when the tuner could not fold the journal up to the trial.
        #[serde(skip_serializing_if = "Option::is_none")]
        requirement: Option<Required>,
    },
    Carried,
}

/// A multi-core R7 failure of this journal or carried into it, with the backoffs charged to it: those whose
/// first multi-core R7 cause it is.
#[derive(Debug, PartialEq, Serialize)]
pub(crate) struct R7Failure<'a> {
    /// Its first event: the trial's `failure` or failing `trial.end`, or the carried `trial.carried`.
    pub(crate) seq: Seq,
    #[serde(skip)]
    pub(crate) time: Timestamp,
    /// Hours from the session's first event.
    pub(crate) hours: f64,
    pub(crate) phase: Option<Phase>,
    #[serde(flatten)]
    pub(crate) origin: Origin<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) workload: Option<WorkloadId>,
    pub(crate) cores: CoreSet,
    pub(crate) duration_s: u32,
    pub(crate) rerun: bool,
    pub(crate) backoffs: Vec<Backoff<'a>>,
}

impl R7Failure<'_> {
    /// The trial, or `carried SEQ`.
    fn name(&self) -> String {
        match &self.origin {
            Origin::Trial { trial, .. } => trial.to_string(),
            Origin::Carried => format!("carried {}", self.seq),
        }
    }

    const fn requirement(&self) -> Option<Required> {
        match &self.origin {
            Origin::Trial { requirement, .. } => *requirement,
            Origin::Carried => None,
        }
    }

    const fn carried(&self) -> bool {
        matches!(self.origin, Origin::Carried)
    }

    fn counts(&self) -> i64 {
        self.backoffs.iter().map(|b| b.counts).sum()
    }
}

#[derive(Default)]
pub(crate) struct R7Decisions<'a> {
    /// Every failure, in journal order of their first event.
    failures: Vec<R7Failure<'a>>,
    since: Option<Timestamp>,
}

impl<'a> R7Decisions<'a> {
    /// The failures whose first event is at or after the cutoff, each with all its backoffs.
    pub(crate) fn failures(&self) -> impl Iterator<Item = &R7Failure<'a>> {
        self.failures
            .iter()
            .filter(|f| selected(f.time, self.since))
    }

    /// The backoffs decided at or after the cutoff, with the failure each is charged to, in journal order.
    pub(crate) fn backoffs(&self) -> Vec<(&R7Failure<'a>, &Backoff<'a>)> {
        let mut backoffs: Vec<_> = self
            .failures
            .iter()
            .flat_map(|f| f.backoffs.iter().map(move |b| (f, b)))
            .filter(|(_, b)| selected(b.time, self.since))
            .collect();
        backoffs.sort_by_key(|(_, b)| b.seq);
        backoffs
    }
}

/// What identifies a failure across its events: its trial, or a carried fact's own event.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
enum Key<'a> {
    Trial(&'a TrialId),
    Carried(Seq),
}

/// A multi-core R7 failure as one of its events observes it.
#[derive(Clone, Copy)]
enum Source<'a> {
    Trial(&'a TrialId, &'a TrialIntent, CoreSet),
    Carried(&'a TrialCarried),
}

struct Attribution<'a> {
    start: Option<Timestamp>,
    intents: HashMap<&'a TrialId, &'a TrialIntent>,
    required: HashMap<&'a TrialId, Required>,
    keys: HashMap<Key<'a>, usize>,
    failures: Vec<R7Failure<'a>>,
}

impl<'a> Attribution<'a> {
    fn hours(&self, time: Timestamp) -> f64 {
        self.start.map_or(0.0, |start| {
            time.duration_since(start).as_secs_f64() / 3600.0
        })
    }

    /// The multi-core R7 failure `event` observes, if any, whatever its outcome.
    fn source(&self, event: &'a Event) -> Option<Source<'a>> {
        let live = |trial: &'a TrialId| {
            let intent = *self.intents.get(trial)?;
            if intent.regime != Regime::R7 {
                return None;
            }
            let cores = Load::recorded(intent.core, intent.cores).cores();
            (cores.len() > 1).then_some(Source::Trial(trial, intent, cores))
        };
        if let Payload::Failure(f) = &event.payload {
            return f.trial.as_ref().and_then(live);
        }
        if let Payload::TrialEnd(end) = &event.payload {
            return live(&end.trial);
        }
        if let Payload::TrialCarried(c) = &event.payload {
            return (c.class.regime == Regime::R7 && c.class.cores.len() > 1)
                .then_some(Source::Carried(c));
        }
        None
    }

    /// The failure `source` observes at `event`, recorded there when this is its first event.
    fn failure(&mut self, source: Source<'a>, event: &'a Event, phase: Option<Phase>) -> usize {
        let key = match source {
            Source::Trial(trial, _, _) => Key::Trial(trial),
            Source::Carried(_) => Key::Carried(event.seq),
        };
        if let Some(&at) = self.keys.get(&key) {
            return at;
        }
        let hours = self.hours(event.time);
        let failure = match source {
            Source::Trial(trial, intent, cores) => R7Failure {
                seq: event.seq,
                time: event.time,
                hours,
                phase,
                origin: Origin::Trial {
                    trial,
                    cycle: intent.cycle,
                    requirement: self.required.get(trial).copied(),
                },
                workload: Some(intent.workload),
                cores,
                duration_s: intent.duration_s,
                rerun: intent.rerun,
                backoffs: Vec::new(),
            },
            Source::Carried(c) => R7Failure {
                seq: event.seq,
                time: event.time,
                hours,
                phase,
                origin: Origin::Carried,
                workload: c.class.workload,
                cores: c.class.cores,
                duration_s: c.class.duration_s,
                rerun: c.rerun,
                backoffs: Vec::new(),
            },
        };
        self.keys.insert(key, self.failures.len());
        self.failures.push(failure);
        self.failures.len() - 1
    }

    /// Charges the backoff at `event` to the failure of its first multi-core R7 cause, if it has one.
    fn backoff(
        &mut self,
        events: &'a [Event],
        event: &'a Event,
        d: &'a TunerDecision,
        phase: Option<Phase>,
    ) {
        let cited = event.cause.iter().find_map(|&seq| {
            let cause = event_at(events, seq)?;
            self.source(cause).map(|source| (source, cause))
        });
        let Some((source, cause)) = cited else {
            return;
        };
        let at = self.failure(source, cause, phase);
        let hours = self.hours(event.time);
        self.failures[at].backoffs.push(Backoff {
            seq: event.seq,
            decision: d,
            time: event.time,
            hours,
            phase,
            core: d.core,
            from: d.from_offset,
            to: d.to_offset,
            counts: d.to_offset.get() - d.from_offset.get(),
            causes: &event.cause,
        });
    }
}

/// Whether the payload records a failed trial: its `failure`, its failing `trial.end`, or a failure carried in.
fn failed(payload: &Payload) -> bool {
    if let Payload::Failure(_) = payload {
        return true;
    }
    if let Payload::TrialEnd(end) = payload {
        return end.outcome == Outcome::Failure;
    }
    if let Payload::TrialCarried(c) = payload {
        return c.outcome == Outcome::Failure;
    }
    false
}

/// Every multi-core R7 failure, each with its backoffs; `since` selects failures by their first event and
/// backoffs by decision time. The tuner folds the whole journal to give each failure and backoff its phase
/// and each live failure its trial's requirement; a journal it cannot fold leaves them unknown from the
/// event it refused on.
pub(crate) fn decisions(events: &[Event], since: Option<Timestamp>) -> R7Decisions<'_> {
    let mut a = Attribution {
        start: events.first().map(|event| event.time),
        intents: HashMap::new(),
        required: HashMap::new(),
        keys: HashMap::new(),
        failures: Vec::new(),
    };
    let mut state = State::new();
    let mut folding = true;
    for event in events {
        if let Payload::TrialIntent(intent) = &event.payload {
            a.intents.insert(&intent.trial, intent);
        } else if let Payload::TunerDecision(d) = &event.payload {
            if d.decision == Decision::Backoff {
                a.backoff(events, event, d, Phase::of(&state));
            }
        } else if failed(&event.payload) {
            if let Some(source) = a.source(event) {
                a.failure(source, event, Phase::of(&state));
            }
        }
        folding = folding && state.fold(event).is_ok();
        if let Payload::TrialIntent(intent) = &event.payload {
            if let Some(requirement) = state.open_requirement() {
                a.required.insert(&intent.trial, Required::of(requirement));
            }
        }
    }
    R7Decisions {
        failures: a.failures,
        since,
    }
}

/// A phase's multi-core R7 failures, by the phase each was first observed in, and its backoffs, by the phase
/// each was decided in.
#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct PhaseTotals {
    pub(crate) failures: u32,
    pub(crate) carried: u32,
    /// Failures charged at least one backoff.
    pub(crate) charged: u32,
    pub(crate) backoffs: u32,
    /// Offset counts the backoffs moved, summed over cores.
    pub(crate) counts: i64,
}

impl PhaseTotals {
    fn add(&mut self, other: &Self) {
        self.failures += other.failures;
        self.carried += other.carried;
        self.charged += other.charged;
        self.backoffs += other.backoffs;
        self.counts += other.counts;
    }
}

pub(crate) fn totals(r: &R7Decisions<'_>) -> BTreeMap<Option<Phase>, PhaseTotals> {
    let mut totals: BTreeMap<Option<Phase>, PhaseTotals> = BTreeMap::new();
    for f in r.failures() {
        let t = totals.entry(f.phase).or_default();
        t.failures += 1;
        t.carried += u32::from(f.carried());
        t.charged += u32::from(!f.backoffs.is_empty());
    }
    for (_, b) in r.backoffs() {
        let t = totals.entry(b.phase).or_default();
        t.backoffs += 1;
        t.counts += b.counts;
    }
    totals
}

/// One JSON object a failure, one a line, in journal order, with its backoffs.
pub(crate) fn records(r: &R7Decisions<'_>) -> Result<String, serde_json::Error> {
    let mut text = String::new();
    for f in r.failures() {
        text.push_str(&serde_json::to_string(f)?);
        text.push('\n');
    }
    Ok(text)
}

pub(crate) fn seq_list(seqs: &[Seq]) -> String {
    let mut list = String::new();
    for (i, seq) in seqs.iter().enumerate() {
        if i > 0 {
            list.push(',');
        }
        let _ = write!(list, "{seq}");
    }
    list
}

pub(crate) fn render_decisions(table: &mut Table, r: &R7Decisions<'_>) {
    table.section(
        "R7 voltage-targeted backoffs",
        &[
            "seq",
            "phase",
            "decision",
            "core",
            "from",
            "to",
            "counts",
            "failure",
            "loaded cores",
            "requirement",
            "causes",
            "reason",
        ],
    );
    for (f, b) in r.backoffs() {
        row!(
            table,
            b.seq,
            Phase::label(b.phase),
            b.decision.decision,
            format!("{:02}", b.core.get()),
            b.from.get(),
            b.to.get(),
            b.counts,
            f.name(),
            core_list(f.cores),
            f.requirement()
                .map_or_else(|| "-".to_owned(), Required::label),
            seq_list(b.causes),
            escape_text(&b.decision.reason)
        );
    }
    table.section(
        "R7 failures and backoffs by phase",
        &[
            "phase", "failures", "carried", "charged", "backoffs", "counts",
        ],
    );
    let totals = totals(r);
    let mut all = PhaseTotals::default();
    let phases = [
        Some(Phase::One),
        Some(Phase::Two),
        Some(Phase::Concluded),
        None,
    ];
    for phase in phases {
        let Some(t) = totals.get(&phase) else {
            continue;
        };
        all.add(t);
        totals_row(table, Phase::label(phase), t);
    }
    totals_row(table, "all", &all);
    table.section("R7 failures by counts charged", &["counts", "failures"]);
    let mut charged: BTreeMap<i64, u32> = BTreeMap::new();
    for f in r.failures() {
        *charged.entry(f.counts()).or_default() += 1;
    }
    for (counts, failures) in charged {
        row!(table, counts, failures);
    }
}

fn totals_row(table: &mut Table, label: &str, t: &PhaseTotals) {
    row!(
        table, label, t.failures, t.carried, t.charged, t.backoffs, t.counts
    );
}

/// One R7 trial's request telemetry, from its end or from a carried fact.
struct Measurement {
    class: Class,
    failed: bool,
    requests: PerCore<f64>,
    top_requesters: CoreSet,
}

/// The measurements of R7 trials that started and ended in this journal, and of other sessions' carried trial
/// facts. A carried fact counts once per source and only with both request fields: the trials of this journal
/// are counted from their own ends, which a fact carried from this very session would repeat.
fn measurements(p: &Projection<'_>, since: Option<Timestamp>) -> Vec<Measurement> {
    let mut out = Vec::new();
    for t in p.trials.iter().filter(|t| {
        t.intent.regime == Regime::R7 && selected(t.time, since) && t.started && t.end.is_some()
    }) {
        if let Some(end) = t.end {
            out.push(Measurement {
                class: t.class,
                failed: end.end.outcome == Outcome::Failure,
                requests: end.end.voltage_requests_v,
                top_requesters: end.end.top_requesters,
            });
        }
    }
    let own = p.events.iter().find_map(|event| {
        if let Payload::SessionStart(start) = &event.payload {
            Some(start.session)
        } else {
            None
        }
    });
    let mut seen = HashSet::new();
    for event in p.events {
        let Payload::TrialCarried(carried) = &event.payload else {
            continue;
        };
        let source = &carried.source;
        if carried.class.regime != Regime::R7
            || carried.voltage_requests_v.is_empty()
            || carried.top_requesters.is_empty()
            || Some(source.session) == own
            || !selected(source.time, since)
            || !seen.insert((source.session, source.seq))
        {
            continue;
        }
        let Some(workload) = carried.class.workload else {
            continue;
        };
        out.push(Measurement {
            class: Class {
                workload,
                cores: carried.class.cores,
                duration_s: carried.class.duration_s,
            },
            failed: carried.outcome == Outcome::Failure,
            requests: carried.voltage_requests_v,
            top_requesters: carried.top_requesters,
        });
    }
    out
}

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct RequestCounts {
    pub(crate) trials: u32,
    pub(crate) failures: u32,
}

impl RequestCounts {
    fn count(&mut self, failed: bool) {
        self.trials += 1;
        self.failures += u32::from(failed);
    }
}

/// A class's top voltage request is binned to 4 mV, rounded down.
const BINS_PER_VOLT: f64 = 250.0;
const BIN_ROUNDING_TOLERANCE: f64 = 1e-9;

#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct RequestMetrics {
    pub(crate) missing_failures: u32,
    /// By class and the bin's number of 4 mV steps.
    pub(crate) bins: BTreeMap<(Class, i64), RequestCounts>,
    /// By class and requester core.
    pub(crate) requesters: BTreeMap<(Class, u8), RequestCounts>,
}

pub(crate) fn requests(p: &Projection<'_>, since: Option<Timestamp>) -> RequestMetrics {
    let mut m = RequestMetrics::default();
    for measurement in measurements(p, since) {
        let Some(top) = top(&measurement.requests) else {
            m.missing_failures += u32::from(measurement.failed);
            continue;
        };
        if measurement.top_requesters.is_empty() {
            m.missing_failures += u32::from(measurement.failed);
            continue;
        }
        let class = measurement.class;
        m.bins
            .entry((class, bin(top)))
            .or_default()
            .count(measurement.failed);
        for core in measurement.top_requesters {
            m.requesters
                .entry((class, core.get()))
                .or_default()
                .count(measurement.failed);
        }
    }
    m
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "a request of a few volts is a few hundred bins"
)]
fn bin(top: f64) -> i64 {
    (top * BINS_PER_VOLT + BIN_ROUNDING_TOLERANCE).floor() as i64
}

#[expect(
    clippy::cast_precision_loss,
    reason = "a bin number of a few hundred is exact as a float"
)]
fn bin_volts(bin: i64) -> f64 {
    bin as f64 / BINS_PER_VOLT
}

pub(crate) fn render_requests(table: &mut Table, m: &RequestMetrics) {
    table.section("R7 voltage requests", &["field", "count"]);
    row!(
        table,
        "failures without request telemetry",
        m.missing_failures
    );
    table.section(
        "R7 trials by top request (4 mV bins)",
        &["class", "top request V", "trials", "failures"],
    );
    if m.bins.is_empty() {
        row!(table, "none");
    }
    for ((class, bin), counts) in &m.bins {
        let low = bin_volts(*bin);
        row!(
            table,
            class.label(),
            format!("[{low:.3}, {:.3})", low + 1.0 / BINS_PER_VOLT),
            counts.trials,
            counts.failures
        );
    }
    table.section(
        "R7 trials by top requester",
        &["class", "core", "trials", "failures"],
    );
    if m.requesters.is_empty() {
        row!(table, "none");
    }
    for ((class, core), counts) in &m.requesters {
        row!(
            table,
            class.label(),
            format!("{core:02}"),
            counts.trials,
            counts.failures
        );
    }
}
