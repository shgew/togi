//! The sanity check of the Rust bench against the scores the Go bench recorded.
//!
//! The two benches draw their simulated machines and failures from different random streams, so a seed's run
//! differs between them and no per-seed equality is expected. What must agree is the distribution of each
//! metric over a scenario's seeds. For every scenario, and for all runs together, each metric's mean over the
//! runs both sides have is compared. A metric is flagged only when the means differ by more than its [`Band`]
//! and by more than [`Z`] standard errors (Welch's two-sample statistic): a large difference that the seeds'
//! own spread does not explain.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

use serde::Deserialize;

use crate::error::Error;
use crate::record::{Record, Status};
use crate::stats::float;

/// A mean difference is flagged only beyond this many standard errors.
pub const Z: f64 = 3.0;

/// The size of a difference worth flagging: beyond `abs` in the metric's own unit and beyond `rel` of Go's mean.
#[derive(Clone, Copy, Debug)]
pub struct Band {
    pub rel: f64,
    pub abs: f64,
}

/// A run of the Go bench, as its baseline file records it. Fields the Rust bench does not compare, and fields
/// Go retired, are ignored.
#[derive(Clone, Debug, Deserialize)]
pub struct GoRun {
    scenario: String,
    seed: u64,
    #[serde(default)]
    status: String,
    #[serde(default)]
    sim_hours: f64,
    first_passed_cycle_h: Option<f64>,
    #[serde(default)]
    passed_cycles: u32,
    #[serde(default)]
    crashes: u32,
    #[serde(default)]
    trials: u32,
    #[serde(default)]
    real_answers: u32,
    #[serde(default)]
    trial_hours: f64,
    #[serde(default)]
    hunts: u32,
    #[serde(default)]
    combinations: u32,
    #[serde(default)]
    depth: i64,
    #[serde(default)]
    hazard_max_per_h: f64,
    worst_r7_hazard_per_h: Option<f64>,
}

/// Reads the Go bench's records, one JSON object per line.
pub fn read_go(path: &Path) -> Result<Vec<GoRun>, Error> {
    let text = fs::read_to_string(path).map_err(|source| Error::ReadFile {
        what: "read Go scores",
        path: path.to_path_buf(),
        source,
    })?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|source| Error::Decode {
                what: "Go scores",
                path: path.to_path_buf(),
                line: index + 1,
                source,
            })
        })
        .collect()
}

/// One run's metrics, whichever bench made it.
#[derive(Clone, Debug)]
struct Row {
    scenario: String,
    seed: u64,
    concluded: f64,
    sim_hours: f64,
    first_passed_cycle_h: Option<f64>,
    passed_cycles: f64,
    crashes: f64,
    trials: f64,
    real_answer_share: f64,
    trial_hours: f64,
    hunts: f64,
    combinations: f64,
    depth: f64,
    hazard_max_per_h: f64,
    worst_r7_hazard_per_h: Option<f64>,
}

fn answer_share(real: u32, trials: u32) -> f64 {
    crate::stats::share(real, trials)
}

#[expect(clippy::cast_precision_loss, reason = "depths are small integers")]
fn depth(depth: i64) -> f64 {
    depth as f64
}

impl From<&Record> for Row {
    fn from(r: &Record) -> Self {
        Self {
            scenario: r.scenario.clone(),
            seed: r.seed,
            concluded: f64::from(u8::from(r.status == Status::Concluded)),
            sim_hours: r.sim_hours,
            first_passed_cycle_h: r.first_passed_cycle_h,
            passed_cycles: f64::from(r.passed_cycles),
            crashes: f64::from(r.crashes),
            trials: f64::from(r.trials),
            real_answer_share: answer_share(r.real_answers, r.trials),
            trial_hours: r.trial_hours,
            hunts: f64::from(r.hunts),
            combinations: f64::from(r.combinations),
            depth: depth(r.depth),
            hazard_max_per_h: r.hazard_max_per_h,
            worst_r7_hazard_per_h: r.worst_r7_hazard_per_h,
        }
    }
}

