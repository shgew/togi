#![expect(
    clippy::float_cmp,
    reason = "record boundaries retain exact coordinates from the same clock"
)]

//! Converts the private machine history into the public, policy-neutral record.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use togi_machine::{CoreId, PerCore};
use togi_sim::day_night::successor as truth;

use super::super::RunError;
use super::machine::{self, Cause, ObservationKind};
use super::run::{PolicyApplications, RunConfig};
use super::{
    Admissibility, Classification, DepthReference, Epoch, Exposure, FirmwareRestoration,
    LearningEvent, MachineEvent, Outcome, PartialCounts, Period, Phase, PolicyReport, PolicyUnit,
    ProfileInterval, ProfileStep, Record, RestorationOutcome, RestorationResult, Retreats,
    RiskAccounts, RiskSource, Runtime, SourceAccount, SourceRisk, StepKind, TruthCause, UnitId,
    UnitMode, UnitPhase, UnitSummary,
};

pub(super) struct Snapshot {
    pub at: Duration,
    pub report: PolicyReport,
    pub applications: PolicyApplications,
}

pub(super) enum Stop {
    BudgetExpired,
    PolicyStopped,
    Harness(String),
}

const DAY_SECONDS: f64 = 86_400.0;

fn day(at: Duration) -> f64 {
    1.0 + at.as_secs_f64() / DAY_SECONDS
}

fn elapsed(day: f64) -> Result<Duration, RunError> {
    Duration::try_from_secs_f64((day - 1.0) * DAY_SECONDS)
        .map_err(|_| RunError::Calendar("record boundary"))
}

fn unit_id(index: usize) -> Result<UnitId, RunError> {
    UnitId::new(u16::try_from(index).map_err(|_| RunError::Calendar("unit count"))?)
        .map_err(|_| RunError::Calendar("unit index"))
}

fn profile(raw: &PerCore<u8>, scenario: &truth::Scenario) -> Result<truth::Profile, RunError> {
    let depths = raw
        .iter()
        .map(|(core, depth)| Ok((core, truth::Depth::new(depth)?)))
        .collect::<Result<PerCore<_>, togi_sim::day_night::TruthError>>()?;
    Ok(truth::Profile::new(depths, scenario.topology())?)
}

fn grouping(
    config: &mut RunConfig,
    scenario: &truth::Scenario,
) -> Result<truth::UnitGrouping, RunError> {
    if config.provenance.topology.is_empty() {
        config.provenance.topology = scenario.topology().to_vec();
    }
    if config.provenance.membership.is_empty() {
        let groups = match config.provenance.unit_mode {
            UnitMode::CcdUniform => truth::UnitGrouping::ccd(scenario.topology())?,
            UnitMode::PerCore => truth::UnitGrouping::cores(scenario.topology())?,
        };
        let coarse = truth::UnitGrouping::ccd(scenario.topology())?;
        config.provenance.membership = groups
            .groups()
            .iter()
            .enumerate()
            .map(|(index, cores)| {
                Ok(PolicyUnit {
                    unit: unit_id(index)?,
                    cores: cores.clone(),
                    kind: match config.provenance.unit_mode {
                        UnitMode::CcdUniform => UnitPhase::Coarse,
                        UnitMode::PerCore => UnitPhase::Core,
                    },
                    parent: match config.provenance.unit_mode {
                        UnitMode::CcdUniform => None,
                        UnitMode::PerCore => Some(unit_id(
                            coarse
                                .groups()
                                .iter()
                                .position(|group| group.contains(&cores[0]))
                                .ok_or(RunError::Calendar("coarse unit membership"))?,
                        )?),
                    },
                })
            })
            .collect::<Result<_, RunError>>()?;
    }
    Ok(truth::UnitGrouping::new(
        scenario.topology(),
        config
            .provenance
            .membership
            .iter()
            .map(|unit| unit.cores.clone())
            .collect(),
    )?)
}

