use std::fmt;

use togi_journal::Build;
use togi_machine::CoreSet;

use crate::error::Error;
use crate::outcome::{Outcome, Status, follow};
use crate::record::Record;
use crate::session::Session;
use crate::summary::{Metric, MetricKind, float, metrics};

/// Where a real value falls against a metric's 10th to 90th percentile.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Placement {
    Inside,
    Below,
    Above,
    /// A lower bound inside or below the range; the run had not finished, so the final value may still land
    /// anywhere at or above it.
    Open,
    /// The ensemble has no value for the metric.
    Unforecast,
}

impl fmt::Display for Placement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Inside => "yes",
            Self::Below => "below",
            Self::Above => "above",
            Self::Open => "open",
            Self::Unforecast => "-",
        })
    }
}

/// One metric scored.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub metric: Metric,
    pub actual: f64,
    /// The actual value only bounds the final one from below: the run had not concluded.
    pub bound: bool,
    pub placement: Placement,
    /// The actual value less the ensemble's median; `None` when the ensemble has no value.
    pub error: Option<f64>,
    /// The actual value's percentile among the ensemble's, as [`Metric::percentile`] defines it.
    pub percentile: Option<f64>,
}

/// A difference that makes the forecast less comparable with the run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Flag {
    /// The forecast was made from a tree with uncommitted changes.
    DirtyForecast {
        commit: String,
    },
    /// Another build revision ran after the anchor.
    Revision {
        rev: String,
        commit: String,
    },
    Ruleset {
        ran: u32,
        forecast: u32,
    },
    EvidenceEpoch {
        ran: u32,
        forecast: u32,
    },
}

impl fmt::Display for Flag {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DirtyForecast { commit } => {
                write!(
                    f,
                    "the forecast was made from a dirty tree at commit {commit}"
                )
            }
            Self::Revision { rev, commit } => write!(
                f,
                "build revision {rev} ran after the anchor; the forecast is at commit {commit}"
            ),
            Self::Ruleset { ran, forecast } => write!(
                f,
                "ruleset {ran} ran after the anchor; the forecast used ruleset {forecast}"
            ),
            Self::EvidenceEpoch { ran, forecast } => write!(
                f,
                "evidence epoch {ran} ran after the anchor; the forecast used epoch {forecast}"
            ),
        }
    }
}

/// A real run scored against a forecast.
#[derive(Clone, Debug)]
pub struct Score<'r> {
    pub record: &'r Record,
    pub outcome: Outcome,
    /// The distinct builds that ran from the anchor through the run's terminal point, in order: the build in
    /// effect at the anchor when its invocation wrote events after it, then each build stamped after the anchor.
    pub builds: Vec<Build>,
    pub flags: Vec<Flag>,
    /// In report order, as [`metrics`] lists them.
    pub rows: Vec<Row>,
}

impl Record {
    /// Scores the real run in `sessions` against the forecast. It refuses sessions that lack the anchor, changed
    /// the journal through it, or hold the run's terminal point in a session this build cannot replay, and a run
    /// whose cores are not the forecast's.
    pub fn score(&self, sessions: &[Session]) -> Result<Score<'_>, Error> {
        let followed = follow::<togi_tuner::State>(sessions, &self.anchor)?;
        let (outcome, builds) = (followed.outcome, followed.builds);
        let metrics = metrics(&self.runs)?;
        let forecast: CoreSet = metrics
            .iter()
            .filter_map(|metric| match metric.kind {
                MetricKind::Core(core) => Some(core),
                _ => None,
            })
            .collect();
        let run: CoreSet = outcome.profile.iter().map(|(core, _)| core).collect();
        if run != forecast {
            return Err(Error::ScoreCores { run, forecast });
        }
        let flags = self.flags(&builds);
        let censored = outcome.status == Status::Censored;
        let rows = metrics
            .into_iter()
            .map(|metric| {
                let actual = match metric.kind {
                    MetricKind::Core(core) => outcome
                        .profile
                        .get(core)
                        .map_or(0.0, |offset| float(offset.0)),
                    MetricKind::Depth => float(outcome.depth),
                    MetricKind::Hours => outcome.hours,
                    MetricKind::Crashes => f64::from(outcome.crashes),
                    MetricKind::Hunts => f64::from(outcome.hunts),
                };
                row(metric, actual, censored)
            })
            .collect();
        Ok(Score {
            record: self,
            outcome,
            builds,
            flags,
            rows,
        })
    }

    fn flags(&self, builds: &[Build]) -> Vec<Flag> {
        let mut flags = Vec::new();
        if self.dirty {
            flags.push(Flag::DirtyForecast {
                commit: self.commit.clone(),
            });
        }
        for build in builds {
            let rev = build.rev.as_deref().unwrap_or_default();
            if !same_revision(rev, &self.commit) {
                flags.push(Flag::Revision {
                    rev: rev.to_owned(),
                    commit: self.commit.clone(),
                });
            }
            if build.ruleset != self.ruleset {
                flags.push(Flag::Ruleset {
                    ran: build.ruleset,
                    forecast: self.ruleset,
                });
            }
            if build.evidence != self.evidence {
                flags.push(Flag::EvidenceEpoch {
                    ran: build.evidence,
                    forecast: self.evidence,
                });
            }
        }
        flags
    }
}

fn row(metric: Metric, actual: f64, censored: bool) -> Row {
    let bound = censored && metric.kind.grows();
    let placement = match metric.range {
        None => Placement::Unforecast,
        Some(range) if actual > range.p90 => Placement::Above,
        Some(_) if bound => Placement::Open,
        Some(range) if actual < range.p10 => Placement::Below,
        Some(_) => Placement::Inside,
    };
    Row {
        error: metric.range.map(|range| actual - range.median),
        percentile: metric.percentile(actual, bound),
        metric,
        actual,
        bound,
        placement,
    }
}

/// Whether a build's short revision names the forecast's commit, either of which may be abbreviated. A dirty
/// build, or one without a revision, names none.
fn same_revision(rev: &str, commit: &str) -> bool {
    if rev.is_empty() || commit.is_empty() || rev.ends_with("-dirty") {
        return false;
    }
    rev.starts_with(commit) || commit.starts_with(rev)
}
