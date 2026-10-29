//! Calendar-driven policy experiments. Truth is retained by the runner, never passed to a policy.

mod machine;
mod policy;
mod ruleset13;
pub mod successor;

use std::time::Duration;

use serde::{Deserialize, Serialize};
use togi_machine::{Machine, TargetOffset, TrialSpec};
use togi_sim::day_night::{FailureCause, Scenario, TrialClass, TruthError};

pub use policy::{CoZero, FixedMargin};
pub use ruleset13::Ruleset13;

/// One policy the runner can build afresh for every scenario and seed. Each variant carries the
/// parameters its construction needs, so a run is reproducible from its `PolicyKind` alone.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PolicyKind {
    /// Diagnostic: stock CO forever, no night experiments.
    CoZero,
    /// Diagnostic: observed night edge minus the calendar's fixed margin.
    FixedMargin,
    /// The production Ruleset 13 policy; `fallback_slope` is the per-count hazard multiplier.
    Ruleset13 { fallback_slope: f64 },
}

/// Whether a policy's results may pass the registered gate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyRole {
    /// A comparator: its scores are diagnostics and can never pass the gate.
    Diagnostic,
    /// The policy under test: a failing score fails the gate.
    Gate,
}

/// A policy name or slope the registry refuses.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum PolicyError {
    #[error("unknown day/night policy {name:?}; known policies: {known}")]
    Unknown { name: String, known: String },
    #[error("policy {0} takes no fallback slope")]
    SlopeUnused(&'static str),
    #[error("policy {0} needs a fallback slope")]
    SlopeRequired(&'static str),
}

impl PolicyKind {
    /// Every diagnostic comparator, in report order. A policy under test is not listed: it needs
    /// parameters, so it is only reachable through [`PolicyKind::by_label`].
    pub const COMPARATORS: [Self; 2] = [Self::CoZero, Self::FixedMargin];

    /// The stable label records and reports carry.
    pub fn label(self) -> &'static str {
        match self {
            Self::CoZero => "co-zero",
            Self::FixedMargin => "fixed-margin",
            Self::Ruleset13 { .. } => "ruleset13",
        }
    }

    pub fn role(self) -> PolicyRole {
        match self {
            Self::CoZero | Self::FixedMargin => PolicyRole::Diagnostic,
            Self::Ruleset13 { .. } => PolicyRole::Gate,
        }
    }

    /// The fallback slope this policy was built with, when it takes one.
    pub fn fallback_slope(self) -> Option<f64> {
        match self {
            Self::CoZero | Self::FixedMargin => None,
            Self::Ruleset13 { fallback_slope } => Some(fallback_slope),
        }
    }

    /// Resolves a label and the optional `--slope` to a policy.
    ///
    /// # Errors
    /// Refuses an unknown label, and a slope for a policy that takes none.
    pub fn by_label(label: &str, fallback_slope: Option<f64>) -> Result<Self, PolicyError> {
        if label == "ruleset13" {
            return fallback_slope
                .map(|fallback_slope| Self::Ruleset13 { fallback_slope })
                .ok_or(PolicyError::SlopeRequired("ruleset13"));
        }
        let kind = Self::COMPARATORS
            .into_iter()
            .find(|kind| kind.label() == label)
            .ok_or_else(|| PolicyError::Unknown {
                name: label.to_owned(),
                known: Self::COMPARATORS
                    .map(Self::label)
                    .into_iter()
                    .chain(["ruleset13"])
                    .collect::<Vec<_>>()
                    .join(", "),
            })?;
        match fallback_slope {
            Some(_) => Err(PolicyError::SlopeUnused(kind.label())),
            None => Ok(kind),
        }
    }

    /// Builds a fresh policy for one run; policies keep per-run state and are never reused.
    ///
    /// # Errors
    /// Refuses parameters the policy cannot run with.
    pub fn build(self, calendar: &CalendarConfig) -> Result<Box<dyn Policy>, RunError> {
        Ok(match self {
            Self::CoZero => Box::new(CoZero::default()),
            Self::FixedMargin => {
                Box::new(FixedMargin::new(calendar.fixed_margin, calendar.day_step)?)
            }
            Self::Ruleset13 { fallback_slope } => Box::new(Ruleset13::new(fallback_slope)?),
        })
    }
}