fn references(
    config: &mut RunConfig,
    scenario: &truth::Scenario,
    grouping: &truth::UnitGrouping,
) -> Result<Vec<truth::Reference>, RunError> {
    let active = Duration::try_from_secs_f64(config.calendar.active_hours * 3600.0)
        .map_err(|_| RunError::Calendar("daily reference active hours"))?;
    let fractions = if config.daily.is_empty() {
        config.calendar.daily_mix.clone().unwrap_or_else(|| {
            scenario
                .daily_mix()
                .iter()
                .map(|block| block.fraction)
                .collect()
        })
    } else {
        if config.daily.len() != scenario.daily_mix().len()
            || config
                .daily
                .iter()
                .zip(scenario.daily_mix())
                .any(|((class, _), block)| class != &block.class)
        {
            return Err(RunError::Calendar("ordered reference daily classes"));
        }
        config
            .daily
            .iter()
            .map(|(_, duration)| duration.as_secs_f64() / active.as_secs_f64())
            .collect()
    };
    let calendar = truth::ReferenceCalendar::new(active, fractions)?;
    let mut schedule = Vec::new();
    let mut previous = None;
    for public_day in 1..=config.calendar.days {
        let epoch = scenario.epoch(public_day)?;
        if previous != Some(epoch.index) {
            schedule.push((public_day, epoch));
            previous = Some(epoch.index);
        }
    }
    if !config.epochs.is_empty() && config.epochs.len() != schedule.len() {
        return Err(RunError::Calendar("truth epoch schedule length"));
    }
    let mut epochs = Vec::with_capacity(schedule.len());
    let mut references = Vec::with_capacity(schedule.len());
    for (index, (first_day, truth_epoch)) in schedule.iter().enumerate() {
        let start_day = f64::from(*first_day);
        let end_day = schedule
            .get(index + 1)
            .map_or(f64::from(config.calendar.days) + 1.0, |(next, _)| {
                f64::from(*next)
            });
        let (id, context) = match truth_epoch.kind {
            truth::EpochKind::Initial => ("initial".to_owned(), "ctx-initial".to_owned()),
            truth::EpochKind::Moving => (
                format!("moving-{}", truth_epoch.index),
                "ctx-initial".to_owned(),
            ),
            truth::EpochKind::Firmware => ("firmware-1".to_owned(), "ctx-firmware-1".to_owned()),
        };
        if let Some(supplied) = config.epochs.get(index)
            && (supplied.id != id
                || supplied.context != context
                || supplied.start_day != start_day
                || supplied.end_day != end_day)
        {
            return Err(RunError::Calendar("truth epoch schedule"));
        }
        let reference = truth::Reference::new(
            scenario,
            *first_day,
            grouping.clone(),
            &calendar,
            config.calendar.lambda_per_hour,
            config.tolerance,
        )?;
        let depth = reference.depth_reference()?;
        epochs.push(Epoch {
            id,
            start_day,
            end_day,
            context,
            reference: DepthReference {
                operational_max: depth.operational.value,
                certified_upper_bound: depth.operational.upper_bound,
                physical_max: depth.physical.value,
                per_unit_max_feasible: depth.unit_maxima,
                tolerance: config.tolerance,
                certificate: format!(
                    "exact-monotone-v1:cap={};states={};components={}",
                    depth.operational.cap, depth.operational.states, depth.operational.components
                ),
            },
        });
        references.push(reference);
    }
    config.epochs = epochs;
    Ok(references)
}

fn applications(snapshots: &[Snapshot]) -> (Vec<ProfileStep>, Vec<FirmwareRestoration>) {
    let mut steps = BTreeMap::new();
    let mut restorations = BTreeMap::new();
    for snapshot in snapshots {
        for step in &snapshot.applications.steps {
            steps.insert(step.id.clone(), step.clone());
        }
        for restoration in &snapshot.applications.restorations {
            restorations.insert(restoration.id.clone(), restoration.clone());
        }
    }
    let mut steps: Vec<_> = steps.into_values().collect();
    steps.sort_by(|a, b| a.day.total_cmp(&b.day).then_with(|| a.id.cmp(&b.id)));
    (steps, restorations.into_values().collect())
}

fn firmware(
    config: &RunConfig,
    scenario: &truth::Scenario,
    observations: &[machine::Observation],
    observed: Duration,
    stock: &PerCore<u8>,
    steps: &mut Vec<ProfileStep>,
    restorations: &mut Vec<FirmwareRestoration>,
) -> Result<(), RunError> {
    for epoch in &config.epochs {
        let at = elapsed(epoch.start_day)?;
        let public_day = u32::try_from(at.as_secs() / 86_400 + 1)
            .map_err(|_| RunError::Calendar("firmware day"))?;
        if at >= observed || scenario.epoch(public_day)?.kind != truth::EpochKind::Firmware {
            continue;
        }
        let previous = observations
            .iter()
            .take_while(|observation| observation.before < at)
            .filter(|observation| !observation.night)
            .last()
            .map_or(*stock, |observation| observation.profile);
        let reset = observations.iter().find(|observation| {
            observation.before == at
                && matches!(observation.kind, ObservationKind::ProfileChange)
                && observation.profile.iter().all(|(_, depth)| depth == 0)
        });
        let readback = reset.map_or_else(
            || {
                observations
                    .iter()
                    .find(|observation| observation.before >= at)
                    .map_or(*stock, |observation| observation.profile)
            },
            |observation| observation.profile,
        );
        if readback.iter().any(|(_, depth)| depth != 0) {
            return Err(RunError::Calendar("firmware stock reset readback"));
        }
        if !steps.iter().any(|step| {
            matches!(&step.kind,
            StepKind::StockReset { epoch: id } if id == &epoch.id)
        }) {
            steps.push(ProfileStep {
                id: format!("runtime-stock-reset-{}", epoch.id),
                day: epoch.start_day,
                before: previous,
                after: readback,
                kind: StepKind::StockReset {
                    epoch: epoch.id.clone(),
                },
            });
        }
        if let Some(restoration) = restorations.iter_mut().find(|item| item.epoch == epoch.id) {
            restoration.previous_profile = previous;
            restoration.stock_reset_day = epoch.start_day;
            restoration.stock_readback = readback;
            restoration
                .membership
                .clone_from(&config.provenance.membership);
        } else {
            restorations.push(FirmwareRestoration {
                id: format!("runtime-restoration-{}", epoch.id),
                epoch: epoch.id.clone(),
                transition_day: epoch.start_day,
                previous_profile: previous,
                membership: config.provenance.membership.clone(),
                stock_reset_day: epoch.start_day,
                stock_readback: readback,
                outcome: RestorationOutcome::Blocked {
                    reason: "policy supplied no qualified whole-profile restoration".to_owned(),
                },
            });
        }
    }
    steps.sort_by(|a, b| a.day.total_cmp(&b.day).then_with(|| a.id.cmp(&b.id)));
    restorations.sort_by(|a, b| a.transition_day.total_cmp(&b.transition_day));
    Ok(())
}