impl From<&GoRun> for Row {
    fn from(r: &GoRun) -> Self {
        Self {
            scenario: r.scenario.clone(),
            seed: r.seed,
            concluded: f64::from(u8::from(r.status == "concluded")),
            sim_hours: r.sim_hours,
            first_passed_cycle_h: r.first_passed_cycle_h,
            passed_cycles: f64::from(r.passed_cycles),
            crashes: f64::from(r.crashes),
            trials: f64::from(r.trials),
            real_answer_share: answer_share(r.real_answers, r.trials),
            trial_hours: r.trial_hours,
            hunts: f64::from(r.hunts),
            combinations: f64::from(r.combinations),
            depth: depth(r.depth),
            hazard_max_per_h: r.hazard_max_per_h,
            worst_r7_hazard_per_h: r.worst_r7_hazard_per_h,
        }
    }
}

struct Metric {
    name: &'static str,
    band: Band,
    value: fn(&Row) -> Option<f64>,
}

/// The metrics compared and the band of each. A band's `rel` is the share of Go's mean a difference must exceed
/// and `abs` the size in the metric's unit it must exceed; counts of rare events have wide bands, quantities
/// every run pins down narrow ones.
const METRICS: [Metric; 13] = [
    Metric {
        name: "concluded",
        band: Band { rel: 0.0, abs: 0.05 },
        value: |r| Some(r.concluded),
    },
    Metric {
        name: "sim_hours",
        band: Band { rel: 0.15, abs: 1.0 },
        value: |r| Some(r.sim_hours),
    },
    Metric {
        name: "first_passed_cycle_h",
        band: Band { rel: 0.15, abs: 1.0 },
        value: |r| r.first_passed_cycle_h,
    },
    Metric {
        name: "passed_cycles",
        band: Band { rel: 0.15, abs: 0.2 },
        value: |r| Some(r.passed_cycles),
    },
    Metric {
        name: "crashes",
        band: Band { rel: 0.25, abs: 2.0 },
        value: |r| Some(r.crashes),
    },
    Metric {
        name: "trials",
        band: Band { rel: 0.15, abs: 20.0 },
        value: |r| Some(r.trials),
    },
    Metric {
        name: "trial_hours",
        band: Band { rel: 0.15, abs: 1.0 },
        value: |r| Some(r.trial_hours),
    },
    Metric {
        name: "hunts",
        band: Band { rel: 0.30, abs: 1.0 },
        value: |r| Some(r.hunts),
    },
    Metric {
        name: "combinations",
        band: Band { rel: 0.50, abs: 0.5 },
        value: |r| Some(r.combinations),
    },
    Metric {
        name: "depth",
        band: Band { rel: 0.08, abs: 5.0 },
        value: |r| Some(r.depth),
    },
    Metric {
        name: "real_answer_share",
        band: Band { rel: 0.0, abs: 0.05 },
        value: |r| Some(r.real_answer_share),
    },
    Metric {
        name: "hazard_max_per_h",
        band: Band { rel: 0.25, abs: 0.02 },
        value: |r| Some(r.hazard_max_per_h),
    },
    Metric {
        name: "worst_r7_hazard_per_h",
        band: Band { rel: 0.25, abs: 0.2 },
        value: |r| r.worst_r7_hazard_per_h,
    },
];

/// One metric of one group of runs, Rust against Go.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub scenario: String,
    pub metric: &'static str,
    pub rust: f64,
    pub go: f64,
    /// Runs each side measured the metric on.
    pub rust_runs: usize,
    pub go_runs: usize,
    /// Welch's statistic of the difference of means; infinite when both sides are constant and differ.
    pub z: f64,
    pub flagged: bool,
}

