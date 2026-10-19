use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;
use togi_machine::{CoreId, CoreSet, object_only};

use crate::error::Error;
use crate::outcome::Status;
use crate::record::Run;

object_only! {
    /// An ensemble's values for one quantity: the median, the 10th to 90th percentile and the extremes. Percentiles
    /// use the nearest rank, so each is a value some run produced; the median of an even count averages the middle
    /// two.
    #[derive(Clone, Copy, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Range {
        pub min: f64,
        pub p10: f64,
        pub median: f64,
        pub p90: f64,
        pub max: f64,
    }
}

impl Range {
    fn of(sorted: &[f64]) -> Option<Self> {
        let n = sorted.len();
        let (min, max) = (*sorted.first()?, *sorted.last()?);
        let rank = |percent: usize| sorted[(percent * n).div_ceil(100).max(1) - 1];
        let median = if n.is_multiple_of(2) {
            f64::midpoint(sorted[n / 2 - 1], sorted[n / 2])
        } else {
            sorted[n / 2]
        };
        Some(Self {
            min,
            p10: rank(10),
            median,
            p90: rank(90),
            max,
        })
    }
}

/// Offsets, depths and counts are far below 2^52, so they are exact as floats.
#[expect(
    clippy::cast_precision_loss,
    reason = "offsets and counts are far below 2^52"
)]
pub(crate) const fn float(value: i64) -> f64 {
    value as f64
}

fn saturating_summary_count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// What a metric measures.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum MetricKind {
    /// A core's final offset.
    Core(CoreId),
    /// The sum of the final offsets.
    Depth,
    /// Hours to conclusion, over concluded runs only.
    Hours,
    Crashes,
    Hunts,
}

impl MetricKind {
    /// Whether the value of a run that has not finished only bounds the final one from below.
    pub const fn grows(self) -> bool {
        matches!(self, Self::Hours | Self::Crashes | Self::Hunts)
    }
}

impl fmt::Display for MetricKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Core(core) => write!(f, "core {:02}", core.get()),
            Self::Depth => f.write_str("depth"),
            Self::Hours => f.write_str("hours"),
            Self::Crashes => f.write_str("crashes"),
            Self::Hunts => f.write_str("hunts"),
        }
    }
}

/// One forecast quantity across the ensemble.
#[derive(Clone, Debug, PartialEq)]
pub struct Metric {
    pub kind: MetricKind,
    /// `None` when no run supplies the metric: hours when no run concluded.
    pub range: Option<Range>,
    values: Vec<f64>,
}

impl Metric {
    fn new(kind: MetricKind, mut values: Vec<f64>) -> Self {
        values.sort_by(f64::total_cmp);
        Self {
            kind,
            range: Range::of(&values),
            values,
        }
    }

    /// Renders a value of the metric: hours to two decimals, counts and offsets as they are.
    pub fn format(&self, value: f64) -> String {
        match self.kind {
            MetricKind::Hours => format!("{value:.2}"),
            MetricKind::Core(_) | MetricKind::Depth | MetricKind::Crashes | MetricKind::Hunts => {
                value.to_string()
            }
        }
    }

    /// The share of runs below `value` plus half of those equal to it, in percent; with `below`, only the share
    /// below `value`, which bounds the percentile of a value that can still grow. `None` when no run supplies
    /// the metric.
    pub fn percentile(&self, value: f64, below: bool) -> Option<f64> {
        if self.values.is_empty() {
            return None;
        }
        let less = self.values.iter().filter(|&&x| x < value).count();
        let equal = if below {
            0
        } else {
            self.values
                .iter()
                .filter(|&&x| x.total_cmp(&value).is_eq())
                .count()
        };
        let share = float(saturating_summary_count(less)) + float(saturating_summary_count(equal)) / 2.0;
        Some(100.0 * share / float(saturating_summary_count(self.values.len())))
    }
}