fn restoration_at(
    restorations: &[FirmwareRestoration],
    at: f64,
    closing_event: Option<&str>,
) -> Option<String> {
    restorations
        .iter()
        .rev()
        .find(|item| {
            if at < item.transition_day {
                return false;
            }
            match &item.outcome {
                RestorationOutcome::Blocked { .. } => false,
                RestorationOutcome::Applied { day, result, .. } => {
                    if at < *day {
                        return false;
                    }
                    match result {
                        RestorationResult::Retained {} => true,
                        RestorationResult::Crashed {
                            event,
                            stock_step_back_day,
                            ..
                        } => {
                            at < *stock_step_back_day
                                || (at == *stock_step_back_day
                                    && closing_event == Some(event.as_str()))
                        }
                    }
                }
            }
        })
        .map(|item| item.id.clone())
}

fn core_unit(membership: &[PolicyUnit], core: CoreId) -> Result<UnitId, RunError> {
    membership
        .iter()
        .find(|unit| unit.cores.contains(&core))
        .map(|unit| unit.unit)
        .ok_or(RunError::Calendar("truth source unit membership"))
}

fn interaction(source: truth::InteractionId) -> RiskSource {
    let (a, b) = source.pair();
    RiskSource::Interaction {
        identity: format!("cores-{}-{}", a.get(), b.get()),
    }
}

fn truth_cause(cause: Cause, membership: &[PolicyUnit]) -> Result<TruthCause, RunError> {
    Ok(match cause {
        Cause::Truth(truth::FailureCause::Core(core)) => {
            let unit = core_unit(membership, core)?;
            TruthCause::Co {
                source: RiskSource::Unit { unit },
                implicated_units: vec![unit],
            }
        }
        Cause::Truth(truth::FailureCause::Interaction(source)) => {
            let (a, b) = source.pair();
            let mut units = vec![core_unit(membership, a)?, core_unit(membership, b)?];
            units.sort_unstable();
            units.dedup();
            TruthCause::Co {
                source: interaction(source),
                implicated_units: units,
            }
        }
        Cause::Truth(truth::FailureCause::Background) => TruthCause::Background {},
        Cause::Graphics | Cause::Power => TruthCause::NonCpu {},
    })
}

fn first_finish(report: &PolicyReport, membership: &[PolicyUnit]) -> Option<f64> {
    membership
        .iter()
        .map(|unit| {
            report
                .finishes
                .iter()
                .find(|finish| finish.unit == unit.unit)
                .map(|finish| finish.day)
        })
        .try_fold(1.0_f64, |day, next| next.map(|next| day.max(next)))
}