/// The sanity table: for each scenario in the order the Rust records name them, then for all runs, each metric's
/// means over the runs of the scenario that both benches ran. A scenario or metric one side lacks is left out.
pub fn compare(rust: &[Record], go: &[GoRun]) -> Vec<Line> {
    let rust: Vec<Row> = rust.iter().map(Row::from).collect();
    let go: Vec<Row> = go.iter().map(Row::from).collect();
    let shared: BTreeSet<(&str, u64)> = {
        let go_keys: BTreeSet<_> = go.iter().map(|r| (r.scenario.as_str(), r.seed)).collect();
        rust.iter()
            .map(|r| (r.scenario.as_str(), r.seed))
            .filter(|key| go_keys.contains(key))
            .collect()
    };
    let in_both = |rows: &[Row]| -> Vec<Row> {
        rows.iter()
            .filter(|r| shared.contains(&(r.scenario.as_str(), r.seed)))
            .cloned()
            .collect()
    };
    let (rust, go) = (in_both(&rust), in_both(&go));
    let mut scenarios: Vec<&str> = Vec::new();
    for row in &rust {
        if !scenarios.contains(&row.scenario.as_str()) {
            scenarios.push(&row.scenario);
        }
    }
    let mut lines = Vec::new();
    for scenario in scenarios.iter().copied().map(Some).chain([None]) {
        let pick = |rows: &[Row]| -> Vec<Row> {
            rows.iter()
                .filter(|r| scenario.is_none_or(|name| r.scenario == name))
                .cloned()
                .collect()
        };
        let (rust, go) = (pick(&rust), pick(&go));
        for metric in &METRICS {
            let rust_values: Vec<f64> = rust.iter().filter_map(metric.value).collect();
            let go_values: Vec<f64> = go.iter().filter_map(metric.value).collect();
            if rust_values.is_empty() || go_values.is_empty() {
                continue;
            }
            lines.push(line(
                scenario.unwrap_or("TOTAL"),
                metric,
                &rust_values,
                &go_values,
            ));
        }
    }
    lines
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / float(values.len())
}

fn variance(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let m = mean(values);
    values.iter().map(|v| (v - m).powi(2)).sum::<f64>() / float(values.len() - 1)
}

fn line(scenario: &str, metric: &Metric, rust: &[f64], go: &[f64]) -> Line {
    let (rust_mean, go_mean) = (mean(rust), mean(go));
    let diff = rust_mean - go_mean;
    let error = (variance(rust) / float(rust.len()) + variance(go) / float(go.len())).sqrt();
    let z = if error > 0.0 {
        diff / error
    } else if diff == 0.0 {
        0.0
    } else {
        f64::INFINITY.copysign(diff)
    };
    let Band { rel, abs } = metric.band;
    let flagged = diff.abs() > abs && diff.abs() > rel * go_mean.abs() && z.abs() > Z;
    Line {
        scenario: scenario.to_owned(),
        metric: metric.name,
        rust: rust_mean,
        go: go_mean,
        rust_runs: rust.len(),
        go_runs: go.len(),
        z,
        flagged,
    }
}

/// Writes the table, one line per scenario and metric, flagged lines marked, and the count flagged.
pub fn report(w: &mut impl io::Write, lines: &[Line]) -> io::Result<()> {
    writeln!(
        w,
        "sanity: means over the runs both benches ran; flagged when the means differ by more than the band and by more than {Z} standard errors"
    )?;
    writeln!(
        w,
        "scenario                metric                    runs      rust        go     rel_diff       z  flag"
    )?;
    for l in lines {
        let rel = if l.go == 0.0 {
            if l.rust == 0.0 { 0.0 } else { f64::INFINITY }
        } else {
            (l.rust - l.go) / l.go.abs()
        };
        writeln!(
            w,
            "{:<23} {:<25} {:>4} {:>10.4} {:>10.4} {:>+11.1}% {:>7.2}  {}",
            l.scenario,
            l.metric,
            l.rust_runs,
            l.rust,
            l.go,
            rel * 100.0,
            l.z,
            if l.flagged { "FLAGGED" } else { "" },
        )?;
    }
    writeln!(
        w,
        "sanity: {} of {} comparisons flagged",
        lines.iter().filter(|l| l.flagged).count(),
        lines.len()
    )
}
