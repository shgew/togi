//! Per-record quantities recomputed from a validated complete record alone.
//!
//! Nothing here reads a simulator, a verdict or the recorded `member` flags beyond the validated
//! ones: `settled_at`, the post-finish safety and risk accounts, depths, retention, per-unit costs and
//! the event identities are all recomputed from the ordered intervals, events, steps and accounts.
//!
//! Safety, settling, final depth, the K/T/H accounts and epoch recovery read the day intervals only: a
//! night interval is a physical probe with no verified daily hours, so an unsafe night probe never
//! counts against the daily contract. Night events stay identifiable by their period.

#[cfg(test)]
#[path = "metrics_review_tests.rs"]
mod metrics_review_tests;

use std::collections::BTreeMap;

use serde::Serialize;
use togi_simrun::day_night::successor::{
    Classification, FinishReason, MachineEvent, Outcome, Period, Phase, ProfileInterval, Record,
    RiskSource, StepIdentity, StepKind, TruthCause, UnitId, UnitPhase,
};

use super::learning::{LearningAudit, audit};
use super::rules::{Rules, Scope};

/// How the record states its outcome; a complete record never carries the other two.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum CompleteOutcome {
    Converged,
    UnconvergedCrashFree,
    UnconvergedWithCoCrashes,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct PostFinish {
    /// Active machine-hours after every unit first finished.
    pub hours: f64,
    pub violating_hours: f64,
    pub first_violation: Option<f64>,
    pub max_joint_ratio: Option<f64>,
    pub max_unit_ratio: Option<f64>,
    /// Every profile held after the first finish is in A; vacuous without a finish.
    pub all_in_a: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SourceRow {
    pub source: String,
    pub learning_k: u64,
    pub learning_h: f64,
    pub post_finish_k: u64,
    pub post_finish_h: f64,
}

/// Overlapping restoration-learning view of the post-finish exposure; a subset, never added.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Tagged {
    pub hours: f64,
    pub h: f64,
    pub k: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SeedRisk {
    /// Unique true CO daily crashes after the first finish, counted once however many units they implicate.
    pub k: u64,
    /// Active machine-hours after the first finish, shared by every source, never summed per source.
    pub t: f64,
    /// Integrated expected CO-added failures over those hours, all sources.
    pub h: f64,
    pub learning_k: u64,
    pub learning_t: f64,
    pub learning_h: f64,
    /// `T > 0` and `H <= rate * lambda * T`, without relaxing the risk budget.
    pub rate_ok: bool,
    pub tagged: Tagged,
    pub sources: Vec<SourceRow>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Causes {
    pub total: u64,
    pub co: u64,
    pub background: u64,
    pub non_cpu: u64,
    pub unattributed: u64,
}

impl Causes {
    fn add(&mut self, truth: &TruthCause) {
        self.total += 1;
        match truth {
            TruthCause::Co { .. } => self.co += 1,
            TruthCause::Background {} => self.background += 1,
            TruthCause::NonCpu {} => self.non_cpu += 1,
            TruthCause::Unattributed {} => self.unattributed += 1,
        }
    }
}

/// Unique machine events by period and phase. Total daily interruptions are separate from CO crashes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct EventCounts {
    pub day_learning: Causes,
    pub day_post_finish: Causes,
    pub night: Causes,
}

impl EventCounts {
    pub fn day(&self) -> Causes {
        Causes {
            total: self.day_learning.total + self.day_post_finish.total,
            co: self.day_learning.co + self.day_post_finish.co,
            background: self.day_learning.background + self.day_post_finish.background,
            non_cpu: self.day_learning.non_cpu + self.day_post_finish.non_cpu,
            unattributed: self.day_learning.unattributed + self.day_post_finish.unattributed,
        }
    }
}

/// Depth counts lost on retreat steps, split by the event's true cause.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Losses {
    pub co: u64,
    pub background: u64,
    pub non_cpu: u64,
    pub unattributed: u64,
    /// A retreat with no triggering event, such as a history-based step back.
    pub history: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UnitMetrics {
    pub unit: UnitId,
    pub finish: Option<(FinishReason, f64)>,
    /// First day this unit alone is finished and stays within its own hazard share through the horizon.
    pub settled_at: Option<f64>,
    pub finish_depth: Option<u8>,
    pub final_depth: u8,
    pub promotions: u64,
    /// Retreat steps naming this unit, derived from the steps, not the recorded summary.
    pub retreat_steps: u64,
    pub hold_hours: f64,
    pub qualification_launches: u64,
    pub qualification_hours: f64,
    pub night_hours: f64,
    pub losses: Losses,
    /// True CO events implicating this unit: an attribution, not an additive count.
    pub co_attributions: u64,
    pub learning_h: f64,
    pub post_finish_h: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DepthMetrics {
    pub final_depths: Vec<u8>,
    pub final_total: u32,
    /// Physical-core mean of the final profile, beside the unit total.
    pub final_core_mean: Option<f64>,
    pub finish_depths: Option<Vec<u8>>,
    /// Certified `M` of the final epoch.
    pub reference: u32,
    pub physical_max: u32,
    /// `ceil(fraction * M)`.
    pub required_total: u64,
    pub unit_floors: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EventView {
    pub id: String,
    pub day: f64,
    pub period: Period,
    pub phase: Phase,
    pub unit_phase: UnitPhase,
    pub restoration_learning: Option<String>,
    pub classification: Classification,
    pub provisional: bool,
    pub reclassified_from: Option<Classification>,
    pub charged: bool,
    pub truth: String,
    pub implicated: Vec<UnitId>,
    pub newest_step: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LedgerView {
    pub limit: u64,
    pub consumed: u64,
    pub remaining: u64,
    pub overspend: u64,
    pub exhausted: bool,
    pub exhausted_day: Option<f64>,
    pub provisional: u64,
    pub reclassified: u64,
    /// Charged unique events by phase; restoration-tagged is an overlapping subset of post-finish exposure.
    pub charged_learning: u64,
    pub charged_post_finish: u64,
    pub charged_restoration_tagged: u64,
}

/// What the policy reported about stops and its newest step, independent of any allowance ledger.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PolicyEvidence {
    pub stopped_units: Vec<UnitId>,
    pub inherited_stops: Vec<togi_simrun::day_night::successor::UnitFinish>,
    pub newest_step: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EpochView {
    pub id: String,
    pub start_day: f64,
    pub end_day: f64,
    pub context: String,
    pub operational_max: u32,
    pub physical_max: u32,
    pub per_unit_max_feasible: Vec<u8>,
    pub certificate: String,
    pub member_hours: f64,
    pub hours: f64,
    pub in_a_at_end: bool,
    /// Start of the final run in A that reaches the epoch's end.
    pub recovered_to_a_at: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SeedMetrics {
    pub seed: u64,
    pub outcome: CompleteOutcome,
    pub first_finish_day: Option<f64>,
    /// First day of the final uninterrupted qualifying interval through the horizon; none is never converged.
    pub settled_at: Option<f64>,
    pub final_in_a: bool,
    pub post_finish: PostFinish,
    pub risk: SeedRisk,
    pub depth: DepthMetrics,
    /// Final profile is in A and meets the benign floor (computed for every scenario, applied to benign ones).
    pub benign_ok: bool,
    /// Never-finished seeds fail; every unit keeps its joint-finish depth less the registered loss and the final profile is in A.
    pub retention_ok: bool,
    pub units: Vec<UnitMetrics>,
    pub events: EventCounts,
    pub event_views: Vec<EventView>,
    /// `None` when the policy reported no allowance ledger; never a fabricated zero ledger.
    pub ledger: Option<LedgerView>,
    pub policy_evidence: PolicyEvidence,
    pub learning: LearningAudit,
    pub epochs: Vec<EpochView>,
    /// Daily CO true crash count over every phase, the basis of the crash-free outcome.
    pub daily_co_crashes: u64,
    pub exposure: togi_simrun::day_night::successor::Exposure,
    pub runtime: togi_simrun::day_night::successor::Runtime,
}

impl SeedMetrics {
    /// Settled day with infinity for a never-converged seed.
    pub fn settled_or_infinite(&self) -> f64 {
        self.settled_at.unwrap_or(f64::INFINITY)
    }
}

pub fn source_label(source: &RiskSource) -> String {
    match source {
        RiskSource::Unit { unit } => format!("unit {}", unit.get()),
        RiskSource::Interaction { identity } => format!("interaction {identity}"),
    }
}

fn identity_label(identity: &StepIdentity) -> String {
    match identity {
        StepIdentity::Unit { unit, proposal } => format!("unit {} step {proposal}", unit.get()),
        StepIdentity::Restoration { restoration, units } => {
            format!(
                "restoration {restoration} (units {})",
                units
                    .iter()
                    .map(|unit| unit.get().to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    }
}

fn truth_label(truth: &TruthCause) -> String {
    match truth {
        TruthCause::Co { source, .. } => format!("co ({})", source_label(source)),
        TruthCause::Background {} => "background".into(),
        TruthCause::NonCpu {} => "non-cpu".into(),
        TruthCause::Unattributed {} => "unattributed".into(),
    }
}

/// Exact number of items; counts are record-bounded, so no cast or saturation is involved.
fn tally<T>(items: impl Iterator<Item = T>) -> u64 {
    items.map(|_| 1_u64).sum()
}

/// Start of the final run of consecutive day intervals (by index) satisfying `ok` that reaches the last
/// day interval. Night intervals carry no daily exposure and never decide settling.
fn trailing_start(
    intervals: &[ProfileInterval],
    days: &[usize],
    ok: impl Fn(usize) -> bool,
) -> Option<f64> {
    let mut start = None;
    for &index in days.iter().rev() {
        if !ok(index) {
            break;
        }
        start = Some(intervals[index].start_day);
    }
    start
}

fn required(
    rules: &Rules,
    record: &Record,
    epoch: &str,
    scope: Scope,
) -> Option<(u64, Vec<u8>, u32)> {
    let reference = &record
        .epochs
        .iter()
        .find(|item| item.id == epoch)?
        .reference;
    let floors = reference
        .per_unit_max_feasible
        .iter()
        .map(|feasible| {
            (*feasible)
                .min(rules.criteria.min_unit_depth)
                .max(scope.unit_floor.unwrap_or(0))
        })
        .collect();
    Some((
        rules
            .criteria
            .useful_depth_of_reference
            .ceil_of(reference.operational_max),
        floors,
        reference.operational_max,
    ))
}

/// Measures one validated complete record; an `Err` names a recorded outcome the recomputation contradicts.
pub fn measure(record: &Record, rules: &Rules, scope: Scope) -> Result<SeedMetrics, String> {
    let complete = match &record.outcome {
        Outcome::CompleteConverged { .. } => CompleteOutcome::Converged,
        Outcome::CompleteUnconvergedCrashFree { .. } => CompleteOutcome::UnconvergedCrashFree,
        Outcome::CompleteUnconvergedWithCoCrashes { .. } => {
            CompleteOutcome::UnconvergedWithCoCrashes
        }
        Outcome::Truncated { .. } | Outcome::HarnessError { .. } => {
            return Err("an incomplete outcome has no scored metrics".into());
        }
    };
    let intervals = &record.intervals;
    // Safety, settling, depth, K/T/H and epoch recovery read the day intervals only; a night interval
    // is a physical probe and carries no daily exposure.
    let days: Vec<usize> = intervals
        .iter()
        .enumerate()
        .filter(|(_, interval)| interval.period == Period::Day)
        .map(|(index, _)| index)
        .collect();
    let final_index = *days.last().ok_or("no deployed day intervals")?;
    let last = &intervals[final_index];
    let mut depths = Vec::with_capacity(intervals.len());
    for interval in intervals {
        depths.push(
            record
                .unit_depths(&interval.profile)
                .map_err(|error| error.to_string())?,
        );
    }
    let first_finish = record.first_finish_day();
    let floors = |interval: &ProfileInterval| required(rules, record, &interval.epoch, scope);
    let floor_met = |index: usize| {
        floors(&intervals[index]).is_some_and(|(total, unit_floors, _)| {
            let depths = &depths[index];
            u64::from(depths.iter().map(|depth| u32::from(*depth)).sum::<u32>()) >= total
                && depths
                    .iter()
                    .zip(&unit_floors)
                    .all(|(depth, floor)| depth >= floor)
        })
    };
    let qualifies = |index: usize| {
        intervals[index].phase == Phase::PostFinish
            && intervals[index].admissibility.member
            && (!scope.benign || floor_met(index))
    };
    let settled_at = trailing_start(intervals, &days, qualifies);
    match (complete, settled_at.is_some()) {
        (CompleteOutcome::Converged, false) => {
            return Err(
                "recorded converged, but no qualifying interval reaches the horizon".into(),
            );
        }
        (
            CompleteOutcome::UnconvergedCrashFree | CompleteOutcome::UnconvergedWithCoCrashes,
            true,
        ) => {
            return Err(
                "recorded unconverged, but a qualifying interval reaches the horizon".into(),
            );
        }
        (CompleteOutcome::Converged, true)
        | (
            CompleteOutcome::UnconvergedCrashFree | CompleteOutcome::UnconvergedWithCoCrashes,
            false,
        ) => {}
    }
    let final_in_a = last.admissibility.member;

    let mut post_finish = PostFinish {
        all_in_a: true,
        ..PostFinish::default()
    };
    let mut risk_learning = (0.0, 0.0);
    let mut tagged = Tagged::default();
    let mut h_post = 0.0;
    for interval in intervals
        .iter()
        .filter(|interval| interval.period == Period::Day)
    {
        let h: f64 = interval
            .sources
            .iter()
            .map(|source| source.expected_failures)
            .sum();
        match interval.phase {
            Phase::Learning => {
                risk_learning.0 += interval.verified_daily_active_hours;
                risk_learning.1 += h;
            }
            Phase::PostFinish => {
                let a = &interval.admissibility;
                post_finish.hours += interval.verified_daily_active_hours;
                h_post += h;
                post_finish.max_joint_ratio = Some(
                    post_finish
                        .max_joint_ratio
                        .map_or(a.joint_ratio, |max| max.max(a.joint_ratio)),
                );
                let unit_max = a.unit_ratios.iter().copied().fold(0.0, f64::max);
                post_finish.max_unit_ratio = Some(
                    post_finish
                        .max_unit_ratio
                        .map_or(unit_max, |max| max.max(unit_max)),
                );
                if !a.member {
                    post_finish.all_in_a = false;
                    post_finish.violating_hours += interval.verified_daily_active_hours;
                    post_finish
                        .first_violation
                        .get_or_insert(interval.start_day);
                }
                if interval.restoration_learning.is_some() {
                    tagged.hours += interval.verified_daily_active_hours;
                    tagged.h += h;
                }
            }
        }
    }

    let mut events = EventCounts::default();
    let mut daily_co = 0_u64;
    let mut views = Vec::with_capacity(record.events.len());
    let by_id: BTreeMap<&str, &MachineEvent> = record
        .events
        .iter()
        .map(|event| (event.policy_event.id.as_str(), event))
        .collect();
    let mut k_post = 0_u64;
    let mut learning_k = 0_u64;
    let mut co_attributions: BTreeMap<UnitId, u64> = BTreeMap::new();
    for event in &record.events {
        let observed = &event.policy_event;
        match (event.period, event.phase) {
            (Period::Night, _) => events.night.add(&event.truth),
            (Period::Day, Phase::Learning) => events.day_learning.add(&event.truth),
            (Period::Day, Phase::PostFinish) => events.day_post_finish.add(&event.truth),
        }
        let implicated = if let TruthCause::Co {
            implicated_units, ..
        } = &event.truth
        {
            implicated_units.clone()
        } else {
            Vec::new()
        };
        if event.period == Period::Day && matches!(event.truth, TruthCause::Co { .. }) {
            daily_co += 1;
            match event.phase {
                Phase::Learning => learning_k += 1,
                Phase::PostFinish => {
                    k_post += 1;
                    if event.restoration_learning.is_some() {
                        tagged.k += 1;
                    }
                }
            }
            for unit in &implicated {
                *co_attributions.entry(*unit).or_default() += 1;
            }
        }
        views.push(EventView {
            id: observed.id.clone(),
            day: observed.day,
            period: event.period,
            phase: event.phase,
            unit_phase: observed.unit_phase,
            restoration_learning: event.restoration_learning.clone(),
            classification: observed.classification,
            provisional: observed.provisional,
            reclassified_from: observed.reclassified_from,
            charged: observed.charged,
            truth: truth_label(&event.truth),
            implicated,
            newest_step: observed.newest_step.as_ref().map(identity_label),
        });
    }
    let mut rows: Vec<SourceRow> = Vec::new();
    for account in &record.accounts.learning {
        rows.push(SourceRow {
            source: source_label(&account.source),
            learning_k: account.k,
            learning_h: account.expected_failures,
            post_finish_k: 0,
            post_finish_h: 0.0,
        });
    }
    for account in &record.accounts.post_finish {
        let label = source_label(&account.source);
        match rows.iter_mut().find(|row| row.source == label) {
            Some(row) => {
                row.post_finish_k = account.k;
                row.post_finish_h = account.expected_failures;
            }
            None => rows.push(SourceRow {
                source: label,
                learning_k: 0,
                learning_h: 0.0,
                post_finish_k: account.k,
                post_finish_h: account.expected_failures,
            }),
        }
    }
    let rate_ok = post_finish.hours > 0.0
        && h_post
            <= rules.criteria.post_finish_rate_of_lambda.value()
                * rules.lambda_per_hour
                * post_finish.hours;
    let risk = SeedRisk {
        k: k_post,
        t: post_finish.hours,
        h: h_post,
        learning_k,
        learning_t: risk_learning.0,
        learning_h: risk_learning.1,
        rate_ok,
        tagged,
        sources: rows,
    };

    let final_depths = depths[final_index].clone();
    let (required_total, unit_floors, reference) =
        floors(last).ok_or("final interval names no epoch")?;
    let finish_index = first_finish.and_then(|_| {
        days.iter()
            .copied()
            .find(|index| intervals[*index].phase == Phase::PostFinish)
    });
    let finish_depths = finish_index.map(|index| depths[index].clone());
    let final_total = final_depths.iter().map(|depth| u32::from(*depth)).sum();
    let physical_max = record
        .epochs
        .iter()
        .find(|epoch| epoch.id == last.epoch)
        .map_or(0, |epoch| epoch.reference.physical_max);
    let depth = DepthMetrics {
        final_core_mean: Record::physical_core_mean_depth(&last.profile),
        final_total,
        final_depths: final_depths.clone(),
        finish_depths: finish_depths.clone(),
        reference,
        physical_max,
        required_total,
        unit_floors,
    };
    let benign_ok = final_in_a && floor_met(final_index);
    let retention_ok = final_in_a
        && finish_depths.as_ref().is_some_and(|finish| {
            finish.iter().zip(&final_depths).all(|(at_finish, last)| {
                u16::from(*last) + u16::from(rules.criteria.retention_max_loss)
                    >= u16::from(*at_finish)
            })
        });

    // Per-unit tables.
    let mut units: Vec<UnitMetrics> = Vec::with_capacity(record.units.len());
    for (index, summary) in record.units.iter().enumerate() {
        let finish = summary
            .finish
            .as_ref()
            .map(|finish| (finish.reason, finish.day));
        let unit_floor = depth.unit_floors.get(index).copied().unwrap_or(0);
        let own_ok = |at: usize| {
            let interval = &intervals[at];
            interval
                .admissibility
                .unit_ratios
                .get(index)
                .is_some_and(|ratio| *ratio <= 1.0)
                && (!scope.benign || depths[at][index] >= unit_floor)
        };
        let settled_at = finish.and_then(|(_, day)| {
            trailing_start(intervals, &days, own_ok).map(|start| start.max(day))
        });
        let mut learning_h = 0.0;
        let mut post_finish_h = 0.0;
        for interval in intervals
            .iter()
            .filter(|interval| interval.period == Period::Day)
        {
            let h: f64 = interval
                .sources
                .iter()
                .filter(|source| source.source == RiskSource::Unit { unit: summary.unit })
                .map(|source| source.expected_failures)
                .sum();
            match interval.phase {
                Phase::Learning => learning_h += h,
                Phase::PostFinish => post_finish_h += h,
            }
        }
        let promotions = tally(record.steps.iter().filter(
            |step| matches!(&step.kind, StepKind::Climb { unit, .. } if *unit == summary.unit),
        ));
        let retreat_steps = tally(record.steps.iter().filter(|step| matches!(&step.kind, StepKind::Retreat { units, .. } if units.contains(&summary.unit))));
        units.push(UnitMetrics {
            unit: summary.unit,
            finish,
            settled_at,
            finish_depth: finish_depths
                .as_ref()
                .and_then(|depths| depths.get(index).copied()),
            final_depth: final_depths.get(index).copied().unwrap_or(0),
            promotions,
            retreat_steps,
            hold_hours: summary.hold_hours,
            qualification_launches: summary.qualification_launches,
            qualification_hours: summary.qualification_hours,
            night_hours: summary.night_hours,
            losses: Losses::default(),
            co_attributions: co_attributions.get(&summary.unit).copied().unwrap_or(0),
            learning_h,
            post_finish_h,
        });
    }
    for step in &record.steps {
        let StepKind::Retreat {
            event,
            units: retreated,
        } = &step.kind
        else {
            continue;
        };
        let (Ok(before), Ok(after)) = (
            record.unit_depths(&step.before),
            record.unit_depths(&step.after),
        ) else {
            continue;
        };
        let cause = event
            .as_deref()
            .and_then(|id| by_id.get(id))
            .map(|event| &event.truth);
        for unit in retreated {
            let lost = u64::from(before[unit.index()].saturating_sub(after[unit.index()]));
            let losses = &mut units[unit.index()].losses;
            match cause {
                Some(TruthCause::Co { .. }) => losses.co += lost,
                Some(TruthCause::Background {}) => losses.background += lost,
                Some(TruthCause::NonCpu {}) => losses.non_cpu += lost,
                Some(TruthCause::Unattributed {}) => losses.unattributed += lost,
                None => losses.history += lost,
            }
        }
    }

    // Ledger and learning audit.
    let charged = || {
        record
            .events
            .iter()
            .filter(|event| event.policy_event.charged)
    };
    let ledger_view = record.policy.allowance.as_ref().map(|ledger| LedgerView {
        limit: ledger.limit,
        consumed: ledger.consumed,
        remaining: ledger.remaining(),
        overspend: ledger.overspend(),
        exhausted: ledger.exhausted(),
        exhausted_day: ledger.exhausted_day,
        provisional: ledger.provisional,
        reclassified: ledger.reclassified,
        charged_learning: tally(charged().filter(|event| event.phase == Phase::Learning)),
        charged_post_finish: tally(charged().filter(|event| event.phase == Phase::PostFinish)),
        charged_restoration_tagged: tally(
            charged().filter(|event| event.restoration_learning.is_some()),
        ),
    });
    let policy_evidence = PolicyEvidence {
        stopped_units: record.policy.stopped_units.clone(),
        inherited_stops: record.policy.inherited_stops.clone(),
        newest_step: record.policy.newest_step.as_ref().map(identity_label),
    };
    let learning = audit(record, rules);

    // Truth epochs: the share of each epoch's hours in A and recovery.
    let epochs = record
        .epochs
        .iter()
        .map(|epoch| {
            let epoch_intervals = || {
                intervals
                    .iter()
                    .filter(|interval| interval.period == Period::Day && interval.epoch == epoch.id)
            };
            let hours = epoch_intervals()
                .map(|interval| interval.verified_daily_active_hours)
                .sum();
            let member_hours = epoch_intervals()
                .filter(|interval| interval.admissibility.member)
                .map(|interval| interval.verified_daily_active_hours)
                .sum();
            let mut recovered = None;
            for interval in epoch_intervals().rev() {
                if !interval.admissibility.member {
                    break;
                }
                recovered = Some(interval.start_day);
            }
            EpochView {
                id: epoch.id.clone(),
                start_day: epoch.start_day,
                end_day: epoch.end_day,
                context: epoch.context.clone(),
                operational_max: epoch.reference.operational_max,
                physical_max: epoch.reference.physical_max,
                per_unit_max_feasible: epoch.reference.per_unit_max_feasible.clone(),
                certificate: epoch.reference.certificate.clone(),
                member_hours,
                hours,
                in_a_at_end: epoch_intervals()
                    .next_back()
                    .is_some_and(|interval| interval.admissibility.member),
                recovered_to_a_at: recovered,
            }
        })
        .collect();

    Ok(SeedMetrics {
        seed: record.provenance.seed,
        outcome: complete,
        first_finish_day: first_finish,
        settled_at,
        final_in_a,
        post_finish,
        risk,
        depth,
        benign_ok,
        retention_ok,
        units,
        events,
        event_views: views,
        ledger: ledger_view,
        policy_evidence,
        learning,
        epochs,
        daily_co_crashes: daily_co,
        exposure: record.exposure.clone(),
        runtime: record.runtime.clone(),
    })
}