/// The forecast quantities in report order: each core's final offset in core order, the total depth, hours to
/// conclusion over concluded runs only, crashes and hunts. Every run counts in the others, so the crashes and
/// hunts of runs that did not conclude enter as lower bounds.
pub fn metrics(runs: &[Run]) -> Result<Vec<Metric>, Error> {
    let first = runs.first().ok_or(Error::NoRuns)?;
    let cores: CoreSet = first.outcome.profile.iter().map(|(core, _)| core).collect();
    for run in runs {
        let theirs: CoreSet = run.outcome.profile.iter().map(|(core, _)| core).collect();
        if theirs != cores {
            return Err(Error::RunCores {
                machine: run.machine.clone(),
                seed: run.seed,
                run: theirs,
                first: cores,
            });
        }
    }
    let column = |value: &dyn Fn(&Run) -> Option<f64>| runs.iter().filter_map(value).collect();
    let mut all: Vec<Metric> = cores
        .iter()
        .map(|core| {
            let offset = |run: &Run| run.outcome.profile.get(core).map(|offset| float(offset.0));
            Metric::new(MetricKind::Core(core), column(&offset))
        })
        .collect();
    all.push(Metric::new(
        MetricKind::Depth,
        column(&|run| Some(float(run.outcome.depth))),
    ));
    all.push(Metric::new(
        MetricKind::Hours,
        column(&|run| (run.outcome.status == Status::Concluded).then_some(run.outcome.hours)),
    ));
    all.push(Metric::new(
        MetricKind::Crashes,
        column(&|run| Some(f64::from(run.outcome.crashes))),
    ));
    all.push(Metric::new(
        MetricKind::Hunts,
        column(&|run| Some(f64::from(run.outcome.hunts))),
    ));
    Ok(all)
}

object_only! {
    /// The ensemble's statuses and ranges, as [`metrics`] computes them.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Summary {
        pub runs: usize,
        pub concluded: usize,
        pub dead_ends: usize,
        pub censored: usize,
        /// Over concluded runs only; `None` when none concluded.
        pub hours: Option<Range>,
        pub crashes: Range,
        pub hunts: Range,
        pub depth: Range,
        pub cores: BTreeMap<CoreId, Range>,
    }
}

/// Counts the runs by status and ranges each metric.
pub fn summarize(runs: &[Run]) -> Result<Summary, Error> {
    let (mut hours, mut crashes, mut hunts, mut depth) = (None, None, None, None);
    let mut cores = BTreeMap::new();
    for metric in metrics(runs)? {
        match metric.kind {
            MetricKind::Core(core) => cores.extend(metric.range.map(|range| (core, range))),
            MetricKind::Depth => depth = metric.range,
            MetricKind::Hours => hours = metric.range,
            MetricKind::Crashes => crashes = metric.range,
            MetricKind::Hunts => hunts = metric.range,
        }
    }
    let (Some(crashes), Some(hunts), Some(depth)) = (crashes, hunts, depth) else {
        return Err(Error::NoRuns);
    };
    let count = |status| {
        runs.iter()
            .filter(|run| run.outcome.status == status)
            .count()
    };
    Ok(Summary {
        runs: runs.len(),
        concluded: count(Status::Concluded),
        dead_ends: count(Status::DeadEnd),
        censored: count(Status::Censored),
        hours,
        crashes,
        hunts,
        depth,
        cores,
    })
}

#[cfg(test)]
mod numeric_tests {
    use super::saturating_summary_count;
    use test_case::test_case;

    #[test_case(0, 0; "zero")]
    #[test_case(1, 1; "one")]
    #[test_case(i64::MAX as usize, i64::MAX; "largest exact count")]
    #[test_case(i64::MAX as usize + 1, i64::MAX; "first saturated count")]
    #[test_case(usize::MAX, i64::MAX; "largest platform count")]
    #[cfg(target_pointer_width = "64")]
    fn summary_counts_saturate(count: usize, expected: i64) {
        assert_eq!(saturating_summary_count(count), expected);
    }
}
