//! The R7 request telemetry report: trials counted by their top voltage request and by their top requester.

use std::collections::{BTreeMap, HashSet};

use jiff::Timestamp;
use togi_journal::{Outcome, Payload};
use togi_machine::{CoreSet, PerCore, Regime};
use togi_requests::top;

use crate::project::{Class, Projection};
use crate::report::selected;
use crate::table::{Table, row};

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

pub(crate) fn checked_bin(top: f64) -> Result<i64, crate::Error> {
    let bin = (top * BINS_PER_VOLT + BIN_ROUNDING_TOLERANCE).floor();
    if !bin.is_finite() || !(-(2.0_f64.powi(63))..2.0_f64.powi(63)).contains(&bin) {
        return Err(crate::Error::VoltageBin(top));
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the finite integral bin was checked against the exact signed 64-bit bounds"
    )]
    Ok(bin as i64)
}

fn bin(top: f64) -> i64 {
    match checked_bin(top) {
        Ok(bin) => bin,
        Err(_) => unreachable!("Journal construction validates recorded voltage bins"),
    }
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
