//! Calendar execution for the separately registered physical-core experiment.

use std::time::{Duration, Instant};

use togi_sim::day_night::TrialClass;
use togi_sim::day_night::successor::{EpochKind, Scenario};

use super::assemble::{Snapshot, Stop, assemble};
use super::machine::Runtime;
use super::{Epoch, FirmwareRestoration, PolicyReport, ProfileStep, Provenance};
use crate::day_night::{CalendarConfig, DayObservation, Night, Policy, RunError};

/// Policy-owned, truth-free witnesses for applications and firmware restoration.
/// Empty means unavailable, not that qualification or permission has been established.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PolicyApplications {
    pub steps: Vec<ProfileStep>,
    pub restorations: Vec<FirmwareRestoration>,
    pub ceilings: Vec<(super::UnitId, super::CeilingEvidence)>,
}

/// One scenario/seed invocation. Empty daily and epoch vectors select the built-in
/// truth calendar; a registered caller validates and supplies its ordered daily mix.
pub struct RunConfig {
    pub calendar: CalendarConfig,
    pub provenance: Provenance,
    pub epochs: Vec<Epoch>,
    pub daily: Vec<(TrialClass, Duration)>,
    pub jobs: u32,
    pub resource_class: String,
    pub compilation_seconds: f64,
    pub tolerance: f64,
    pub deadline: Option<Instant>,
}

impl PolicyReport {
    /// A comparator without a successor finish contract or machine allowance.
    pub fn unavailable() -> Self {
        Self {
            finishes: Vec::new(),
            allowance: None,
            events: Vec::new(),
            stopped_units: Vec::new(),
            inherited_stops: Vec::new(),
            newest_step: None,
            readback: None,
            unit_membership: Vec::new(),
        }
    }
}

/// Runs the full horizon, including days after policy completion of its climb.
/// Early policy termination, budget expiry and operational errors retain their observed
/// prefix in the returned record. Invalid inputs fail before starting a trajectory.
///
/// # Errors
/// Refuses malformed calendars, profiles and scenario identities before execution.
pub fn run(mut config: RunConfig, policy: &mut dyn Policy) -> Result<super::Record, RunError> {
    let started = Instant::now();
    let scenario = Scenario::builtin(&config.provenance.scenario, config.provenance.seed)?;
    validate(&config)?;
    if config.daily.is_empty() {
        config.daily = daily(&scenario, &config.calendar)?;
    }
    let active = config.daily.iter().map(|(_, duration)| *duration).sum::<Duration>();
    if active != Duration::from_secs_f64(config.calendar.active_hours * 3600.0) {
        return Err(RunError::Calendar("successor daily mix active duration"));
    }
    let mut runtime = Runtime::new(scenario, config.provenance.seed)?;
    runtime.set_deadline(config.deadline);
    let mut machine = runtime.seams();
    let mut snapshots = vec![snapshot(&runtime, policy)];
    let execution = (|| -> Result<Option<Stop>, RunError> {
        for day in 1..=config.calendar.days {
            if config.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Ok(Some(Stop::BudgetExpired));
            }
            if policy.is_finished() {
                return Ok(Some(Stop::PolicyStopped));
            }
            runtime.set_day(day);
            let day_start = runtime.now();
            let epoch = runtime.scenario().epoch(day)?;
            if epoch.first_day == day && epoch.kind == EpochKind::Firmware {
                runtime.firmware_changed()?;
                policy.firmware_changed(&mut machine)?;
                capture(&mut snapshots, &runtime, policy);
            }
            policy.start_day(day, &mut machine)?;
            capture(&mut snapshots, &runtime, policy);
            for (class, budget) in &config.daily {
                let mut remaining = *budget;
                while !remaining.is_zero() {
                    let event = runtime.active(class, remaining)?;
                    if event.elapsed.is_zero() {
                        return Err(RunError::Calendar("zero-length successor active event"));
                    }
                    remaining = remaining.checked_sub(event.elapsed)
                        .ok_or(RunError::Calendar("successor active overrun"))?;
                    let evidence = if event.cause.is_some() {
                        DayObservation::Ended { active: event.elapsed, answer: event.answer }
                    } else {
                        DayObservation::Completed { active: event.elapsed }
                    };
                    policy.observe_day(day, &evidence, &mut machine)?;
                    capture(&mut snapshots, &runtime, policy);
                    if policy.is_finished() {
                        return Ok(Some(Stop::PolicyStopped));
                    }
                }
            }
            let weekday = u8::try_from((day - 1) % 7)
                .map_err(|_| RunError::Calendar("successor weekday"))?;
            if config.calendar.night_weekdays.contains(&weekday) {
                let budget = Duration::from_secs_f64(config.calendar.night_hours * 3600.0);
                let night = Night { day, budget, trials: runtime.night_specs()? };
                runtime.begin_night(budget);
                let result = policy.night(&night, &mut machine);
                runtime.end_night();
                capture(&mut snapshots, &runtime, policy);
                result?;
            }
            if policy.is_finished() {
                return Ok(Some(Stop::PolicyStopped));
            }
            let spent = runtime.now().checked_sub(day_start)
                .ok_or(RunError::Calendar("successor monotonic clock moved backwards"))?;
            let remainder = Duration::from_hours(24).checked_sub(spent)
                .ok_or(RunError::Calendar("successor policy overran the calendar day"))?;
            runtime.sleep(remainder)?;
            capture(&mut snapshots, &runtime, policy);
        }
        Ok(None)
    })();
    let mut stop = match execution {
        Ok(stop) => stop,
        Err(error) if budget_expired(&error) => Some(Stop::BudgetExpired),
        Err(error) => Some(Stop::Harness(togi_machine::error_chain(&error))),
    };
    capture(&mut snapshots, &runtime, policy);
    match runtime.depths() {
        Ok(readback) => {
            if let Some(last) = snapshots.last_mut() {
                last.report.readback = Some(readback);
            }
        }
        Err(error) => stop = Some(Stop::Harness(togi_machine::error_chain(&error))),
    }
    let observed = runtime.now();
    let observations = runtime.drain_observations();
    let mut record = assemble(
        config, &runtime.scenario(), observations, snapshots, observed, stop, 0.0,
    )?;
    record.runtime.wall_seconds = started.elapsed().as_secs_f64();
    Ok(record)
}