fn events(
    observations: &[machine::Observation],
    snapshots: &[Snapshot],
    report: &mut PolicyReport,
    membership: &[PolicyUnit],
    restorations: &[FirmwareRestoration],
    finish: Option<f64>,
) -> Result<Vec<MachineEvent>, RunError> {
    let daily: Vec<_> = observations
        .iter()
        .enumerate()
        .filter_map(|(index, observation)| {
            matches!(
                observation.kind,
                ObservationKind::Active { cause: Some(_), .. }
            )
            .then_some(index)
        })
        .collect();
    let mut event_indices = BTreeMap::new();
    let mut policy_events = BTreeMap::new();
    let mut assigned = BTreeSet::new();
    for snapshot in snapshots {
        for event in &snapshot.report.events {
            if !event_indices.contains_key(&event.id) {
                let pending = || {
                    daily.iter().copied().filter(|index| {
                        !assigned.contains(index) && observations[*index].after <= snapshot.at
                    })
                };
                let index = pending()
                    .find(|index| day(observations[*index].after) == event.day)
                    .or_else(|| pending().find(|index| !observations[*index].night))
                    .ok_or(RunError::Calendar(
                        "policy event lacks observed interruption",
                    ))?;
                event_indices.insert(event.id.clone(), index);
                assigned.insert(index);
            }
            policy_events.insert(event.id.clone(), event.clone());
        }
    }
    let mut result = Vec::new();
    for (index, observation) in observations.iter().enumerate() {
        let ObservationKind::Active {
            cause: Some(cause), ..
        } = observation.kind
        else {
            continue;
        };
        let at = day(observation.after);
        let policy_id = event_indices
            .iter()
            .find_map(|(id, observed)| (*observed == index).then_some(id));
        let mut event = policy_id
            .and_then(|id| policy_events.get(id))
            .cloned()
            .unwrap_or_else(|| LearningEvent {
                id: format!("runtime-interruption-{index}"),
                day: at,
                classification: Classification::Unreported,
                provisional: false,
                reclassified_from: None,
                charged: false,
                phase: Phase::Learning,
                unit_phase: membership
                    .first()
                    .map_or(UnitPhase::Coarse, |unit| unit.kind),
                newest_step: None,
            });
        event.day = at;
        let triggers_finish = report
            .finishes
            .iter()
            .any(|finish| finish.day == at && finish.witnesses.contains(&event.id));
        event.phase = if finish.is_some_and(|finish| at >= finish) && !triggers_finish {
            Phase::PostFinish
        } else {
            Phase::Learning
        };
        let restoration_learning = restoration_at(restorations, at, Some(&event.id));
        result.push(MachineEvent {
            phase: event.phase,
            policy_event: event,
            period: if observation.night {
                Period::Night
            } else {
                Period::Day
            },
            truth: truth_cause(cause, membership)?,
            restoration_learning,
            profile: observation.profile,
        });
    }
    report.events = result
        .iter()
        .filter(|event| {
            event.period == Period::Day
                && event.policy_event.classification != Classification::Unreported
        })
        .map(|event| event.policy_event.clone())
        .collect();
    Ok(result)
}

fn add_exposure(
    sources: &mut Vec<SourceRisk>,
    exposure: &truth::Exposure,
    membership: &[PolicyUnit],
) -> Result<(), RunError> {
    for (core, expected) in exposure.co_per_core.iter() {
        let unit = core_unit(membership, core)?;
        let source = sources
            .iter_mut()
            .find(|source| source.source == RiskSource::Unit { unit })
            .ok_or(RunError::Calendar("interval unit risk source"))?;
        source.expected_failures += expected;
    }
    for item in &exposure.interactions {
        let source = interaction(item.source);
        if let Some(entry) = sources.iter_mut().find(|entry| entry.source == source) {
            entry.expected_failures += item.expected_failures;
        } else {
            sources.push(SourceRisk {
                source,
                expected_failures: item.expected_failures,
            });
        }
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "trajectory splitting combines independent run artifact ledgers"
)]
fn intervals(
    config: &RunConfig,
    scenario: &truth::Scenario,
    references: &[truth::Reference],
    observations: &[machine::Observation],
    observed: Duration,
    finish: Option<f64>,
    events: &[MachineEvent],
    steps: &[ProfileStep],
    restorations: &[FirmwareRestoration],
    stock: PerCore<u8>,
) -> Result<Vec<ProfileInterval>, RunError> {
    let until = day(observed);
    let mut boundaries = vec![1.0, until];
    boundaries.extend(config.epochs.iter().map(|epoch| epoch.start_day));
    let mut previous_period = Period::Day;
    for observation in observations {
        let period = if observation.night {
            Period::Night
        } else {
            Period::Day
        };
        if period != previous_period || matches!(observation.kind, ObservationKind::ProfileChange) {
            boundaries.push(day(observation.before));
        }
        previous_period = period;
    }
    boundaries.extend(finish);
    boundaries.extend(events.iter().map(|event| event.policy_event.day));
    boundaries.extend(steps.iter().map(|step| step.day));
    for restoration in restorations {
        boundaries.push(restoration.transition_day);
        if let RestorationOutcome::Applied { day, result, .. } = &restoration.outcome {
            boundaries.push(*day);
            if let RestorationResult::Crashed {
                stock_step_back_day,
                ..
            } = result
            {
                boundaries.push(*stock_step_back_day);
            }
        }
    }
    boundaries.retain(|boundary| *boundary >= 1.0 && *boundary <= until);
    boundaries.sort_by(f64::total_cmp);
    boundaries.dedup();
    let mut cursor = 0;
    let mut current = stock;
    let mut period = Period::Day;
    let mut result = Vec::with_capacity(boundaries.len().saturating_sub(1));
    for pair in boundaries.windows(2) {
        let start = pair[0];
        let end = pair[1];
        while let Some(observation) = observations.get(cursor)
            && day(observation.before) <= start
        {
            current = observation.profile;
            period = if observation.night {
                Period::Night
            } else {
                Period::Day
            };
            cursor += 1;
        }
        let epoch = config
            .epochs
            .iter()
            .position(|epoch| epoch.start_day <= start && start < epoch.end_day)
            .ok_or(RunError::Calendar("deployed interval epoch"))?;
        let admissibility = references[epoch].admissibility(&profile(&current, scenario)?)?;
        let member = admissibility.joint_ratio <= 1.0
            && admissibility
                .per_unit_ratios
                .iter()
                .all(|ratio| *ratio <= 1.0);
        result.push(ProfileInterval {
            start_day: start,
            end_day: end,
            profile: current,
            period,
            membership: config.provenance.membership.clone(),
            epoch: config.epochs[epoch].id.clone(),
            verified_daily_active_hours: 0.0,
            phase: if finish.is_some_and(|finish| start >= finish) {
                Phase::PostFinish
            } else {
                Phase::Learning
            },
            restoration_learning: restoration_at(restorations, start, None),
            admissibility: Admissibility {
                joint_ratio: admissibility.joint_ratio,
                unit_ratios: admissibility.per_unit_ratios,
                member,
            },
            sources: config
                .provenance
                .membership
                .iter()
                .map(|unit| SourceRisk {
                    source: RiskSource::Unit { unit: unit.unit },
                    expected_failures: 0.0,
                })
                .collect(),
        });
    }
    // Public fractional days lose nanosecond precision. Retain the actual clock coordinate
    // at observed boundaries instead of rounding it back through the public representation.
    let clock: BTreeMap<_, _> = observations
        .iter()
        .flat_map(|observation| [observation.before, observation.after])
        .chain([Duration::ZERO, observed])
        .map(|at| (day(at).to_bits(), at))
        .collect();
    let ends = boundaries
        .iter()
        .skip(1)
        .map(|boundary| {
            clock
                .get(&boundary.to_bits())
                .copied()
                .map_or_else(|| elapsed(*boundary), Ok)
        })
        .collect::<Result<Vec<_>, _>>()?;
    integrate(&mut result, &ends, observations, config, scenario)?;
    Ok(result)
}

