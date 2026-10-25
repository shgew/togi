use std::collections::BTreeSet;
use std::io::{self, Write};
use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use togi_sim::saturating_count;
use togi_simrun::day_night::{CalendarConfig, Metrics, PolicyKind, RunError};

use crate::day_night::{CapKind, DailyCoverage, RegisteredScenario, Registration};
use crate::pool;

/// One independently reproducible seed; the policy label is not a gate verdict.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub scenario: String,
    pub seed: u64,
    /// The label of the `PolicyKind` that ran.
    pub comparator: String,
    /// Wall-clock seconds this one run took.
    pub wall_s: f64,
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
pub enum ComparatorError {
    #[error("select registered day/night seeds")]
    Seeds(#[from] crate::day_night::Error),
    #[error("run day/night comparator")]
    Run(#[from] RunError),
    #[error("write day/night results")]
    Write(#[from] io::Error),
    #[error("encode day/night results")]
    Encode(#[from] serde_json::Error),
    #[error("a day/night run finished without a record")]
    Missing,
}

/// One run of a scenario and seed under a freshly built policy; production uses
/// `togi_simrun::day_night::run`. It is called from several threads at once.
pub type Runner<'a> =
    dyn Fn(&str, u64, PolicyKind, CalendarConfig) -> Result<Metrics, RunError> + Sync + 'a;

/// Runs every policy on every selected seed of every registered scenario on `jobs` threads, then
/// writes each record to `jsonl` as one JSON line. Records come back (and are written) in
/// registered scenario order, then seed, then the order of `policies`, whatever order the threads
/// finished in. `run` is `togi_simrun::day_night::run` outside tests.
pub fn run_policies(
    registration: &Registration,
    selection: crate::Selection,
    policies: &[PolicyKind],
    jobs: NonZeroUsize,
    run: &Runner<'_>,
    jsonl: &mut dyn Write,
) -> Result<Vec<Record>, ComparatorError> {
    let ranges = match selection {
        crate::Selection::Dev => vec![&registration.seeds.dev],
        crate::Selection::Holdout => vec![&registration.seeds.holdout],
        crate::Selection::All => vec![&registration.seeds.dev, &registration.seeds.holdout],
    };
    let mut work = Vec::new();
    for scenario in &registration.scenarios {
        for range in &ranges {
            for seed in range.values()? {
                for policy in policies {
                    work.push((scenario, seed, *policy));
                }
            }
        }
    }
    let slots: Vec<Mutex<Option<Record>>> = work.iter().map(|_| Mutex::new(None)).collect();
    let order: Vec<usize> = (0..work.len()).collect();
    let errors = pool::run(jobs.get(), &order, |index| {
        let (scenario, seed, policy) = work[index];
        let calendar = CalendarConfig {
            days: registration.calendar.days,
            year_days: registration.calendar.year_days,
            lambda_per_hour: registration.lambda_per_hour,
            active_hours: f64::from(registration.calendar.daily_hours),
            night_hours: f64::from(registration.calendar.night_hours),
            night_weekdays: registration.calendar.night_weekdays.clone(),
            firmware_change_day: (scenario.name == "firmware-change")
                .then_some(registration.calendar.firmware_change_day),
            ..CalendarConfig::default()
        };
        let started = Instant::now();
        let metrics = run(&scenario.name, seed, policy, calendar)?;
        let record = Record {
            scenario: scenario.name.clone(),
            seed,
            comparator: policy.label().to_owned(),
            wall_s: started.elapsed().as_secs_f64(),
            metrics,
        };
        *slots[index]
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(record);
        Ok::<(), RunError>(())
    });
    if let Some((_, error)) = errors.into_iter().next() {
        return Err(error.into());
    }
    let mut records = Vec::with_capacity(slots.len());
    for slot in slots {
        let record = slot
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ok_or(ComparatorError::Missing)?;
        serde_json::to_writer(&mut *jsonl, &record)?;
        writeln!(jsonl)?;
        records.push(record);
    }
    Ok(records)
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

/// What one complete registered seed set measured, before any threshold is applied. Fractions
/// divide by every seed, so a censored seed counts against each; they are NaN for no seeds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measured {
    pub runs: usize,
    pub censored: usize,
    /// Fraction of seeds with no climb crash on any CCD.
    pub climb_free: f64,
    /// Fraction of seeds whose every CCD stayed within the registered first-year crash count.
    pub year_one_within: f64,
    /// Fraction of seeds whose overshoot never exceeded one count.
    pub overshoot_one: f64,
    /// The deepest overshoot over all seeds and CCDs.
    pub max_overshoot: u8,
    /// Median over seeds of the worst CCD's convergence day; infinite when not converged.
    pub median_convergence_days: f64,
    /// Fraction of seeds whose every CCD retained its converged depth within the registered loss.
    pub retained: f64,
    /// The most cap-promotion steps any CCD took.
    pub max_promotions: u32,
}

/// Measures one complete registered seed set, never dropping censored seeds from denominators.
pub fn measure(registration: &Registration, metrics: &[&Metrics]) -> Measured {
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
    Measured {
        runs: metrics.len(),
        censored: metrics.iter().filter(|metric| metric.censored).count(),
        climb_free: fraction(&|metric| metric.climb_co_crashes.iter().all(|count| *count == 0)),
        year_one_within: fraction(&|metric| {
            metric
                .year_one_co_crashes
                .iter()
                .all(|count| *count <= thresholds.hidden_year_one_max_crashes_per_ccd)
        }),
        overshoot_one: fraction(&|metric| metric.max_overshoot.iter().all(|depth| *depth <= 1)),
        max_overshoot: metrics
            .iter()
            .flat_map(|metric| &metric.max_overshoot)
            .copied()
            .max()
            .unwrap_or(0),
        median_convergence_days: median(&convergences),
        retained: fraction(&|metric| {
            metric.converged_depth.iter().zip(&metric.final_depth).all(
                |(converged, final_depth)| {
                    converged.is_some_and(|depth| {
                        *final_depth >= depth.saturating_sub(thresholds.retention_max_lost_counts)
                    })
                },
            )
        }),
        max_promotions: metrics
            .iter()
            .flat_map(|metric| &metric.promotions)
            .copied()
            .max()
            .unwrap_or(0),
    }
}

/// Judges one complete registered seed set, never dropping censored seeds from denominators.
/// Callers must check experiment identity and seed completeness before presenting this as a gate.
pub fn judge(
    registration: &Registration,
    scenario: &RegisteredScenario,
    metrics: &[&Metrics],
) -> Criteria {
    let thresholds = &registration.thresholds;
    let measured = measure(registration, metrics);
    let uncensored = measured.runs > 0 && measured.censored == 0;
    Criteria {
        covered_climb: match scenario.daily_class {
            DailyCoverage::Covered => {
                Some(measured.climb_free >= thresholds.covered_crash_free_fraction)
            }
            DailyCoverage::Hidden => None,
        },
        hidden_year_one: match scenario.daily_class {
            DailyCoverage::Hidden => {
                Some(measured.year_one_within >= thresholds.hidden_year_one_fraction)
            }
            DailyCoverage::Covered => None,
        },
        overshoot_fraction: measured.overshoot_one >= thresholds.overshoot_one_count_fraction,
        overshoot_absolute: uncensored
            && measured.max_overshoot <= thresholds.overshoot_absolute_max_counts,
        convergence: measured.median_convergence_days
            <= f64::from(thresholds.median_convergence_days),
        retention: scenario
            .retention
            .then_some(measured.retained >= thresholds.retention_fraction),
        fixed_cap_steps: match scenario.cap {
            CapKind::Fixed => Some(
                uncensored && measured.max_promotions <= thresholds.fixed_cap_max_steps_per_ccd,
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
    policies: &[PolicyKind],
    records: &[Record],
) -> io::Result<()> {
    writeln!(
        out,
        "{}: policy summary; only ruleset13 can pass the gate; registered slopes {:?}",
        registration.id, registration.policy_slopes
    )?;
    writeln!(
        out,
        "scenario comparator seeds CO/day suspect year1/CCD climb-free% converge-d overshoot steps final-depth retained% bg-retreat CO-retreat nonCPU-retreat censored"
    )?;
    for scenario in &registration.scenarios {
        for comparator in policies.iter().map(|policy| policy.label()) {
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
