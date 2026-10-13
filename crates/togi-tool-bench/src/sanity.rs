//! The sanity check of a bench run's current records against the scores of a recorded baseline.
//!
//! The recorded baseline and the current records come from the same bench, so on an unchanged tree, suite and
//! inputs a seed's run is the same on both sides. A change to the tuner, the simulator or their inputs can move any
//! seed's run, so no per-seed equality is expected: what must agree is the distribution of each metric over a
//! scenario's seeds. For every scenario, and for all runs together, each metric's mean over the
//! runs both sides have is compared. A metric is flagged only when the means differ by more than its [`Band`]
//! and by more than [`Z`] standard errors (Welch's two-sample statistic, stratified by scenario for all runs
//! together): a large difference that the seeds' own spread does not explain. A group of one run has no spread,
//! so it is never flagged. Three things are flagged whatever the spread: any drop in the share of runs that
//! concluded, a metric only one side measured, and a metric the sides measured on different numbers of runs.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;

use serde::Deserialize;

use crate::error::Error;
use crate::record::{Record, Status};
use crate::stats::float;

/// The metric a run either has or lacks: any drop in the share of runs that concluded is flagged, since a share of
/// zeros and ones has no spread to judge it by.
const CONCLUDED: &str = "concluded";

/// A mean difference is flagged only beyond this many standard errors.
pub const Z: f64 = 3.0;

/// The size of a difference worth flagging: beyond `abs` in the metric's own unit and beyond `rel` of the baseline's mean.
#[derive(Clone, Copy, Debug)]
pub struct Band {
    pub rel: f64,
    pub abs: f64,
}