fn integrate(
    intervals: &mut [ProfileInterval],
    ends: &[Duration],
    observations: &[machine::Observation],
    config: &RunConfig,
    scenario: &truth::Scenario,
) -> Result<(), RunError> {
    let mut state = truth::TruthState::cold(scenario.topology())?;
    let mut cursor = 0;
    for observation in observations {
        if matches!(observation.kind, ObservationKind::ProfileChange) {
            state.interrupt();
            continue;
        }
        if matches!(
            observation.kind,
            ObservationKind::Active { launch: true, .. }
        ) {
            state.interrupt();
        }
        let exact = profile(&observation.profile, scenario)?;
        let mut before = observation.before;
        while before < observation.after {
            while ends.get(cursor).is_some_and(|end| *end <= before) {
                cursor += 1;
            }
            let interval = intervals
                .get_mut(cursor)
                .ok_or(RunError::Calendar("observation interval coverage"))?;
            let after = ends[cursor].min(observation.after);
            let duration = after
                .checked_sub(before)
                .filter(|duration| !duration.is_zero())
                .ok_or(RunError::Calendar("observation boundary precision"))?;
            match &observation.kind {
                ObservationKind::Active {
                    class, exposure, ..
                } => {
                    let full = before == observation.before && after == observation.after;
                    let split;
                    let actual = if full {
                        exposure
                    } else {
                        let public_day = u32::try_from(before.as_secs() / 86_400 + 1)
                            .map_err(|_| RunError::Calendar("observed day"))?;
                        split = scenario.evaluate(class, &exact, &state, duration, public_day)?;
                        &split
                    };
                    if !observation.night {
                        interval.verified_daily_active_hours += duration.as_secs_f64() / 3600.0;
                    }
                    add_exposure(&mut interval.sources, actual, &config.provenance.membership)?;
                    state.advance_profile(Some(class), &exact, duration)?;
                }
                ObservationKind::Sleep => state.advance_profile(None, &exact, duration)?,
                ObservationKind::ProfileChange => {}
            }
            before = after;
        }
        if matches!(
            observation.kind,
            ObservationKind::Active { cause: Some(_), .. }
        ) {
            state.interrupt();
        }
    }
    Ok(())
}

