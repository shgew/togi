use std::collections::BTreeSet;
use std::io::{self, Write};

use serde::{Deserialize, Serialize};
use togi_sim::saturating_count;
use togi_simrun::day_night::Metrics;

use crate::day_night::{CapKind, DailyCoverage, RegisteredScenario, Registration};

/// One independently reproducible seed; the policy label is not a gate verdict.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub scenario: String,
    pub seed: u64,
    pub comparator: String,
    pub metrics: Metrics,
}

/// Exact empirical tail results for one scenario and policy. A missing convergence is infinite.
#[derive(Debug, Serialize)]
pub struct Criteria {
    pub covered_climb: Option<bool>,
    pub hidden_year_one: Option<bool>,
    pub overshoot_fraction: bool,
    pub overshoot_absolute: bool,
    pub convergence: bool,
    pub retention: Option<bool>,
    pub fixed_cap_steps: Option<bool>,
}

impl Criteria {
    pub fn passed(&self) -> bool {
        self.covered_climb != Some(false)
            && self.hidden_year_one != Some(false)
            && self.overshoot_fraction
            && self.overshoot_absolute
            && self.convergence
            && self.retention != Some(false)
            && self.fixed_cap_steps != Some(false)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScoreError {
    #[error("select registered day/night seeds")]
    Seeds(#[from] crate::day_night::Error),
    #[error("incomplete or duplicate day/night seed set for scenario {0}")]
    SeedsMismatch(String),
    #[error("invalid per-CCD day/night metric dimensions for scenario {0}")]
    Dimensions(String),
    #[error("day/night records contain an unregistered scenario")]
    UnknownScenario,
}

/// Checks completeness before evaluating each scenario. The caller identifies one recorded policy;
/// comparator scores remain diagnostics, not a substitute for the real policy's registered score.
pub fn score(
    registration: &Registration,
    selection: crate::Selection,
    policy: &str,
    records: &[Record],
) -> Result<Vec<(String, Criteria)>, ScoreError> {
    let ranges = match selection {
        crate::Selection::Dev => vec![&registration.seeds.dev],
        crate::Selection::Holdout => vec![&registration.seeds.holdout],
        crate::Selection::All => vec![&registration.seeds.dev, &registration.seeds.holdout],
    };
    let mut expected = BTreeSet::new();
    for range in ranges {
        expected.extend(range.values()?);
    }
    if records
        .iter()
        .filter(|record| record.comparator == policy)
        .any(|record| {
            !registration
                .scenarios
                .iter()
                .any(|scenario| scenario.name == record.scenario)
        })
    {
        return Err(ScoreError::UnknownScenario);
    }
    registration
        .scenarios
        .iter()
        .map(|scenario| {
            let selected: Vec<_> = records
                .iter()
                .filter(|record| record.scenario == scenario.name && record.comparator == policy)
                .collect();
            let actual: BTreeSet<_> = selected.iter().map(|record| record.seed).collect();
            if actual != expected || selected.len() != expected.len() {
                return Err(ScoreError::SeedsMismatch(scenario.name.clone()));
            }
            if selected
                .iter()
                .any(|record| !valid_dimensions(&record.metrics))
            {
                return Err(ScoreError::Dimensions(scenario.name.clone()));
            }
            Ok((
                scenario.name.clone(),
                judge(
                    registration,
                    scenario,
                    &selected
                        .iter()
                        .map(|record| &record.metrics)
                        .collect::<Vec<_>>(),
                ),
            ))
        })
        .collect()
}

fn valid_dimensions(metric: &Metrics) -> bool {
    let count = metric.final_depth.len();
    (1..=2).contains(&count)
        && metric.convergence_days.len() == count
        && metric.max_overshoot.len() == count
        && metric.promotions.len() == count
        && metric.daily_co_crashes.len() == count
        && metric.year_one_co_crashes.len() == count
        && metric.climb_co_crashes.len() == count
        && metric.converged_depth.len() == count
        && metric.background_retreat_counts.len() == count
        && metric.justified_retreat_counts.len() == count
        && metric.non_cpu_retreat_counts.len() == count
        && metric.unattributed_retreat_counts.len() == count
}

/// Judges one complete registered seed set, never dropping censored seeds from denominators.
/// Callers must check experiment identity and seed completeness before presenting this as a gate.
pub fn judge(
    registration: &Registration,
    scenario: &RegisteredScenario,
    metrics: &[&Metrics],
) -> Criteria {
    let thresholds = &registration.thresholds;
    let fraction = |predicate: &dyn Fn(&Metrics) -> bool| {
        let count = metrics
            .iter()
            .filter(|metric| !metric.censored && predicate(metric))
            .count();
        saturating_count(count) / saturating_count(metrics.len())
    };
    let convergences: Vec<_> = metrics
        .iter()
        .map(|metric| worst_convergence(metric))
        .collect();
    Criteria {
        covered_climb: match scenario.daily_class {
            DailyCoverage::Covered => Some(
                fraction(&|metric| metric.climb_co_crashes.iter().all(|count| *count == 0))
                    >= thresholds.covered_crash_free_fraction,
            ),
            DailyCoverage::Hidden => None,
        },
        hidden_year_one: match scenario.daily_class {
            DailyCoverage::Hidden => Some(
                fraction(&|metric| {
                    metric
                        .year_one_co_crashes
                        .iter()
                        .all(|count| *count <= thresholds.hidden_year_one_max_crashes_per_ccd)
                }) >= thresholds.hidden_year_one_fraction,
            ),
            DailyCoverage::Covered => None,
        },
        overshoot_fraction: fraction(&|metric| {
            metric.max_overshoot.iter().all(|depth| *depth <= 1)
        }) >= thresholds.overshoot_one_count_fraction,
        overshoot_absolute: !metrics.is_empty()
            && metrics.iter().all(|metric| {
                !metric.censored
                    && metric
                        .max_overshoot
                        .iter()
                        .all(|depth| *depth <= thresholds.overshoot_absolute_max_counts)
            }),
        convergence: median(&convergences) <= f64::from(thresholds.median_convergence_days),
        retention: scenario.retention.then(|| {
            fraction(&|metric| {
                metric.converged_depth.iter().zip(&metric.final_depth).all(
                    |(converged, final_depth)| {
                        converged.is_some_and(|depth| {
                            *final_depth
                                >= depth.saturating_sub(thresholds.retention_max_lost_counts)
                        })
                    },
                )
            }) >= thresholds.retention_fraction
        }),
        fixed_cap_steps: match scenario.cap {
            CapKind::Fixed => Some(
                !metrics.is_empty()
                    && metrics.iter().all(|metric| {
                        !metric.censored
                            && metric
                                .promotions
                                .iter()
                                .all(|steps| *steps <= thresholds.fixed_cap_max_steps_per_ccd)
                    }),
            ),
            CapKind::Moving | CapKind::Firmware => None,
        },
    }
}

fn worst_convergence(metrics: &Metrics) -> f64 {
    if metrics.censored || metrics.convergence_days.is_empty() {
        return f64::INFINITY;
    }
    metrics
        .convergence_days
        .iter()
        .map(|day| day.map_or(f64::INFINITY, f64::from))
        .fold(0.0, f64::max)
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return f64::INFINITY;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        sorted[middle - 1] / 2.0 + sorted[middle] / 2.0
    } else {
        sorted[middle]
    }
}

/// Diagnostic summary, with no PASS/FAIL attribution to the absent real policy.
pub fn report(
    out: &mut dyn Write,
    registration: &Registration,
    records: &[Record],
) -> io::Result<()> {
    writeln!(
        out,
        "{}: comparator diagnostics; Ruleset 13 pending #1217; slopes {:?} unscored",
        registration.id, registration.policy_slopes
    )?;
    writeln!(
        out,
        "scenario comparator seeds CO/day suspect year1/CCD climb-free% converge-d overshoot steps final-depth retained% bg-retreat CO-retreat nonCPU-retreat censored"
    )?;
    for scenario in &registration.scenarios {
        for comparator in ["co-zero", "fixed-margin"] {
            let metrics: Vec<_> = records
                .iter()
                .filter(|record| {
                    record.scenario == scenario.name && record.comparator == comparator
                })
                .map(|record| &record.metrics)
                .collect();
            let total = |field: fn(&Metrics) -> &[u32]| -> u64 {
                metrics
                    .iter()
                    .flat_map(|metric| field(metric))
                    .map(|value| u64::from(*value))
                    .sum()
            };
            let max = |field: fn(&Metrics) -> &[u32]| -> u32 {
                metrics
                    .iter()
                    .flat_map(|metric| field(metric))
                    .copied()
                    .max()
                    .unwrap_or(0)
            };
            let overshoot = metrics
                .iter()
                .flat_map(|metric| &metric.max_overshoot)
                .copied()
                .max()
                .unwrap_or(0);
            let final_depth = metrics
                .iter()
                .map(|metric| {
                    metric
                        .final_depth
                        .iter()
                        .map(|depth| f64::from(*depth))
                        .sum::<f64>()
                        / saturating_count(metric.final_depth.len())
                })
                .collect::<Vec<_>>();
            let convergence = median(
                &metrics
                    .iter()
                    .map(|metric| worst_convergence(metric))
                    .collect::<Vec<_>>(),
            );
            let climb_free = saturating_count(
                metrics
                    .iter()
                    .filter(|metric| {
                        !metric.censored && metric.climb_co_crashes.iter().all(|count| *count == 0)
                    })
                    .count(),
            ) / saturating_count(metrics.len())
                * 100.0;
            let retained = saturating_count(
                metrics
                    .iter()
                    .filter(|metric| {
                        !metric.censored
                            && metric.converged_depth.iter().zip(&metric.final_depth).all(
                                |(converged, final_depth)| {
                                    converged.is_some_and(|depth| {
                                        *final_depth
                                            >= depth.saturating_sub(
                                                registration.thresholds.retention_max_lost_counts,
                                            )
                                    })
                                },
                            )
                    })
                    .count(),
            ) / saturating_count(metrics.len())
                * 100.0;
            let suspects: u64 = metrics
                .iter()
                .map(|metric| u64::from(metric.cpu_suspect_events))
                .sum();
            let crashes: u64 = metrics
                .iter()
                .map(|metric| u64::from(metric.true_co_crashes))
                .sum();
            let censored = metrics.iter().filter(|metric| metric.censored).count();
            writeln!(
                out,
                "{} {} {} {} {} {} {:.1} {:.1} {} {} {:.1} {:.1} {} {} {} {}",
                scenario.name,
                comparator,
                metrics.len(),
                crashes,
                suspects,
                max(|metric| &metric.year_one_co_crashes),
                climb_free,
                convergence,
                overshoot,
                max(|metric| &metric.promotions),
                median(&final_depth),
                retained,
                total(|metric| &metric.background_retreat_counts),
                total(|metric| &metric.justified_retreat_counts),
                total(|metric| &metric.non_cpu_retreat_counts),
                censored
            )?;
        }
    }
    writeln!(
        out,
        "CO/day counts unique CO-caused daily crashes over seeds; retreats sum per-CCD counts; year1/CCD, overshoot and steps are maxima; convergence is median worst-CCD day (inf = not converged); final-depth is median per-run mean CCD depth. Per-CCD JSON crash counts can charge both CCDs for a joint crash. Censored partial counts are lower bounds."
    )
}
