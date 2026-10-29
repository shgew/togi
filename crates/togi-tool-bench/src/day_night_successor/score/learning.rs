//! Recomputes the learning contract of #1457 §2 and §4 from a record's events, steps and restorations.
//!
//! `Record::validate` proves the representation and the ledger arithmetic; this audit judges the
//! permission rules: allowance and stop rules for every promotion, the step size and hold, the exact
//! qualification, the charge and stop consequences of an event, and every firmware restoration.
//! Chronology at an equal day uses the event's newest-step identity: an event caused by a step never
//! retroactively forbids that step, while an earlier same-day charge forbids the next promotion.

use serde::Serialize;
use togi_machine::PerCore;
use togi_simrun::day_night::successor::{
    Classification, FirmwareRestoration, MachineEvent, Period, Phase, ProfileStep, Qualification,
    Record, RestorationOutcome, RestorationResult, StepIdentity, StepKind, UnitId,
};

use super::rules::{MAX_STEP, Rules};

#[cfg(test)]
#[path = "learning_review_tests.rs"]
mod review_tests;

/// One broken learning-contract rule on one record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Violation {
    pub rule: &'static str,
    pub subject: String,
    pub detail: String,
}

/// How a firmware restoration ended, as the record shows it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub enum RestorationStatus {
    Blocked { reason: String },
    Retained,
    Crashed { event: String },
}

/// The identities of one restoration, for the per-seed report.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RestorationView {
    pub id: String,
    pub epoch: String,
    pub transition_day: f64,
    pub stock_reset_day: f64,
    pub applied_day: Option<f64>,
    pub clean_stock_hours: Option<f64>,
    /// Active hours at stock between reset and application, recomputed from the intervals.
    pub stock_hours_recomputed: Option<f64>,
    pub previous_depths: Vec<u8>,
    pub status: RestorationStatus,
    pub stopped_units: Vec<UnitId>,
    pub qualification: Option<String>,
    pub violations: usize,
}

/// One promotion, for the per-seed report.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PromotionView {
    pub id: String,
    pub day: f64,
    pub unit: UnitId,
    pub before: u8,
    pub after: u8,
    pub ceiling: u8,
    pub charged_before: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LearningAudit {
    /// The record carried an allowance ledger, so the contract could be judged; false means unreported,
    /// which is not the same as zero violations.
    pub reported: bool,
    pub violations: Vec<Violation>,
    pub promotions: Vec<PromotionView>,
    pub restorations: Vec<RestorationView>,
}

/// Audits the learning contract. A record without an allowance ledger has nothing to judge: the audit
/// is returned unreported, never as an empty violation list.
pub fn audit(record: &Record, rules: &Rules) -> LearningAudit {
    if record.policy.allowance.is_none() {
        return LearningAudit::default();
    }
    let mut audit = Audit {
        record,
        rules,
        result: LearningAudit {
            reported: true,
            ..LearningAudit::default()
        },
    };
    audit.ledger();
    audit.charges();
    audit.chain();
    audit.summaries();
    audit.promotions();
    audit.event_stops();
    audit.restorations();
    audit.tags();
    audit.result
}

struct Audit<'a> {
    record: &'a Record,
    rules: &'a Rules,
    result: LearningAudit,
}

/// The span one applied restoration governs: from its application step to the next application that
/// changes the profile (the stock step-back of a crash), or the horizon. Derived from the steps, never
/// from the restoration's declared result or from tags.
struct Lifetime {
    start: f64,
    end: f64,
    /// The event a closing retreat names; that event precedes the retreat and still belongs to the span.
    closing_event: Option<String>,
}

fn zero(profile: &PerCore<u8>) -> bool {
    profile.iter().all(|(_, depth)| depth == 0)
}

/// Every classification except `NonCpu` keeps the event CPU-suspect for charging.
fn suspect(classification: Classification) -> bool {
    match classification {
        Classification::CpuSuspect | Classification::Unanswered | Classification::Unexplained => {
            true
        }
        Classification::NonCpu | Classification::Unreported => false,
    }
}