fn snapshot(runtime: &Runtime, policy: &dyn Policy) -> Snapshot {
    Snapshot { at: runtime.now(), report: policy.report(), applications: policy.applications() }
}

fn capture(snapshots: &mut Vec<Snapshot>, runtime: &Runtime, policy: &dyn Policy) {
    let next = snapshot(runtime, policy);
    if snapshots.last().is_none_or(|previous| {
        previous.report != next.report || previous.applications != next.applications
    }) {
        snapshots.push(next);
    }
}

fn budget_expired(error: &RunError) -> bool {
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(error) = cause {
        if error.downcast_ref::<RunError>().is_some_and(|error| matches!(error, RunError::BudgetExpired)) {
            return true;
        }
        // Transparent machine errors forward their source and omit the boxed error
        // itself; inspect that payload so a budget interruption does not become a
        // harness failure merely because it crossed a machine seam.
        cause = if let Some(togi_machine::Error::Other(inner)) =
            error.downcast_ref::<togi_machine::Error>()
        {
            Some(inner.as_ref())
        } else {
            error.source()
        };
    }
    false
}

fn validate(config: &RunConfig) -> Result<(), RunError> {
    let calendar = &config.calendar;
    if calendar.days == 0 || calendar.days > 36_500 || calendar.year_days == 0 {
        return Err(RunError::Calendar("successor horizon"));
    }
    if !calendar.active_hours.is_finite() || calendar.active_hours <= 0.0
        || !calendar.night_hours.is_finite() || calendar.night_hours < 0.0
        || calendar.active_hours + calendar.night_hours > 24.0
        || !calendar.lambda_per_hour.is_finite() || calendar.lambda_per_hour <= 0.0
        || calendar.night_weekdays.iter().any(|day| *day > 6)
        || config.jobs == 0 || config.resource_class.is_empty()
        || !config.compilation_seconds.is_finite() || config.compilation_seconds < 0.0
        || !config.tolerance.is_finite() || config.tolerance < 0.0 || config.tolerance >= 1.0
    {
        return Err(RunError::Calendar("successor calendar and invocation"));
    }
    if calendar.firmware_change_day.is_some() {
        return Err(RunError::Calendar("successor truth owns the firmware schedule"));
    }
    Ok(())
}

fn daily(scenario: &Scenario, calendar: &CalendarConfig) -> Result<Vec<(TrialClass, Duration)>, RunError> {
    let fractions = calendar.daily_mix.clone().unwrap_or_else(|| {
        scenario.daily_mix().iter().map(|block| block.fraction).collect()
    });
    if fractions.len() != scenario.daily_mix().len()
        || fractions.iter().any(|fraction| !fraction.is_finite() || *fraction < 0.0)
        || (fractions.iter().sum::<f64>() - 1.0).abs() > 1e-12
    {
        return Err(RunError::Calendar("successor daily mix"));
    }
    let active = Duration::from_secs_f64(calendar.active_hours * 3600.0);
    let mut remaining = active;
    Ok(scenario.daily_mix().iter().zip(&fractions).enumerate().map(|(index, (block, fraction))| {
        let duration = if index + 1 == fractions.len() { remaining } else { active.mul_f64(*fraction).min(remaining) };
        remaining -= duration;
        (block.class.clone(), duration)
    }).collect())
}