fn accounts(
    phase: Phase,
    membership: &[PolicyUnit],
    intervals: &[ProfileInterval],
    events: &[MachineEvent],
) -> Result<Vec<SourceAccount>, RunError> {
    let mut sources: Vec<_> = membership
        .iter()
        .map(|unit| RiskSource::Unit { unit: unit.unit })
        .collect();
    for source in intervals
        .iter()
        .flat_map(|interval| &interval.sources)
        .map(|source| &source.source)
        .chain(events.iter().filter_map(|event| match &event.truth {
            TruthCause::Co { source, .. } => Some(source),
            TruthCause::Background {} | TruthCause::NonCpu {} | TruthCause::Unattributed {} => None,
        }))
    {
        if !sources.contains(source) {
            sources.push(source.clone());
        }
    }
    let active_hours = intervals
        .iter()
        .filter(|interval| interval.period == Period::Day && interval.phase == phase)
        .map(|interval| interval.verified_daily_active_hours)
        .sum();
    sources.into_iter().map(|source| {
        let k = events.iter().filter(|event| event.period == Period::Day && event.phase == phase
            && matches!(&event.truth, TruthCause::Co { source: actual, .. } if actual == &source)).count();
        let expected_failures = intervals.iter().filter(|interval| interval.period == Period::Day && interval.phase == phase)
            .flat_map(|interval| &interval.sources).filter(|entry| entry.source == source)
            .map(|entry| entry.expected_failures).sum();
        Ok(SourceAccount { source, k: u64::try_from(k).map_err(|_| RunError::Calendar("event count"))?,
            active_hours, expected_failures })
    }).collect()
}

fn unit_depth(raw: &PerCore<u8>, unit: &PolicyUnit) -> Result<u8, RunError> {
    unit.cores
        .first()
        .and_then(|core| raw.get(*core))
        .ok_or(RunError::Calendar("summary unit profile"))
}

#[expect(
    clippy::too_many_arguments,
    reason = "unit summaries join observed costs and policy-owned application evidence"
)]
fn summaries(
    membership: &[PolicyUnit],
    report: &PolicyReport,
    observations: &[machine::Observation],
    intervals: &[ProfileInterval],
    events: &[MachineEvent],
    steps: &[ProfileStep],
    restorations: &[FirmwareRestoration],
    stock: PerCore<u8>,
) -> Result<Vec<UnitSummary>, RunError> {
    membership
        .iter()
        .map(|unit| {
            let mut summary = UnitSummary {
                unit: unit.unit,
                finish: report
                    .finishes
                    .iter()
                    .find(|finish| finish.unit == unit.unit)
                    .cloned(),
                promotions: 0,
                retreats: Retreats {
                    co_counts: 0,
                    background_counts: 0,
                    non_cpu_counts: 0,
                    unattributed_counts: 0,
                    steps: 0,
                },
                hold_hours: intervals
                    .iter()
                    .map(|interval| interval.verified_daily_active_hours)
                    .sum(),
                qualification_launches: 0,
                qualification_hours: 0.0,
                night_hours: 0.0,
                ceiling_evidence: Vec::new(),
            };
            if report.allowance.is_some() {
                // A whole-profile application can produce many zero-time per-core writes.
                // Count the policy's application once, not one matching intermediate vector.
                for step in steps {
                    match &step.kind {
                        StepKind::Climb { unit: owner, .. } if owner == &unit.unit => {
                            summary.promotions += 1;
                        }
                        StepKind::Retreat { units, event, .. } if units.contains(&unit.unit) => {
                            record_retreat(
                                &mut summary,
                                events,
                                event.as_deref(),
                                step.day,
                                unit_depth(&step.before, unit)?,
                                unit_depth(&step.after, unit)?,
                            );
                        }
                        StepKind::Climb { .. }
                        | StepKind::Retreat { .. }
                        | StepKind::Restoration { .. }
                        | StepKind::StockReset { .. } => {}
                    }
                }
            }
            let mut previous = stock;
            for observation in observations {
                if report.allowance.is_none()
                    && !observation.night
                    && matches!(observation.kind, ObservationKind::ProfileChange)
                {
                    let before = unit_depth(&previous, unit)?;
                    let after = unit_depth(&observation.profile, unit)?;
                    if after > before {
                        summary.promotions += 1;
                    }
                    if after < before {
                        record_retreat(
                            &mut summary,
                            events,
                            None,
                            day(observation.before),
                            before,
                            after,
                        );
                    }
                }
                if !observation.night || report.allowance.is_some() {
                    previous = observation.profile;
                }
                if observation.night
                    && let ObservationKind::Active {
                        class,
                        seconds,
                        trial_id,
                        launch,
                        ..
                    } = &observation.kind
                {
                    let loaded = class.loaded();
                    if !unit.cores.iter().any(|core| loaded.contains(*core)) {
                        continue;
                    }
                    summary.night_hours += seconds.observed.as_secs_f64() / 3600.0;
                    let qualified = trial_id.as_ref().is_some_and(|id| {
                        steps.iter().any(|step| {
                            let qualification = match &step.kind {
                                StepKind::Climb {
                                    unit: owner,
                                    qualification,
                                    ..
                                } if owner == &unit.unit => Some(qualification),
                                StepKind::Climb { .. }
                                | StepKind::Restoration { .. }
                                | StepKind::Retreat { .. }
                                | StepKind::StockReset { .. } => None,
                            };
                            qualification.is_some_and(|qualification| {
                                qualification.fresh_witnesses.contains(id)
                                    || qualification.reused_witnesses.contains(id)
                                    || qualification.stock_controls.contains(id)
                            })
                        }) || restorations
                            .iter()
                            .any(|restoration| match &restoration.outcome {
                                RestorationOutcome::Blocked { .. } => false,
                                RestorationOutcome::Applied { qualification, .. } => {
                                    qualification.fresh_witnesses.contains(id)
                                        || qualification.reused_witnesses.contains(id)
                                        || qualification.stock_controls.contains(id)
                                }
                            })
                    });
                    if qualified {
                        summary.qualification_launches += u64::from(*launch);
                        summary.qualification_hours += seconds.observed.as_secs_f64() / 3600.0;
                    }
                }
            }
            Ok(summary)
        })
        .collect()
}

