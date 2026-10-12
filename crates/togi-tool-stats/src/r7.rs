use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

use jiff::Timestamp;
use togi_journal::{CheckingChain, Decision, Event, Outcome, Payload, Seq, TrialId, TunerDecision};
use togi_machine::{CoreSet, PerCore, Regime};
use togi_render::escape_text;
use togi_requests::top;

use crate::evidence::event_at;
use crate::project::{Class, Projection, core_list};
use crate::report::selected;
use crate::table::{Table, row};

/// A voltage-targeted backoff of a multi-core R7 failure; `steps` is how many offset counts it moved.
pub(crate) struct Backoff<'a> {
    pub(crate) seq: Seq,
    pub(crate) decision: &'a TunerDecision,
    pub(crate) steps: i64,
    pub(crate) causes: &'a [Seq],
}

pub(crate) struct Chain<'a> {
    pub(crate) seq: Seq,
    pub(crate) chain: &'a CheckingChain,
}

#[derive(Default)]
pub(crate) struct R7Decisions<'a> {
    pub(crate) backoffs: Vec<Backoff<'a>>,
    pub(crate) chains: Vec<Chain<'a>>,
}

pub(crate) fn decisions(events: &[Event], since: Option<Timestamp>) -> R7Decisions<'_> {
    let mut intents: HashMap<&TrialId, (Regime, CoreSet)> = HashMap::new();
    for event in events {
        if let Payload::TrialIntent(intent) = &event.payload {
            intents.insert(&intent.trial, (intent.regime, intent.cores));
        }
    }
    let multi_core_r7 = |trial: &TrialId| {
        intents
            .get(trial)
            .is_some_and(|&(regime, cores)| regime == Regime::R7 && cores.len() > 1)
    };
    let mut r = R7Decisions::default();
    for event in events.iter().filter(|e| selected(e.time, since)) {
        match &event.payload {
            Payload::TunerDecision(d) if d.decision == Decision::Backoff => {
                let r7 = event
                    .cause
                    .iter()
                    .filter_map(|&seq| event_at(events, seq))
                    .any(|source| match &source.payload {
                        Payload::Failure(f) => f.trial.as_ref().is_some_and(multi_core_r7),
                        Payload::TrialCarried(c) => {
                            c.class.regime == Regime::R7 && c.class.cores.len() > 1
                        }
                        Payload::TrialEnd(end) => multi_core_r7(&end.trial),
                        _ => false,
                    });
                if r7 {
                    r.backoffs.push(Backoff {
                        seq: event.seq,
                        decision: d,
                        steps: d.to_offset.get() - d.from_offset.get(),
                        causes: &event.cause,
                    });
                }
            }
            Payload::CheckingChain(chain) => r.chains.push(Chain {
                seq: event.seq,
                chain,
            }),
            _ => {}
        }
    }
    r
}

fn seq_list(seqs: &[Seq]) -> String {
    let mut list = String::new();
    for (i, seq) in seqs.iter().enumerate() {
        if i > 0 {
            list.push(',');
        }
        let _ = write!(list, "{seq}");
    }
    list
}

/// The cores of each request group, highest request first: `00 > 01,02`.
fn group_list(groups: &[CoreSet]) -> String {
    groups
        .iter()
        .map(|&group| core_list(group))
        .collect::<Vec<_>>()
        .join(" > ")
}

pub(crate) fn render_decisions(table: &mut Table, r: &R7Decisions<'_>) {
    table.section(
        "R7 voltage-targeted backoffs",
        &[
            "seq", "decision", "core", "from", "to", "counts", "causes", "reason",
        ],
    );
    for b in &r.backoffs {
        let d = b.decision;
        row!(
            table,
            b.seq,
            d.decision,
            format!("{:02}", d.core.get()),
            d.from_offset.get(),
            d.to_offset.get(),
            b.steps,
            seq_list(b.causes),
            escape_text(&d.reason)
        );
    }
    table.section(
        "R7 chain derivations",
        &[
            "seq",
            "cycle",
            "step",
            "CCD",
            "workload",
            "part",
            "request groups",
            "loaded cores",
            "sources",
        ],
    );
    for c in &r.chains {
        let d = c.chain;
        let sources = if d.source_seqs.is_empty() {
            "offset fallback".to_owned()
        } else {
            seq_list(&d.source_seqs)
        };
        row!(
            table,
            c.seq,
            d.cycle,
            d.step,
            d.ccd,
            d.workload,
            d.part,
            group_list(&d.groups),
            core_list(d.cores),
            sources
        );
    }
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
    let own = p.events.iter().find_map(|event| match &event.payload {
        Payload::SessionStart(start) => Some(start.session),
        _ => None,
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