impl Audit<'_> {
    fn violate(
        &mut self,
        rule: &'static str,
        subject: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.result.violations.push(Violation {
            rule,
            subject: subject.into(),
            detail: detail.into(),
        });
    }

    fn depths(&mut self, subject: &str, profile: &PerCore<u8>) -> Option<Vec<u8>> {
        if let Ok(depths) = self.record.unit_depths(profile) {
            Some(depths)
        } else {
            self.violate(
                "unit-profile",
                subject,
                "profile is not uniform within every tuning unit",
            );
            None
        }
    }

    fn step_index(&self, identity: &StepIdentity) -> Option<usize> {
        match identity {
            StepIdentity::Unit { proposal, .. } => self.record.steps.iter().position(|step| &step.id == proposal),
            StepIdentity::Restoration { restoration, .. } => self
                .record
                .steps
                .iter()
                .position(|step| matches!(&step.kind, StepKind::Restoration { restoration: id } if id == restoration)),
        }
    }

    /// Charged events that precede the step at `index`: earlier days, and same-day events whose newest
    /// step is not the step itself or a later one.
    fn charged_before(&self, index: usize) -> u64 {
        let step = &self.record.steps[index];
        self.record
            .events
            .iter()
            .filter(|event| event.policy_event.charged)
            .filter(|event| {
                let day = event.policy_event.day;
                if day < step.day - self.rules.day_tolerance {
                    return true;
                }
                if day > step.day + self.rules.day_tolerance {
                    return false;
                }
                event
                    .policy_event
                    .newest_step
                    .as_ref()
                    .and_then(|identity| self.step_index(identity))
                    .is_none_or(|newest| newest < index)
            })
            .map(|_| 1_u64)
            .sum()
    }

    fn epoch_at(&self, day: f64) -> Option<&str> {
        self.record
            .epochs
            .iter()
            .find(|epoch| {
                day >= epoch.start_day - self.rules.day_tolerance
                    && day < epoch.end_day - self.rules.day_tolerance
            })
            .map(|epoch| epoch.id.as_str())
    }

    fn ledger(&mut self) {
        let Some(ledger) = &self.record.policy.allowance else {
            return;
        };
        if u64::from(self.rules.criteria.allowance_limit) != ledger.limit {
            self.violate(
                "allowance-limit",
                "ledger",
                format!(
                    "limit {} differs from the registered {}",
                    ledger.limit, self.rules.criteria.allowance_limit
                ),
            );
        }
    }

    /// Charge rules: a suspect daily event at a nonzero profile is charged; a charge is never refunded,
    /// including by reclassification, and never exists without a suspect basis.
    fn charges(&mut self) {
        for event in self
            .record
            .events
            .iter()
            .filter(|event| event.period == Period::Day)
        {
            let observed = &event.policy_event;
            let nonzero = !zero(&event.profile);
            let was_suspect =
                suspect(observed.classification) || observed.reclassified_from.is_some_and(suspect);
            if nonzero && was_suspect && !observed.charged {
                self.violate(
                    "charge-refund",
                    &observed.id,
                    "a suspect or provisional event at a nonzero profile is not charged",
                );
            }
            if observed.charged && !was_suspect && observed.reclassified_from.is_none() {
                self.violate(
                    "charge-basis",
                    &observed.id,
                    "a charged event has no suspect classification",
                );
            }
        }
    }

    /// Every profile change is one recorded application: the ordered steps start at stock and chain
    /// exactly to each interval's profile, so omitting a step cannot hide a promotion or restoration.
    fn chain(&mut self) {
        let record = self.record;
        let tolerance = self.rules.day_tolerance;
        let mut current: PerCore<u8> = record
            .provenance
            .topology
            .iter()
            .map(|core| (core.core, 0))
            .collect();
        let mut next = 0;
        for (index, interval) in record
            .intervals
            .iter()
            .filter(|interval| interval.period == Period::Day)
            .enumerate()
        {
            while let Some(step) = record.steps.get(next)
                && step.day <= interval.start_day + tolerance
            {
                if index > 0
                    && step.day < interval.start_day - tolerance
                    && step.before != step.after
                {
                    self.violate(
                        "step-mid-interval",
                        &step.id,
                        format!(
                            "application at day {} changes the profile inside an interval",
                            step.day
                        ),
                    );
                }
                if step.before != current {
                    self.violate(
                        "step-chain",
                        &step.id,
                        "the step does not start from the deployed profile",
                    );
                }
                match &step.kind {
                    StepKind::Retreat { .. }
                        if !step.after.iter().all(|(core, depth)| {
                            step.before.get(core).is_some_and(|before| depth <= before)
                        }) =>
                    {
                        self.violate("retreat-direction", &step.id, "a retreat deepens a core");
                    }
                    StepKind::StockReset { .. } if !zero(&step.after) => self.violate(
                        "stock-reset-profile",
                        &step.id,
                        "a stock reset does not end at stock",
                    ),
                    StepKind::Climb { .. }
                    | StepKind::Restoration { .. }
                    | StepKind::Retreat { .. }
                    | StepKind::StockReset { .. } => {}
                }
                current = step.after;
                next += 1;
            }
            if interval.profile != current {
                self.violate(
                    "interval-chain",
                    format!("interval at day {}", interval.start_day),
                    "the deployed profile is not the result of the recorded applications",
                );
                current = interval.profile;
            }
        }
        for step in record
            .steps
            .iter()
            .skip(next)
            .filter(|step| step.before != step.after)
        {
            self.violate(
                "step-mid-interval",
                &step.id,
                format!(
                    "application at day {} changes the profile inside the last interval",
                    step.day
                ),
            );
        }
    }

    /// Recorded promotion and retreat-step counts agree with the applications.
    fn summaries(&mut self) {
        let record = self.record;
        for summary in &record.units {
            let promotions = record.steps.iter().filter(|step| matches!(&step.kind, StepKind::Climb { unit, .. } if *unit == summary.unit)).count() as u64;
            let retreats = record.steps.iter().filter(|step| matches!(&step.kind, StepKind::Retreat { units, .. } if units.contains(&summary.unit))).count() as u64;
            if summary.promotions != promotions || summary.retreats.steps != retreats {
                self.violate("summary-counts", format!("unit {}", summary.unit.get()), format!("recorded {} promotions and {} retreat steps; the applications show {promotions} and {retreats}", summary.promotions, summary.retreats.steps));
            }
        }
    }

    fn promotions(&mut self) {
        let record = self.record;
        let criteria = self.rules.criteria;
        let limit = u64::from(criteria.allowance_limit);
        for (index, step) in record.steps.iter().enumerate() {
            let StepKind::Climb {
                unit,
                ceiling_depth,
                clean_hold_hours,
                qualification,
            } = &step.kind
            else {
                continue;
            };
            let (Some(before), Some(after)) = (
                self.depths(&step.id, &step.before),
                self.depths(&step.id, &step.after),
            ) else {
                continue;
            };
            let (before_u, after_u) = (before[unit.index()], after[unit.index()]);
            let spent = self.charged_before(index);
            self.result.promotions.push(PromotionView {
                id: step.id.clone(),
                day: step.day,
                unit: *unit,
                before: before_u,
                after: after_u,
                ceiling: *ceiling_depth,
                charged_before: spent,
            });
            if spent >= limit {
                self.violate(
                    "promotion-after-exhaustion",
                    &step.id,
                    format!("{spent} charged events precede a promotion; the limit is {limit}"),
                );
            }
            self.check_stop_chain(step, *unit);
            let distance = u32::from(ceiling_depth.saturating_sub(before_u));
            let expected = u32::from(before_u) + distance.div_ceil(2).min(u32::from(MAX_STEP));
            if distance == 0 || u32::from(after_u) != expected {
                self.violate("promotion-step", &step.id, format!("unit {} moves {before_u} -> {after_u} under ceiling {ceiling_depth}; the rule gives {expected}", unit.get()));
            }
            if before
                .iter()
                .zip(&after)
                .enumerate()
                .any(|(other, (b, a))| other != unit.index() && b != a)
            {
                self.violate(
                    "promotion-single-unit",
                    &step.id,
                    "a promotion changes a unit other than the promoted one",
                );
            }
            let needed = f64::from(if before_u == 0 {
                criteria.first_step_clean_stock_hours
            } else {
                criteria.clean_hold_hours
            });
            if *clean_hold_hours + self.rules.day_tolerance < needed {
                self.violate(
                    "promotion-hold",
                    &step.id,
                    format!("clean hold {clean_hold_hours} h is below the required {needed} h"),
                );
            }
            let previous_day = record.steps[..index]
                .last()
                .map_or(1.0, |previous| previous.day);
            let hold_start = self.clean_since(previous_day, step.day);
            let hours: f64 = record
                .intervals
                .iter()
                .filter(|interval| {
                    interval.period == Period::Day
                        && interval.start_day >= hold_start - self.rules.day_tolerance
                        && interval.end_day <= step.day + self.rules.day_tolerance
                        && interval.profile == step.before
                })
                .map(|interval| interval.verified_daily_active_hours)
                .sum();
            if hours + self.rules.day_tolerance < needed
                || *clean_hold_hours > hours + self.rules.day_tolerance
            {
                self.violate("promotion-verified-hold", &step.id,
                    format!("{hours} verified clean hours at the previous whole profile; claimed {clean_hold_hours}, required {needed}"));
            }
            self.check_qualification(&step.id, step, qualification, &step.after);
        }
    }

    fn clean_since(&self, from: f64, until: f64) -> f64 {
        self.record
            .events
            .iter()
            .filter(|event| {
                event.period == Period::Day
                    && event.policy_event.day >= from
                    && event.policy_event.day < until
                    && (suspect(event.policy_event.classification)
                        || event.policy_event.provisional
                        || event.policy_event.reclassified_from.is_some_and(suspect))
            })
            .map(|event| event.policy_event.day)
            .fold(from, f64::max)
    }

    fn check_stop_chain(&mut self, step: &ProfileStep, unit: UnitId) {
        let tolerance = self.rules.day_tolerance;
        if let Some(finish) = self
            .record
            .units
            .get(unit.index())
            .and_then(|summary| summary.finish.as_ref())
            && step.day > finish.day + tolerance
        {
            self.violate(
                "promotion-after-finish",
                &step.id,
                format!(
                    "unit {} finished at day {} and cannot reopen",
                    unit.get(),
                    finish.day
                ),
            );
        }
        let parent = self
            .record
            .provenance
            .membership
            .get(unit.index())
            .and_then(|member| member.parent);
        if let Some(finish) = self
            .record
            .policy
            .inherited_stops
            .iter()
            .find(|finish| Some(finish.unit) == parent)
            && step.day >= finish.day - tolerance
        {
            self.violate(
                "promotion-after-parent-stop",
                &step.id,
                format!(
                    "coarse unit {} stopped at day {}; its cores cannot reopen it",
                    finish.unit.get(),
                    finish.day
                ),
            );
        }
    }

    fn check_qualification(
        &mut self,
        subject: &str,
        step: &ProfileStep,
        qualification: &Qualification,
        expected: &PerCore<u8>,
    ) {
        if &qualification.profile != expected || qualification.profile != step.after {
            self.violate(
                "qualification-profile",
                subject,
                "the qualified profile is not the exact applied profile",
            );
        }
        if self.epoch_at(step.day) != Some(qualification.epoch.as_str()) {
            self.violate(
                "qualification-context",
                subject,
                format!(
                    "qualification epoch {} is not the epoch of day {}",
                    qualification.epoch, step.day
                ),
            );
        }
        if self.epoch_at(qualification.day) != Some(qualification.epoch.as_str()) {
            self.violate(
                "qualification-date-context",
                subject,
                "qualification predates its recorded truth context",
            );
        }
        if !qualification.safety_checks_satisfied {
            self.violate(
                "qualification-safety",
                subject,
                "safety checks were not satisfied",
            );
        }
        if qualification.fresh_witnesses.is_empty() && qualification.reused_witnesses.is_empty() {
            self.violate("qualification-witness", subject, "no qualification witness");
        }
        if qualification.day > step.day + self.rules.day_tolerance {
            self.violate(
                "qualification-order",
                subject,
                "qualification follows its application",
            );
        }
    }

    /// A learning-phase charged event stops its newest unit from the event onward and steps it back.
    /// Both are bound to the event, not to the unit's summary finish: a later finish does not satisfy
    /// the stop, the retreat must follow the event, and no promotion of the unit may follow the event.
    fn event_stops(&mut self) {
        let record = self.record;
        let tolerance = self.rules.day_tolerance;
        for event in record.events.iter().filter(|event| {
            event.period == Period::Day
                && event.policy_event.charged
                && event.phase == Phase::Learning
        }) {
            let observed = &event.policy_event;
            if observed.newest_step.is_none() && !zero(&event.profile) {
                self.violate(
                    "event-step-identity",
                    &observed.id,
                    "a charged learning event has no newest-step identity",
                );
            }
            let Some(StepIdentity::Unit { unit, proposal }) = &observed.newest_step else {
                continue;
            };
            let cause = record.steps.iter().position(|step| step.id == *proposal);
            let stopped = record
                .units
                .get(unit.index())
                .and_then(|summary| summary.finish.as_ref())
                .is_some_and(|finish| finish.day <= observed.day + tolerance);
            if !stopped {
                self.violate(
                    "event-stop",
                    &observed.id,
                    format!(
                        "unit {} has no finish at or before its charged event",
                        unit.get()
                    ),
                );
            }
            let retreated = record.steps.iter().enumerate().any(|(index, step)| {
                matches!(&step.kind, StepKind::Retreat { event: Some(id), units } if *id == observed.id && units.contains(unit))
                    && self.follows(observed.day, cause, index, step.day)
                    && match (record.unit_depths(&step.before), record.unit_depths(&step.after)) {
                        (Ok(before), Ok(after)) => after[unit.index()] < before[unit.index()],
                        _ => false,
                    }
            });
            if !retreated {
                self.violate(
                    "event-step-back",
                    &observed.id,
                    format!(
                        "unit {} does not step back after its charged event",
                        unit.get()
                    ),
                );
            }
            for (index, step) in record.steps.iter().enumerate() {
                if matches!(&step.kind, StepKind::Climb { unit: climbed, .. } if climbed == unit)
                    && self.follows(observed.day, cause, index, step.day)
                {
                    self.violate(
                        "promotion-after-stop",
                        &step.id,
                        format!(
                            "unit {} is promoted after its charged event {}",
                            unit.get(),
                            observed.id
                        ),
                    );
                }
            }
        }
    }

    /// Whether the step at `index` (day `step_day`) comes after an event of day `event_day` caused by
    /// the step at `cause`: later days follow; at an equal day only a later application follows, so the
    /// event's own step never trails its event.
    fn follows(&self, event_day: f64, cause: Option<usize>, index: usize, step_day: f64) -> bool {
        let tolerance = self.rules.day_tolerance;
        if step_day > event_day + tolerance {
            true
        } else if step_day < event_day - tolerance {
            false
        } else {
            cause.is_some_and(|cause| index > cause)
        }
    }

    fn stock_hours(&self, from: f64, until: f64) -> (f64, bool) {
        let mut hours = 0.0;
        let mut stock = true;
        for interval in self
            .record
            .intervals
            .iter()
            .filter(|interval| interval.period == Period::Day)
        {
            let overlap = interval.end_day.min(until) - interval.start_day.max(from);
            if overlap <= self.rules.day_tolerance {
                continue;
            }
            if zero(&interval.profile) {
                hours += interval.verified_daily_active_hours * overlap
                    / (interval.end_day - interval.start_day);
            } else {
                stock = false;
            }
        }
        (hours, stock)
    }

    fn restorations(&mut self) {
        let record = self.record;
        let tolerance = self.rules.day_tolerance;
        for restoration in &record.restorations {
            let before = self.result.violations.len();
            let id = restoration.id.as_str();
            let previous = self
                .depths(id, &restoration.previous_profile)
                .unwrap_or_default();
            if !zero(&restoration.stock_readback)
                || restoration.stock_reset_day + tolerance < restoration.transition_day
            {
                self.violate(
                    "restoration-stock",
                    id,
                    "the firmware change does not reset to a stock readback",
                );
            }
            let reset_step = record.steps.iter().find(|step| {
                matches!(&step.kind, StepKind::StockReset { epoch } if *epoch == restoration.epoch)
                    && self.rules.same_day(step.day, restoration.stock_reset_day)
                    && zero(&step.after)
            });
            match reset_step {
                None => self.violate(
                    "restoration-stock-step",
                    id,
                    "no stock-reset step at the recorded reset day",
                ),
                Some(step) if step.before != restoration.previous_profile => self.violate(
                    "restoration-previous-profile",
                    id,
                    "the saved previous vector is not the profile deployed before the firmware reset",
                ),
                Some(_) => {}
            }
            let steps: Vec<(usize, &ProfileStep)> = record.steps.iter().enumerate().filter(|(_, step)| matches!(&step.kind, StepKind::Restoration { restoration: item } if item == id)).collect();
            let mut view = RestorationView {
                id: restoration.id.clone(),
                epoch: restoration.epoch.clone(),
                transition_day: restoration.transition_day,
                stock_reset_day: restoration.stock_reset_day,
                applied_day: None,
                clean_stock_hours: None,
                stock_hours_recomputed: None,
                previous_depths: previous.clone(),
                status: RestorationStatus::Retained,
                stopped_units: Vec::new(),
                qualification: None,
                violations: 0,
            };
            match &restoration.outcome {
                RestorationOutcome::Blocked { reason } => {
                    view.status = RestorationStatus::Blocked {
                        reason: reason.clone(),
                    };
                    if !steps.is_empty() {
                        self.violate(
                            "restoration-blocked-applied",
                            id,
                            "a blocked restoration also has an application step",
                        );
                    }
                }
                RestorationOutcome::Applied {
                    day,
                    qualification,
                    clean_stock_hours,
                    result,
                } => {
                    view.applied_day = Some(*day);
                    view.clean_stock_hours = Some(*clean_stock_hours);
                    view.qualification = Some(qualification.id.clone());
                    self.applied(
                        restoration,
                        *day,
                        qualification,
                        *clean_stock_hours,
                        &steps,
                        &previous,
                        &mut view,
                    );
                    match result {
                        RestorationResult::Retained {} => view.status = RestorationStatus::Retained,
                        RestorationResult::Crashed {
                            event,
                            stock_step_back_day,
                            stock_readback,
                            stopped_units,
                            witnesses,
                        } => {
                            view.status = RestorationStatus::Crashed {
                                event: event.clone(),
                            };
                            view.stopped_units.clone_from(stopped_units);
                            self.crashed(
                                restoration,
                                *day,
                                event,
                                *stock_step_back_day,
                                stock_readback,
                                stopped_units,
                                witnesses,
                                &previous,
                            );
                        }
                    }
                    let declared = match result {
                        RestorationResult::Crashed { event, .. } => Some(event.as_str()),
                        RestorationResult::Retained {} => None,
                    };
                    self.restoration_crashes(restoration, declared, &previous);
                }
            }
            view.violations = self.result.violations.len() - before;
            self.result.restorations.push(view);
        }
        let known: Vec<&str> = record
            .restorations
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        for step in &record.steps {
            if let StepKind::Restoration { restoration } = &step.kind
                && !known.contains(&restoration.as_str())
            {
                self.violate(
                    "restoration-orphan",
                    &step.id,
                    "a restoration step has no firmware restoration record",
                );
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one restoration's recorded identities are checked together"
    )]
    fn applied(
        &mut self,
        restoration: &FirmwareRestoration,
        day: f64,
        qualification: &Qualification,
        clean_stock_hours: f64,
        steps: &[(usize, &ProfileStep)],
        previous: &[u8],
        view: &mut RestorationView,
    ) {
        let record = self.record;
        let id = restoration.id.as_str();
        let criteria = self.rules.criteria;
        if clean_stock_hours + self.rules.day_tolerance
            < f64::from(criteria.restoration_clean_stock_hours)
        {
            self.violate(
                "restoration-stock-hours",
                id,
                format!(
                    "{clean_stock_hours} clean stock hours are below {}",
                    criteria.restoration_clean_stock_hours
                ),
            );
        }
        let hold_start = self.clean_since(restoration.stock_reset_day, day);
        let (hours, stock) = self.stock_hours(hold_start, day);
        view.stock_hours_recomputed = Some(hours);
        if !stock
            || hours + self.rules.day_tolerance < f64::from(criteria.restoration_clean_stock_hours)
        {
            self.violate("restoration-stock-interval", id, format!("{hours:.3} verified active stock hours between reset and application; stock only: {stock}"));
        }
        if qualification.profile != restoration.previous_profile {
            self.violate(
                "restoration-exact-profile",
                id,
                "the qualified profile is not the exact previous vector",
            );
        }
        if qualification.epoch != restoration.epoch
            || qualification.stock_controls.is_empty()
            || qualification.fresh_witnesses.is_empty()
            || !qualification.reused_witnesses.is_empty()
        {
            self.violate("restoration-new-context", id, "the restoration needs new-context stock controls and fresh qualification witnesses only");
        }
        if !qualification.safety_checks_satisfied {
            self.violate("restoration-safety", id, "safety checks were not satisfied");
        }
        if qualification.day > day + self.rules.day_tolerance
            || qualification.day + self.rules.day_tolerance < restoration.stock_reset_day
            || day + self.rules.day_tolerance < restoration.stock_reset_day
        {
            self.violate(
                "restoration-order",
                id,
                "qualification or application precedes its stock interval",
            );
        }
        let [(index, step)] = steps else {
            self.violate(
                "restoration-single-step",
                id,
                format!(
                    "{} restoration application steps; exactly one is required",
                    steps.len()
                ),
            );
            return;
        };
        if !self.rules.same_day(step.day, day)
            || step.before != restoration.stock_readback
            || step.after != restoration.previous_profile
        {
            self.violate("restoration-whole-profile", id, "the application is not one whole-profile step from the stock readback to the previous vector");
        }
        let spent = self.charged_before(*index);
        if spent >= u64::from(criteria.allowance_limit) {
            self.violate(
                "restoration-exhausted",
                id,
                format!(
                    "{spent} charged events precede a restoration; the limit is {}",
                    criteria.allowance_limit
                ),
            );
        }
        for unit in &record.units {
            let allowed = unit
                .ceiling_evidence
                .iter()
                .filter(|evidence| {
                    evidence.epoch == restoration.epoch
                        && evidence.day <= day + self.rules.day_tolerance
                })
                .map(|evidence| evidence.ceiling_depth)
                .min();
            if let (Some(ceiling), Some(depth)) = (allowed, previous.get(unit.unit.index()))
                && ceiling < *depth
            {
                self.violate(
                    "restoration-adverse-ceiling",
                    id,
                    format!(
                        "unit {} restores depth {depth} over new-context ceiling {ceiling}",
                        unit.unit.get()
                    ),
                );
            }
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one restoration crash's recorded identities are checked together"
    )]
    fn crashed(
        &mut self,
        restoration: &FirmwareRestoration,
        day: f64,
        event: &str,
        step_back_day: f64,
        readback: &PerCore<u8>,
        stopped: &[UnitId],
        witnesses: &[String],
        previous: &[u8],
    ) {
        let record = self.record;
        let id = restoration.id.as_str();
        let tolerance = self.rules.day_tolerance;
        let Some(crash) = record
            .events
            .iter()
            .find(|item| item.policy_event.id == event)
        else {
            self.violate(
                "restoration-crash-event",
                id,
                "the crash event is not in the machine events",
            );
            return;
        };
        if !crash.policy_event.charged
            || !(suspect(crash.policy_event.classification)
                || crash.policy_event.reclassified_from.is_some_and(suspect))
        {
            self.violate(
                "restoration-crash-charge",
                id,
                "the crash event is not a charged CPU-suspect event",
            );
        }
        if crash.policy_event.day + tolerance < day
            || step_back_day + tolerance < crash.policy_event.day
        {
            self.violate(
                "restoration-crash-order",
                id,
                "the crash precedes the application or the step-back precedes the crash",
            );
        }
        let units_match = |units: &[UnitId]| {
            units.len() == stopped.len() && units.iter().all(|unit| stopped.contains(unit))
        };
        if !matches!(&crash.policy_event.newest_step, Some(StepIdentity::Restoration { restoration: item, units }) if item == id && units_match(units))
        {
            self.violate(
                "restoration-crash-identity",
                id,
                "the crash event does not name this restoration and its stopped units",
            );
        }
        let restored: Vec<usize> = previous
            .iter()
            .enumerate()
            .filter(|(_, depth)| **depth > 0)
            .map(|(unit, _)| unit)
            .collect();
        if !restored
            .iter()
            .all(|unit| stopped.iter().any(|item| item.index() == *unit))
        {
            self.violate(
                "restoration-crash-stop",
                id,
                "a restored unit is not stopped after the crash",
            );
        }
        if witnesses.is_empty() {
            self.violate(
                "restoration-crash-witness",
                id,
                "the crash names no witness for the permanent stop",
            );
        }
        for unit in stopped {
            let permanent = record.policy.stopped_units.contains(unit)
                && !record.steps.iter().any(|step| {
                    matches!(&step.kind, StepKind::Climb { unit: climbed, .. } if climbed == unit)
                        && step.day > step_back_day + tolerance
                });
            if !permanent {
                self.violate(
                    "restoration-crash-stop",
                    id,
                    format!(
                        "unit {} is not permanently stopped after the crash",
                        unit.get()
                    ),
                );
            }
            // The crash never resets the first-finish boundary: the unit keeps the finish it had
            // before the firmware change.
            let kept = record
                .units
                .get(unit.index())
                .and_then(|summary| summary.finish.as_ref())
                .is_some_and(|finish| finish.day <= restoration.transition_day + tolerance);
            if !kept {
                self.violate(
                    "restoration-crash-finish",
                    id,
                    format!(
                        "unit {} lacks the finish it recorded before the firmware change",
                        unit.get()
                    ),
                );
            }
        }
        let stepped = record.steps.iter().any(|step| {
            matches!(&step.kind, StepKind::Retreat { event: Some(item), units } if item == event && units_match(units)) && self.rules.same_day(step.day, step_back_day) && zero(&step.after) && step.after == *readback
        });
        if !stepped || !zero(readback) {
            self.violate(
                "restoration-step-back",
                id,
                "the crash does not undo the restoration to a stock step-back",
            );
        }
    }

    fn lifetime(&self, restoration: &FirmwareRestoration) -> Option<Lifetime> {
        if !matches!(restoration.outcome, RestorationOutcome::Applied { .. }) {
            return None;
        }
        let steps = &self.record.steps;
        let mut own = steps.iter().enumerate().filter(|(_, step)| {
            matches!(&step.kind, StepKind::Restoration { restoration: id } if *id == restoration.id)
        });
        let (index, application) = own.next()?;
        if own.next().is_some() {
            return None;
        }
        let closing = steps
            .iter()
            .skip(index + 1)
            .find(|step| step.before != step.after);
        Some(Lifetime {
            start: application.day,
            end: closing.map_or(self.rules.horizon_end(), |step| step.day),
            closing_event: closing.and_then(|step| match &step.kind {
                StepKind::Retreat {
                    event: Some(id), ..
                } => Some(id.clone()),
                StepKind::Retreat { event: None, .. }
                | StepKind::Climb { .. }
                | StepKind::Restoration { .. }
                | StepKind::StockReset { .. } => None,
            }),
        })
    }

    /// Whether a daily event happens while the restoration governs the profile. At the application day
    /// only an event naming the restoration follows it; at the closing day only the closing retreat's
    /// own event precedes it.
    fn within(&self, life: &Lifetime, id: &str, event: &MachineEvent) -> bool {
        let tolerance = self.rules.day_tolerance;
        let day = event.policy_event.day;
        let names = matches!(&event.policy_event.newest_step, Some(StepIdentity::Restoration { restoration, .. }) if restoration == id);
        let after = day > life.start + tolerance || (day >= life.start - tolerance && names);
        let before = day < life.end - tolerance
            || (day <= life.end + tolerance
                && life.closing_event.as_deref() == Some(event.policy_event.id.as_str()));
        after && before
    }

    /// Crash obligations come from the recorded events, whatever result the record declares: a charged
    /// CPU-suspect daily event while the restored profile is deployed needs a stock step-back and
    /// permanent stops of the restored units. The declared crash is judged by `crashed`.
    fn restoration_crashes(
        &mut self,
        restoration: &FirmwareRestoration,
        declared: Option<&str>,
        previous: &[u8],
    ) {
        let Some(life) = self.lifetime(restoration) else {
            return;
        };
        let record = self.record;
        let tolerance = self.rules.day_tolerance;
        let id = restoration.id.as_str();
        let restored: Vec<UnitId> = record
            .units
            .iter()
            .filter(|summary| {
                previous
                    .get(summary.unit.index())
                    .is_some_and(|depth| *depth > 0)
            })
            .map(|summary| summary.unit)
            .collect();
        let applicable: Vec<&MachineEvent> = record
            .events
            .iter()
            .filter(|event| {
                let observed = &event.policy_event;
                event.period == Period::Day
                    && observed.charged
                    && (suspect(observed.classification)
                        || observed.reclassified_from.is_some_and(suspect))
                    && declared != Some(observed.id.as_str())
                    && self.within(&life, id, event)
            })
            .collect();
        for event in applicable {
            let observed = &event.policy_event;
            self.violate(
                "restoration-crash-undeclared",
                &observed.id,
                format!("a charged CPU-suspect event happens on the profile restored by {id}, but the result does not record it as the crash"),
            );
            let stepped = record.steps.iter().any(|step| {
                matches!(&step.kind, StepKind::Retreat { event: Some(item), units } if *item == observed.id && restored.iter().all(|unit| units.contains(unit)))
                    && zero(&step.after)
                    && step.day + tolerance >= observed.day
            });
            if !stepped {
                self.violate(
                    "restoration-step-back",
                    id,
                    format!(
                        "no stock step-back follows the charged event {}",
                        observed.id
                    ),
                );
            }
            let permanent = restored.iter().all(|unit| {
                record.policy.stopped_units.contains(unit) && !record.steps.iter().any(|step| {
                    matches!(&step.kind, StepKind::Climb { unit: climbed, .. } if climbed == unit)
                        && step.day > observed.day + tolerance
                })
            });
            if !permanent {
                self.violate(
                    "restoration-crash-stop",
                    id,
                    format!(
                        "restored units are not permanently stopped after the charged event {}",
                        observed.id
                    ),
                );
            }
        }
    }

    /// Restoration-learning tags are exactly the application-to-result span of each applied restoration:
    /// every daily interval and daily event inside it carries the restoration's tag, and nothing else
    /// does. A night event's tag only has to name an applied restoration at or before it.
    fn tags(&mut self) {
        let record = self.record;
        let tolerance = self.rules.day_tolerance;
        let lives: Vec<(&str, Lifetime)> = record
            .restorations
            .iter()
            .filter_map(|restoration| {
                self.lifetime(restoration)
                    .map(|life| (restoration.id.as_str(), life))
            })
            .collect();
        let show =
            |tag: Option<&str>| tag.map_or_else(|| "no tag".to_owned(), |tag| format!("tag {tag}"));
        let mut found: Vec<(String, String)> = Vec::new();
        for interval in record
            .intervals
            .iter()
            .filter(|interval| interval.period == Period::Day)
        {
            let expected = lives
                .iter()
                .find(|(_, life)| {
                    interval.start_day >= life.start - tolerance
                        && interval.start_day < life.end - tolerance
                })
                .map(|(id, _)| *id);
            if interval.restoration_learning.as_deref() != expected {
                found.push((
                    format!("interval at day {}", interval.start_day),
                    format!(
                        "has {}; the restoration application and result require {}",
                        show(interval.restoration_learning.as_deref()),
                        show(expected)
                    ),
                ));
            }
        }
        for event in &record.events {
            let observed = &event.policy_event;
            let tag = event.restoration_learning.as_deref();
            if event.period == Period::Day {
                let expected = lives
                    .iter()
                    .find(|(id, life)| self.within(life, id, event))
                    .map(|(id, _)| *id);
                if tag != expected {
                    found.push((
                        format!("event {}", observed.id),
                        format!(
                            "has {}; the restoration application and result require {}",
                            show(tag),
                            show(expected)
                        ),
                    ));
                }
            } else if let Some(tag) = tag
                && !record.restorations.iter().any(|restoration| {
                    restoration.id == tag
                        && matches!(&restoration.outcome, RestorationOutcome::Applied { day, .. } if observed.day + tolerance >= *day)
                })
            {
                found.push((
                    format!("event {}", observed.id),
                    format!("has tag {tag} without an applied restoration at or before it"),
                ));
            }
        }
        for (subject, detail) in found {
            self.violate("restoration-tag", subject, detail);
        }
    }
}