/// Calendar day zero is Monday. Hours not spent active or testing are sleep, not exposure.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CalendarConfig {
    pub days: u32,
    pub year_days: u32,
    pub lambda_per_hour: f64,
    pub active_hours: f64,
    pub night_hours: f64,
    pub night_weekdays: Vec<u8>,
    /// Overrides the scenario's firmware transition, when supplied.
    pub firmware_change_day: Option<u32>,
    pub fixed_margin: u8,
    pub day_step: u8,
    /// Overrides the scenario's fractions without changing its workload classes.
    pub daily_mix: Option<Vec<f64>>,
}

impl Default for CalendarConfig {
    fn default() -> Self {
        Self {
            days: 730,
            year_days: 365,
            lambda_per_hour: 1.0 / 3000.0,
            active_hours: 8.0,
            night_hours: 8.0,
            night_weekdays: vec![0, 2, 4],
            firmware_change_day: None,
            fixed_margin: 4,
            day_step: 8,
            daily_mix: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RunError {
    #[error("validate the day/night calendar: {0}")]
    Calendar(&'static str),
    #[error("evaluate the day/night truth")]
    Truth(#[from] TruthError),
    #[error("operate the simulated machine")]
    Machine(#[from] togi_machine::Error),
    #[error("construct the simulated host")]
    Model(#[from] togi_sim::InvalidMachine),
    #[error("advance the simulated host")]
    Clock(#[from] togi_sim::ArithmeticError),
    #[error("validate the night trial's placement")]
    Roles(#[from] togi_machine::InvalidRoles),
    #[error("fallback slope {0} must be a finite per-count hazard multiplier above one")]
    Slope(f64),
    #[error("the Ruleset 13 policy never settled: {0}")]
    Unsettled(&'static str),
    #[error("fold a journal event into the Ruleset 13 state")]
    Fold(#[from] togi_tuner::policy::FoldError),
    #[error("query the Ruleset 13 state")]
    NotReady(#[from] togi_tuner::policy::NotReady),
    #[error("build a Ruleset 13 trial")]
    Plan(#[from] togi_tuner::policy::PlanError),
    #[error("build a day profile")]
    Profile(#[from] togi_machine::CcdProfileError),
    #[error("policy requested more trial time than the night's allocation")]
    NightOverrun,
    #[error("the successor invocation execution budget expired")]
    BudgetExpired,
}

/// What the operator answered, not the hidden cause of an interruption.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndAnswer {
    Cpu,
    Other,
}

/// Evidence deliberately has no oracle cause, hazard, safe offset, or scenario identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DayObservation {
    Completed {
        active: Duration,
    },
    Ended {
        active: Duration,
        answer: Option<EndAnswer>,
    },
}

/// The classification the policy actually made, independently of the hidden cause.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DayClassification {
    CpuSuspect,
    NotCpuSuspect,
}

impl DayObservation {
    /// The diagnostic comparators' conservative answer handling; another policy may disagree.
    pub fn conservative_classification(&self) -> DayClassification {
        match self {
            Self::Completed { .. } => DayClassification::NotCpuSuspect,
            Self::Ended {
                answer: Some(EndAnswer::Other),
                ..
            } => DayClassification::NotCpuSuspect,
            Self::Ended {
                answer: Some(EndAnswer::Cpu) | None,
                ..
            } => DayClassification::CpuSuspect,
        }
    }
}

/// Public night requests contain only ordinary machine trial descriptions.
pub struct Night {
    pub day: u32,
    pub budget: Duration,
    pub trials: Vec<TrialSpec>,
}

/// A policy can inspect machine seams and event evidence, but cannot inspect the truth model.
/// Production Ruleset 13 can supply an adapter here without copying its decision rules.
pub trait Policy {
    fn firmware_changed(&mut self, machine: &mut Machine<'_>) -> Result<(), RunError>;
    fn start_day(&mut self, day: u32, machine: &mut Machine<'_>) -> Result<(), RunError>;
    fn observe_day(
        &mut self,
        day: u32,
        evidence: &DayObservation,
        machine: &mut Machine<'_>,
    ) -> Result<DayClassification, RunError>;
    fn night(&mut self, night: &Night, machine: &mut Machine<'_>) -> Result<(), RunError>;

    /// Observations owned by the policy, never simulator truth. A legacy policy does not
    /// acquire a successor finish or machine learning allowance merely by being observed.
    fn report(&self) -> successor::PolicyReport {
        successor::PolicyReport::unavailable()
    }

    /// Policy-owned application and qualification witnesses; the successor runner checks
    /// them against the actual machine trajectory rather than inventing permissions.
    fn applications(&self) -> successor::PolicyApplications {
        successor::PolicyApplications::default()
    }

    /// Whether a caller-selected journal boundary ended this run during its last night.
    fn is_finished(&self) -> bool {
        false
    }
}

/// Per-CCD vectors always retain every CCD and every seed, including censored runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    pub convergence_days: Vec<Option<u32>>,
    pub max_overshoot: Vec<u8>,
    pub promotions: Vec<u32>,
    pub daily_co_crashes: Vec<u32>,
    pub year_one_co_crashes: Vec<u32>,
    pub climb_co_crashes: Vec<u32>,
    pub cpu_suspect_events: u32,
    /// CO-depth counts lost to background-triggered retreats, not incident counts.
    pub background_retreat_counts: Vec<u32>,
    /// CO-depth counts lost on CCDs actually implicated by a true CO event.
    pub justified_retreat_counts: Vec<u32>,
    /// CO-depth counts lost after hidden graphics or power interruptions.
    pub non_cpu_retreat_counts: Vec<u32>,
    /// CO-depth counts lost on an unaffected CCD or without an interruption.
    pub unattributed_retreat_counts: Vec<u32>,
    pub converged_depth: Vec<Option<u8>>,
    pub final_depth: Vec<u8>,
    pub censored: bool,
    pub fixed_cap: bool,
    pub active_hours: f64,
    pub night_hours: f64,
    pub sleep_hours: f64,
    pub true_co_crashes: u32,
    pub year_one_true_co_crashes: u32,
    pub non_cpu_events: u32,
    pub background_events: u32,
}

impl Metrics {
    fn new(ccds: usize, fixed_cap: bool) -> Self {
        Self {
            convergence_days: vec![None; ccds],
            max_overshoot: vec![0; ccds],
            promotions: vec![0; ccds],
            daily_co_crashes: vec![0; ccds],
            year_one_co_crashes: vec![0; ccds],
            climb_co_crashes: vec![0; ccds],
            cpu_suspect_events: 0,
            background_retreat_counts: vec![0; ccds],
            justified_retreat_counts: vec![0; ccds],
            non_cpu_retreat_counts: vec![0; ccds],
            unattributed_retreat_counts: vec![0; ccds],
            converged_depth: vec![None; ccds],
            final_depth: vec![0; ccds],
            censored: false,
            fixed_cap,
            active_hours: 0.0,
            night_hours: 0.0,
            sleep_hours: 0.0,
            true_co_crashes: 0,
            year_one_true_co_crashes: 0,
            non_cpu_events: 0,
            background_events: 0,
        }
    }
}

/// Runs a registered scenario using a freshly built `policy`.
///
/// # Errors
/// Refuses malformed calendars, unknown scenarios, and failed machine operations.
pub fn run(
    scenario: &str,
    seed: u64,
    policy: PolicyKind,
    calendar: CalendarConfig,
) -> Result<Metrics, RunError> {
    let mut policy = policy.build(&calendar)?;
    run_with_policy(scenario, seed, calendar, policy.as_mut())
}

/// What the calendar does before a day starts, as decided by a caller observing the policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DayStart {
    /// Run the day as the calendar plans it.
    Run,
    /// Change the firmware, as the calendar's own transition does, then run the day.
    ChangeFirmware,
    /// End the run without starting the day.
    Stop,
}

/// Runs any policy using exactly the same calendar and truth-backed machine seams.
///
/// # Errors
/// Refuses malformed calendars, unknown scenarios, and failed machine operations.
pub fn run_with_policy(
    scenario: &str,
    seed: u64,
    calendar: CalendarConfig,
    policy: &mut dyn Policy,
) -> Result<Metrics, RunError> {
    run_controlled(scenario, seed, calendar, policy, |_, _| DayStart::Run)
}

/// Runs `policy` like [`run_with_policy`], asking `start` before each day whether to change the
/// firmware beyond the calendar's own transition, or to end the run early.
pub(crate) fn run_controlled<P: Policy + ?Sized>(
    scenario: &str,
    seed: u64,
    calendar: CalendarConfig,
    policy: &mut P,
    mut start: impl FnMut(u32, &P) -> DayStart,
) -> Result<Metrics, RunError> {
    let scenario = Scenario::builtin(scenario, seed)?;
    validate(&calendar, &scenario)?;
    let transition = calendar.firmware_change_day.or(scenario.transition_day());
    let fixed_cap = !scenario.has_moving_cap() && transition.is_none();
    let mut runtime = machine::Runtime::new(scenario, seed, calendar.lambda_per_hour, transition)?;
    let mut machine = runtime.seams();
    let mut metrics = Metrics::new(runtime.ccd_count(), fixed_cap);
    let fractions = calendar.daily_mix.unwrap_or_else(|| {
        runtime
            .scenario()
            .daily_mix()
            .iter()
            .map(|block| block.fraction)
            .collect()
    });
    let active = Duration::from_secs_f64(calendar.active_hours * 3600.0);
    let fraction_sum = fractions.iter().sum::<f64>();
    let mut remaining = active;
    let daily: Vec<(TrialClass, Duration)> = runtime
        .scenario()
        .daily_mix()
        .iter()
        .zip(&fractions)
        .enumerate()
        .map(|(index, (block, fraction))| {
            let duration = if index + 1 == fractions.len() {
                remaining
            } else {
                active.mul_f64(fraction / fraction_sum).min(remaining)
            };
            remaining -= duration;
            (block.class.clone(), duration)
        })
        .collect();
    let mut previous = runtime.depths()?;
    let mut cap_epoch = runtime.cap_epoch();
    let mut caps = runtime.safe_depths(active, &fractions)?;
    for day in 0..calendar.days {
        let start = start(day, &*policy);
        if start == DayStart::Stop {
            break;
        }
        runtime.set_day(day);
        if start == DayStart::ChangeFirmware {
            runtime.change_firmware_on(day);
        }
        let day_start = runtime.now();
        let epoch = runtime.cap_epoch();
        if epoch != cap_epoch {
            caps = runtime.safe_depths(active, &fractions)?;
            cap_epoch = epoch;
        }
        if transition == Some(day) || start == DayStart::ChangeFirmware {
            runtime.firmware_changed()?;
            policy.firmware_changed(&mut machine)?;
        }
        let before_start = runtime.depths()?;
        policy.start_day(day, &mut machine)?;
        let current = runtime.depths()?;
        record_profile(&mut metrics, &before_start, &current, &caps, day, None);
        for (class, budget) in &daily {
            let mut remaining = *budget;
            while !remaining.is_zero() {
                let event = runtime.active(class, remaining)?;
                let elapsed = event.elapsed;
                remaining = remaining.saturating_sub(elapsed);
                let before = runtime.depths()?;
                let observation = if let Some(cause) = event.cause {
                    match cause {
                        machine::Cause::Truth(FailureCause::Ccd(ccd)) => {
                            record_co(
                                &mut metrics,
                                std::iter::once(ccd.index()),
                                day,
                                calendar.year_days,
                            );
                        }
                        machine::Cause::Truth(FailureCause::Interaction) => {
                            record_co(
                                &mut metrics,
                                0..runtime.ccd_count(),
                                day,
                                calendar.year_days,
                            );
                        }
                        machine::Cause::Truth(FailureCause::Background) => {
                            metrics.background_events += 1;
                        }
                        machine::Cause::Graphics | machine::Cause::Power => {
                            metrics.non_cpu_events += 1;
                        }
                    }
                    DayObservation::Ended {
                        active: elapsed,
                        answer: event.answer,
                    }
                } else {
                    DayObservation::Completed { active: elapsed }
                };
                let classification = policy.observe_day(day, &observation, &mut machine)?;
                if classification == DayClassification::CpuSuspect {
                    metrics.cpu_suspect_events += 1;
                }
                let after = runtime.depths()?;
                record_profile(&mut metrics, &before, &after, &caps, day, event.cause);
                if elapsed.is_zero() {
                    return Err(RunError::Calendar("zero-length active event"));
                }
            }
        }
        let before_night = runtime.depths()?;
        let night_hours = if calendar
            .night_weekdays
            .contains(&u8::try_from(day % 7).map_err(|_| RunError::Calendar("weekday"))?)
        {
            let budget = Duration::from_secs_f64(calendar.night_hours * 3600.0);
            let night = Night {
                day,
                budget,
                trials: runtime.night_specs()?,
            };
            runtime.begin_night(budget);
            policy.night(&night, &mut machine)?;
            let exposure = runtime.end_night().as_secs_f64() / 3600.0;
            metrics.night_hours += exposure;
            exposure
        } else {
            0.0
        };
        let spent = runtime
            .now()
            .checked_sub(day_start)
            .ok_or(RunError::Calendar("monotonic clock moved backwards"))?;
        let remainder = Duration::from_hours(24)
            .checked_sub(spent)
            .ok_or(RunError::Calendar("policy waits overran the calendar day"))?;
        metrics.active_hours += active.as_secs_f64() / 3600.0;
        if !policy.is_finished() {
            runtime.sleep(remainder)?;
            // Includes policy sleeps and unused night time, neither of which is active exposure.
            metrics.sleep_hours += 24.0 - active.as_secs_f64() / 3600.0 - night_hours;
        }
        let final_depth = runtime.depths()?;
        record_profile(&mut metrics, &before_night, &final_depth, &caps, day, None);
        previous = final_depth;
        if policy.is_finished() {
            break;
        }
    }
    metrics.final_depth = previous[..runtime.ccd_count()].to_vec();
    metrics.censored = metrics.convergence_days.iter().any(Option::is_none);
    Ok(metrics)
}

fn record_profile(
    metrics: &mut Metrics,
    before: &[u8],
    after: &[u8],
    caps: &[u8],
    day: u32,
    cause: Option<machine::Cause>,
) {
    for (ccd, &cap) in caps.iter().enumerate() {
        if after[ccd] > before[ccd] {
            metrics.promotions[ccd] += 1;
        }
        let lost = u32::from(before[ccd].saturating_sub(after[ccd]));
        match cause {
            Some(machine::Cause::Truth(FailureCause::Background)) => {
                metrics.background_retreat_counts[ccd] += lost;
            }
            Some(machine::Cause::Truth(FailureCause::Ccd(cause))) if cause.index() == ccd => {
                metrics.justified_retreat_counts[ccd] += lost;
            }
            Some(machine::Cause::Truth(FailureCause::Interaction)) => {
                metrics.justified_retreat_counts[ccd] += lost;
            }
            Some(machine::Cause::Truth(FailureCause::Ccd(_))) | None => {
                metrics.unattributed_retreat_counts[ccd] += lost;
            }
            Some(machine::Cause::Graphics | machine::Cause::Power) => {
                metrics.non_cpu_retreat_counts[ccd] += lost;
            }
        }
        metrics.max_overshoot[ccd] = metrics.max_overshoot[ccd].max(after[ccd].saturating_sub(cap));
        if after[ccd].abs_diff(cap) <= 1 && metrics.convergence_days[ccd].is_none() {
            metrics.convergence_days[ccd] = Some(day + 1);
            metrics.converged_depth[ccd] = Some(after[ccd]);
        }
    }
}

fn record_co(
    metrics: &mut Metrics,
    ccds: impl IntoIterator<Item = usize>,
    day: u32,
    year_days: u32,
) {
    metrics.true_co_crashes += 1;
    if day < year_days {
        metrics.year_one_true_co_crashes += 1;
    }
    let climbing = metrics.convergence_days.iter().any(Option::is_none);
    for ccd in ccds {
        metrics.daily_co_crashes[ccd] += 1;
        if day < year_days {
            metrics.year_one_co_crashes[ccd] += 1;
        }
        if climbing {
            metrics.climb_co_crashes[ccd] += 1;
        }
    }
}

fn validate(calendar: &CalendarConfig, scenario: &Scenario) -> Result<(), RunError> {
    if calendar.year_days == 0
        || !calendar.lambda_per_hour.is_finite()
        || calendar.lambda_per_hour <= 0.0
    {
        return Err(RunError::Calendar(
            "year length and target rate must be positive",
        ));
    }
    if calendar.days == 0 || calendar.days > 36_500 {
        return Err(RunError::Calendar("days must be in 1..=36500"));
    }
    if !calendar.active_hours.is_finite()
        || calendar.active_hours <= 0.0
        || !calendar.night_hours.is_finite()
        || calendar.night_hours < 0.0
        || calendar.active_hours + calendar.night_hours > 24.0
    {
        return Err(RunError::Calendar(
            "active and night hours must fit a 24-hour day",
        ));
    }
    if calendar.night_weekdays.iter().any(|day| *day > 6) {
        return Err(RunError::Calendar("night weekdays must be in 0..=6"));
    }
    if let Some(fractions) = &calendar.daily_mix {
        if fractions.len() != scenario.daily_mix().len()
            || fractions.iter().any(|f| !f.is_finite() || *f < 0.0)
            || (fractions.iter().sum::<f64>() - 1.0).abs() > 1e-12
        {
            return Err(RunError::Calendar(
                "daily mix must give one nonnegative fraction per workload and sum to one",
            ));
        }
    }
    Ok(())
}

pub(super) fn depth(offset: TargetOffset) -> Result<u8, RunError> {
    u8::try_from(-offset.get()).map_err(|_| RunError::Calendar("offset magnitude"))
}

#[cfg(test)]
mod tests;