fn record_retreat(
    summary: &mut UnitSummary,
    events: &[MachineEvent],
    named_event: Option<&str>,
    at: f64,
    before: u8,
    after: u8,
) {
    summary.retreats.steps += 1;
    let counts = u64::from(before.saturating_sub(after));
    let cause = events
        .iter()
        .rev()
        .find(|event| {
            named_event.map_or_else(
                || event.policy_event.day == at,
                |id| event.policy_event.id == id,
            )
        })
        .map(|event| &event.truth);
    match cause {
        Some(TruthCause::Co { .. }) => summary.retreats.co_counts += counts,
        Some(TruthCause::Background {}) => summary.retreats.background_counts += counts,
        Some(TruthCause::NonCpu {}) => summary.retreats.non_cpu_counts += counts,
        Some(TruthCause::Unattributed {}) | None => summary.retreats.unattributed_counts += counts,
    }
}

fn converged(
    intervals: &[ProfileInterval],
    epochs: &[Epoch],
    membership: &[PolicyUnit],
    scenario: &str,
) -> Result<bool, RunError> {
    let Some(last) = intervals
        .iter()
        .rev()
        .find(|interval| interval.period == Period::Day)
    else {
        return Ok(false);
    };
    if last.phase != Phase::PostFinish || !last.admissibility.member {
        return Ok(false);
    }
    let benign = matches!(
        scenario,
        "successor-edge-deep"
            | "successor-no-write-limit-failure"
            | "successor-within-ccd-heterogeneous"
    );
    if !benign {
        return Ok(true);
    }
    let reference = &epochs
        .iter()
        .find(|epoch| epoch.id == last.epoch)
        .ok_or(RunError::Calendar("final reference epoch"))?
        .reference;
    let depths = membership
        .iter()
        .map(|unit| unit_depth(&last.profile, unit))
        .collect::<Result<Vec<_>, _>>()?;
    let total: u32 = depths.iter().map(|depth| u32::from(*depth)).sum();
    let required = (4 * reference.operational_max).div_ceil(5);
    Ok(total >= required
        && depths
            .iter()
            .zip(&reference.per_unit_max_feasible)
            .all(|(depth, maximum)| {
                *depth
                    >= 5.min(*maximum)
                        .max(if scenario == "successor-no-write-limit-failure" {
                            44
                        } else {
                            0
                        })
            }))
}