/// A run as the baseline file records it. Fields the sanity check does not compare, and fields the bench retired,
/// are ignored.
#[derive(Clone, Debug, Deserialize)]
pub struct BaselineRun {
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

/// Reads the baseline's records, one JSON object per line.
pub fn read_baseline(path: &Path) -> Result<Vec<BaselineRun>, Error> {
    let text = fs::read_to_string(path).map_err(|source| Error::ReadFile {
        what: "read baseline scores",
        path: path.to_path_buf(),
        source,
    })?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|source| Error::Decode {
                what: "baseline scores",
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

impl From<&BaselineRun> for Row {
    fn from(r: &BaselineRun) -> Self {
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

/// The metrics compared and the band of each. A band's `rel` is the share of the baseline's mean a difference must exceed
/// and `abs` the size in the metric's unit it must exceed; counts of rare events have wide bands, quantities
/// every run pins down narrow ones.
const METRICS: [Metric; 13] = [
    Metric {
        name: CONCLUDED,
        band: Band { rel: 0.0, abs: 0.0 },
        value: |r| Some(r.concluded),
    },
    Metric {
        name: "sim_hours",
        band: Band {
            rel: 0.05,
            abs: 1.0,
        },
        value: |r| Some(r.sim_hours),
    },
    Metric {
        name: "first_passed_cycle_h",
        band: Band {
            rel: 0.05,
            abs: 1.0,
        },
        value: |r| r.first_passed_cycle_h,
    },
    Metric {
        name: "passed_cycles",
        band: Band {
            rel: 0.15,
            abs: 0.2,
        },
        value: |r| Some(r.passed_cycles),
    },
    Metric {
        name: "crashes",
        band: Band {
            rel: 0.25,
            abs: 2.0,
        },
        value: |r| Some(r.crashes),
    },
    Metric {
        name: "trials",
        band: Band {
            rel: 0.05,
            abs: 20.0,
        },
        value: |r| Some(r.trials),
    },
    Metric {
        name: "trial_hours",
        band: Band {
            rel: 0.05,
            abs: 1.0,
        },
        value: |r| Some(r.trial_hours),
    },
    Metric {
        name: "hunts",
        band: Band {
            rel: 0.30,
            abs: 1.0,
        },
        value: |r| Some(r.hunts),
    },
    Metric {
        name: "combinations",
        band: Band {
            rel: 0.50,
            abs: 0.5,
        },
        value: |r| Some(r.combinations),
    },
    Metric {
        name: "depth",
        band: Band {
            rel: 0.03,
            abs: 5.0,
        },
        value: |r| Some(r.depth),
    },
    Metric {
        name: "real_answer_share",
        band: Band {
            rel: 0.0,
            abs: 0.05,
        },
        value: |r| Some(r.real_answer_share),
    },
    Metric {
        name: "hazard_max_per_h",
        band: Band {
            rel: 0.25,
            abs: 0.02,
        },
        value: |r| Some(r.hazard_max_per_h),
    },
    Metric {
        name: "worst_r7_hazard_per_h",
        band: Band {
            rel: 0.25,
            abs: 0.2,
        },
        value: |r| r.worst_r7_hazard_per_h,
    },
];

/// One metric of one group of runs, the current records against the baseline.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub scenario: String,
    pub metric: &'static str,
    /// Means over the runs each side measured the metric on; NaN for a side that never did.
    pub current: f64,
    pub baseline: f64,
    /// Runs each side measured the metric on.
    pub current_runs: usize,
    pub baseline_runs: usize,
    /// Welch's statistic of the difference of means; infinite when both sides are constant and differ.
    pub z: f64,
    pub flagged: bool,
}

/// The sanity table: for each scenario in the order the current records name them, then for all runs, each metric's
/// means over the runs of the scenario that both the current records and the baseline have. A scenario one side lacks is left out; a metric
/// that only one side measured, or that the sides measured on different numbers of runs, is a flagged line, and
/// is left out only when neither side measured it. A missing mean is NaN.
pub fn compare(current: &[Record], baseline: &[BaselineRun]) -> Vec<Line> {
    let current: Vec<Row> = current.iter().map(Row::from).collect();
    let baseline: Vec<Row> = baseline.iter().map(Row::from).collect();
    let shared: BTreeSet<(&str, u64)> = {
        let baseline_keys: BTreeSet<_> = baseline
            .iter()
            .map(|r| (r.scenario.as_str(), r.seed))
            .collect();
        current
            .iter()
            .map(|r| (r.scenario.as_str(), r.seed))
            .filter(|key| baseline_keys.contains(key))
            .collect()
    };
    let in_both = |rows: &[Row]| -> Vec<Row> {
        rows.iter()
            .filter(|r| shared.contains(&(r.scenario.as_str(), r.seed)))
            .cloned()
            .collect()
    };
    let (current, baseline) = (in_both(&current), in_both(&baseline));
    let mut scenarios: Vec<&str> = Vec::new();
    for row in &current {
        if !scenarios.contains(&row.scenario.as_str()) {
            scenarios.push(&row.scenario);
        }
    }
    let mut lines = Vec::new();
    for scenario in scenarios.iter().copied().map(Some).chain([None]) {
        let members: Vec<&str> = scenario.map_or_else(|| scenarios.clone(), |name| vec![name]);
        for metric in &METRICS {
            let values = |rows: &[Row], name: &str| -> Vec<f64> {
                rows.iter()
                    .filter(|r| r.scenario == name)
                    .filter_map(metric.value)
                    .collect()
            };
            let strata: Vec<Stratum> = members
                .iter()
                .map(|name| Stratum {
                    current: values(&current, name),
                    baseline: values(&baseline, name),
                })
                .collect();
            lines.extend(line(scenario.unwrap_or("TOTAL"), metric, &strata));
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

/// One scenario's values of a metric on each side.
struct Stratum {
    current: Vec<f64>,
    baseline: Vec<f64>,
}

/// The line of one metric over `strata`, one scenario's or all of them; none when neither side measured it.
///
/// The means are over every value. The standard error is stratified by scenario, since both sides ran the same
/// seeds of each scenario and the spread between scenarios cancels in the difference of means:
/// `sqrt(sum_s (n_s/N)^2 (var_current_s/n_current_s + var_baseline_s/n_baseline_s))`, with `n_s` the baseline runs of stratum `s`.
fn line(scenario: &str, metric: &Metric, strata: &[Stratum]) -> Option<Line> {
    let current: Vec<f64> = strata
        .iter()
        .flat_map(|s| s.current.iter().copied())
        .collect();
    let baseline: Vec<f64> = strata
        .iter()
        .flat_map(|s| s.baseline.iter().copied())
        .collect();
    if current.is_empty() && baseline.is_empty() {
        return None;
    }
    let one_side_lacks = current.is_empty() || baseline.is_empty();
    let (current_mean, baseline_mean) = (mean(&current), mean(&baseline));
    let spread = strata
        .iter()
        .filter(|s| s.current.len() >= 2 && s.baseline.len() >= 2);
    let baseline_total: usize = strata
        .iter()
        .filter(|s| !s.current.is_empty() && !s.baseline.is_empty())
        .map(|s| s.baseline.len())
        .sum();
    let error = spread
        .clone()
        .map(|s| {
            let weight = float(s.baseline.len()) / float(baseline_total);
            weight.powi(2)
                * (variance(&s.current) / float(s.current.len())
                    + variance(&s.baseline) / float(s.baseline.len()))
        })
        .sum::<f64>()
        .sqrt();
    let diff = current_mean - baseline_mean;
    let z = if one_side_lacks || spread.count() == 0 || diff == 0.0 {
        0.0
    } else if error > 0.0 {
        diff / error
    } else {
        f64::INFINITY.copysign(diff)
    };
    let Band { rel, abs } = metric.band;
    let flagged = one_side_lacks
        || current.len() != baseline.len()
        || if metric.name == CONCLUDED {
            diff < 0.0
        } else {
            diff.abs() > abs && diff.abs() > rel * baseline_mean.abs() && z.abs() > Z
        };
    Some(Line {
        scenario: scenario.to_owned(),
        metric: metric.name,
        current: current_mean,
        baseline: baseline_mean,
        current_runs: current.len(),
        baseline_runs: baseline.len(),
        z,
        flagged,
    })
}

fn cell(value: f64) -> String {
    if value.is_nan() {
        "-".to_owned()
    } else {
        format!("{value:.4}")
    }
}

/// Writes the table, one line per scenario and metric, flagged lines marked, and the count flagged.
pub fn report(w: &mut impl io::Write, lines: &[Line]) -> io::Result<()> {
    writeln!(
        w,
        "sanity: means over the runs in both the current records and the baseline; flagged when the means differ by more than the band and by more than {Z} standard errors, when concluded runs were lost, or when a side lacks the metric on runs the other has"
    )?;
    writeln!(
        w,
        "scenario                metric                    current_runs baseline_runs    current   baseline     rel_diff       z  flag"
    )?;
    for l in lines {
        let rel = if l.current.is_nan() || l.baseline.is_nan() {
            "n/a".to_owned()
        } else if l.baseline == 0.0 {
            if l.current == 0.0 { "+0.0%" } else { "+inf%" }.to_owned()
        } else {
            format!(
                "{:+.1}%",
                (l.current - l.baseline) / l.baseline.abs() * 100.0
            )
        };
        writeln!(
            w,
            "{:<23} {:<25} {:>12} {:>13} {:>10} {:>10} {:>12} {:>7.2}  {}",
            l.scenario,
            l.metric,
            l.current_runs,
            l.baseline_runs,
            cell(l.current),
            cell(l.baseline),
            rel,
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
