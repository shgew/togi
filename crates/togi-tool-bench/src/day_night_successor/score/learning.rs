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
    Classification, FinishReason, FirmwareRestoration, MachineEvent, Period, Phase, ProfileStep,
    Qualification, Record, RestorationOutcome, RestorationResult, StepIdentity, StepKind, UnitId,
};

use super::rules::{MAX_STEP, Rules};

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
    pub violations: Vec<Violation>,
    pub promotions: Vec<PromotionView>,
    pub restorations: Vec<RestorationView>,
}

pub fn audit(record: &Record, rules: &Rules) -> LearningAudit {
    let mut audit = Audit {
        record,
        rules,
        result: LearningAudit::default(),
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

fn zero(profile: &PerCore<u8>) -> bool {
    profile.iter().all(|(_, depth)| depth == 0)
}

/// Every classification except `NonCpu` keeps the event CPU-suspect for charging.
fn suspect(classification: Classification) -> bool {
    match classification {
        Classification::CpuSuspect | Classification::Unanswered | Classification::Unexplained => {
            true
        }
        Classification::NonCpu => false,
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
        let ledger = &self.record.policy.allowance;
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
        for (index, interval) in record.intervals.iter().enumerate() {
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
                    interval.start_day >= hold_start - self.rules.day_tolerance
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

    /// A learning-phase charged event steps its newest unit back and stops it.
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
            let Some(StepIdentity::Unit { unit, .. }) = &observed.newest_step else {
                continue;
            };
            let stopped = record
                .units
                .get(unit.index())
                .and_then(|summary| summary.finish.as_ref())
                .is_some_and(|finish| match finish.reason {
                    FinishReason::Ceiling => finish.day <= observed.day + tolerance,
                    FinishReason::CpuSuspectStop
                    | FinishReason::AllowanceExhausted
                    | FinishReason::InheritedStop
                    | FinishReason::FirmwareRestorationStop => {
                        finish.day + tolerance >= observed.day
                    }
                });
            if !stopped {
                self.violate(
                    "event-stop",
                    &observed.id,
                    format!("unit {} is not stopped by its charged event", unit.get()),
                );
            }
            let retreated = record.steps.iter().any(|step| {
                matches!(&step.kind, StepKind::Retreat { event: Some(id), units } if *id == observed.id && units.contains(unit))
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
        }
    }

    fn stock_hours(&self, from: f64, until: f64) -> (f64, bool) {
        let mut hours = 0.0;
        let mut stock = true;
        for interval in &self.record.intervals {
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
            let reset_step = record.steps.iter().any(|step| {
                matches!(&step.kind, StepKind::StockReset { epoch } if *epoch == restoration.epoch)
                    && self.rules.same_day(step.day, restoration.stock_reset_day)
                    && zero(&step.after)
            });
            if !reset_step {
                self.violate(
                    "restoration-stock-step",
                    id,
                    "no stock-reset step at the recorded reset day",
                );
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

    /// Restoration-learning tags name an applied restoration and never precede its application.
    fn tags(&mut self) {
        let record = self.record;
        let tolerance = self.rules.day_tolerance;
        let applied = |tag: &str| {
            record
                .restorations
                .iter()
                .find_map(|restoration| match &restoration.outcome {
                    RestorationOutcome::Applied { day, .. } if restoration.id == tag => Some(*day),
                    RestorationOutcome::Applied { .. } | RestorationOutcome::Blocked { .. } => None,
                })
        };
        let mut found = Vec::new();
        for interval in &record.intervals {
            if let Some(tag) = &interval.restoration_learning
                && applied(tag).is_none_or(|day| interval.start_day + tolerance < day)
            {
                found.push(format!("interval at day {}", interval.start_day));
            }
        }
        for event in &record.events {
            let tag_bad = |event: &MachineEvent| {
                event.restoration_learning.as_deref().is_some_and(|tag| {
                    applied(tag).is_none_or(|day| event.policy_event.day + tolerance < day)
                })
            };
            if tag_bad(event) {
                found.push(format!("event {}", event.policy_event.id));
            }
        }
        for subject in found {
            self.violate(
                "restoration-tag",
                subject,
                "restoration-learning tag without an applied restoration at or before it",
            );
        }
    }
}