/// Keeps policy evidence separate from the metric-side truth ledger. Empty ledgers and finishes
/// remain absent; only structural membership/readback and observed machine history are supplied.
#[expect(
    clippy::needless_pass_by_value,
    reason = "the assembly seam consumes the completed run artifacts"
)]
pub(super) fn assemble(
    mut config: RunConfig,
    scenario: &truth::Scenario,
    observations: Vec<machine::Observation>,
    snapshots: Vec<Snapshot>,
    observed: Duration,
    stop: Option<Stop>,
    wall_seconds: f64,
) -> Result<Record, RunError> {
    let grouping = grouping(&mut config, scenario)?;
    let references = references(&mut config, scenario, &grouping)?;
    let stock: PerCore<_> = scenario
        .topology()
        .iter()
        .map(|core| (core.core, 0))
        .collect();
    let mut report = snapshots
        .last()
        .ok_or(RunError::Calendar("final policy snapshot"))?
        .report
        .clone();
    if report.unit_membership.is_empty() {
        report
            .unit_membership
            .clone_from(&config.provenance.membership);
    }
    if report.readback.is_none() {
        report.readback = Some(
            observations
                .last()
                .map_or(stock, |observation| observation.profile),
        );
    }
    // A firmware stop must not replace the original first-finish boundary.
    let mut originals = BTreeMap::new();
    for snapshot in &snapshots {
        for finish in &snapshot.report.finishes {
            originals
                .entry(finish.unit)
                .or_insert_with(|| finish.clone());
        }
    }
    report.finishes = originals.into_values().collect();
    let finish = first_finish(&report, &config.provenance.membership);
    let (mut steps, mut restorations) = applications(&snapshots);
    firmware(
        &config,
        scenario,
        &observations,
        observed,
        &stock,
        &mut steps,
        &mut restorations,
    )?;
    let events = events(
        &observations,
        &snapshots,
        &mut report,
        &config.provenance.membership,
        &restorations,
        finish,
    )?;
    let intervals = intervals(
        &config,
        scenario,
        &references,
        &observations,
        observed,
        finish,
        &events,
        &steps,
        &restorations,
        stock,
    )?;
    let mut units = summaries(
        &config.provenance.membership,
        &report,
        &observations,
        &intervals,
        &events,
        &steps,
        &restorations,
        stock,
    )?;
    for snapshot in &snapshots {
        for (unit, evidence) in &snapshot.applications.ceilings {
            let summary = units
                .iter_mut()
                .find(|summary| summary.unit == *unit)
                .ok_or(RunError::Calendar("ceiling evidence unit"))?;
            if !summary.ceiling_evidence.contains(evidence) {
                summary.ceiling_evidence.push(evidence.clone());
            }
        }
    }
    let accounts = RiskAccounts {
        learning: accounts(
            Phase::Learning,
            &config.provenance.membership,
            &intervals,
            &events,
        )?,
        post_finish: accounts(
            Phase::PostFinish,
            &config.provenance.membership,
            &intervals,
            &events,
        )?,
    };
    let mut exposure = Exposure {
        intended_seconds: 0.0,
        observed_seconds: 0.0,
        eligible_seconds: 0.0,
        onset_active_seconds: 0.0,
    };
    let mut launches = 0;
    for observation in &observations {
        if let ObservationKind::Active {
            seconds,
            intended,
            launch,
            ..
        } = &observation.kind
        {
            if *launch {
                exposure.intended_seconds += intended.as_secs_f64();
                launches += 1;
            }
            exposure.observed_seconds += seconds.observed.as_secs_f64();
            exposure.eligible_seconds += seconds.eligible.as_secs_f64();
            exposure.onset_active_seconds += seconds
                .onset_active
                .iter()
                .map(|(_, seconds)| seconds.as_secs_f64())
                .fold(0.0_f64, f64::max);
        }
    }
    let daily = events.iter().filter(|event| event.period == Period::Day);
    let interruptions = u64::try_from(daily.clone().count())
        .map_err(|_| RunError::Calendar("interruption count"))?;
    let crashes = u64::try_from(
        daily
            .filter(|event| matches!(event.truth, TruthCause::Co { .. }))
            .count(),
    )
    .map_err(|_| RunError::Calendar("CO crash count"))?;
    let until = day(observed);
    let lower_bound = PartialCounts {
        daily_interruptions: interruptions,
        daily_co_crashes: crashes,
        active_hours: intervals
            .iter()
            .map(|interval| interval.verified_daily_active_hours)
            .sum(),
    };
    let outcome = match stop {
        Some(Stop::Harness(error)) => Outcome::HarnessError {
            observed_until_day: until,
            error,
        },
        Some(Stop::BudgetExpired) => Outcome::Truncated {
            observed_until_day: until,
            reason: "invocation execution budget expired".to_owned(),
            lower_bound,
        },
        Some(Stop::PolicyStopped) => Outcome::Truncated {
            observed_until_day: until,
            reason: "policy stopped before the observation horizon".to_owned(),
            lower_bound,
        },
        None if until < f64::from(config.calendar.days) + 1.0 => Outcome::Truncated {
            observed_until_day: until,
            reason: "observation ended before the horizon".to_owned(),
            lower_bound,
        },
        None => {
            if converged(
                &intervals,
                &config.epochs,
                &config.provenance.membership,
                scenario.id(),
            )? {
                Outcome::CompleteConverged {
                    observed_until_day: until,
                }
            } else if crashes > 0 {
                Outcome::CompleteUnconvergedWithCoCrashes {
                    observed_until_day: until,
                }
            } else {
                Outcome::CompleteUnconvergedCrashFree {
                    observed_until_day: until,
                }
            }
        }
    };
    Ok(Record {
        provenance: config.provenance,
        epochs: config.epochs,
        intervals,
        events,
        units,
        policy: report,
        steps,
        restorations,
        accounts,
        exposure,
        outcome,
        runtime: Runtime {
            wall_seconds,
            compilation_seconds: config.compilation_seconds,
            invocation: None,
            jobs: config.jobs,
            resource_class: config.resource_class,
            trial_launches: launches,
        },
    })
}

#[cfg(test)]
#[path = "assemble_tests.rs"]
mod tests;
