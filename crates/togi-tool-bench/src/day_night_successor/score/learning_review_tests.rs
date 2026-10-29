//! Boundary and rejection tests for the learning audit: event-ordered stops and step-backs, same-day
//! charge ordering, restoration vector/qualification/allowance/crash rules and restoration-learning tags.
//!
//! Each case audits one synthetic record and names the exact rules the audit must raise. Expectations
//! are written from the contract in #1457 (§2 and §4), never copied from a scorer run.

use test_case::test_case;
use togi_machine::{CoreId, PerCore};
use togi_simrun::day_night::successor::{
    CeilingEvidence, Classification, EdgeObservation, FinishReason, Qualification, Record,
    RestorationOutcome, RestorationResult, StepIdentity, StepKind, UnitFinish,
};

use super::super::fixture::{Ev, Lab, Restore, Spec, uid};
use super::{Audit, LearningAudit, Rules, audit};

const EDGE: &str = "successor-edge-deep";
const FIRMWARE: &str = "successor-firmware-change";
const SLOPE: f64 = 1.5;

fn lab(scenario: &str) -> Lab {
    Lab::new(&[scenario], 1)
}

fn edge(lab: &Lab, events: Vec<Ev>) -> Record {
    let mut spec = Spec::new(EDGE, 20_001, SLOPE);
    spec.events = events;
    spec.build(lab)
}

fn firmware(lab: &Lab, restore: Restore, events: Vec<Ev>) -> Record {
    let mut spec = Spec::new(FIRMWARE, 20_001, SLOPE).restore(restore);
    spec.events = events;
    spec.build(lab)
}

fn judged(lab: &Lab, record: &Record) -> LearningAudit {
    audit(record, &Rules::new(&lab.registration))
}

fn rules(result: &LearningAudit) -> Vec<&'static str> {
    result
        .violations
        .iter()
        .map(|violation| violation.rule)
        .collect()
}

fn subjects(result: &LearningAudit) -> Vec<&str> {
    result
        .violations
        .iter()
        .map(|violation| violation.subject.as_str())
        .collect()
}

fn identity(name: &str) -> Option<StepIdentity> {
    match name {
        "none" => None,
        "climb-0" | "climb-1" => Some(StepIdentity::Unit {
            unit: uid(usize::from(name == "climb-1")),
            proposal: name.into(),
        }),
        "restoration" => Some(StepIdentity::Restoration {
            restoration: "fw".into(),
            units: vec![uid(0), uid(1)],
        }),
        other => panic!("unknown step identity {other}"),
    }
}

fn uniform(record: &Record, depth: u8) -> PerCore<u8> {
    record
        .provenance
        .topology
        .iter()
        .map(|core| (core.core, depth))
        .collect()
}

fn lower(profile: &mut PerCore<u8>, cores: &[CoreId]) {
    for core in cores {
        if let Some(depth) = profile.get(*core)
            && depth > 0
        {
            profile.set(*core, depth - 1);
        }
    }
}

/// Steps unit 0 back by one count at the day of its promotion, right after that promotion, naming
/// `event`. The later applications and intervals carry the lowered profile.
fn step_back_unit_zero(record: &mut Record, event: &str) {
    let cores = record.provenance.membership[0].cores.clone();
    let before = record.steps[0].after;
    let mut after = before;
    lower(&mut after, &cores);
    let day = record.steps[0].day;
    record.steps.insert(
        1,
        super::ProfileStep {
            id: "retreat-0".into(),
            day,
            before,
            after,
            kind: StepKind::Retreat {
                event: Some(event.into()),
                units: vec![uid(0)],
            },
        },
    );
    for step in record.steps.iter_mut().skip(2) {
        lower(&mut step.before, &cores);
        lower(&mut step.after, &cores);
        if let StepKind::Climb { qualification, .. } = &mut step.kind {
            lower(&mut qualification.profile, &cores);
        }
    }
    for interval in &mut record.intervals {
        lower(&mut interval.profile, &cores);
    }
    record.units[0].retreats.steps += 1;
}

fn unit_event(day: f64) -> Ev {
    Ev {
        newest_step: identity("climb-0"),
        ..Ev::co("e", day, 0)
    }
}

fn qualification(record: &mut Record) -> &mut Qualification {
    match &mut record.restorations[0].outcome {
        RestorationOutcome::Applied { qualification, .. } => qualification,
        RestorationOutcome::Blocked { .. } => unreachable!("an applied restoration"),
    }
}

fn result(record: &mut Record) -> &mut RestorationResult {
    match &mut record.restorations[0].outcome {
        RestorationOutcome::Applied { result, .. } => result,
        RestorationOutcome::Blocked { .. } => unreachable!("an applied restoration"),
    }
}

fn clean_stock_hours(record: &mut Record) -> &mut f64 {
    match &mut record.restorations[0].outcome {
        RestorationOutcome::Applied {
            clean_stock_hours, ..
        } => clean_stock_hours,
        RestorationOutcome::Blocked { .. } => unreachable!("an applied restoration"),
    }
}

// ---- Event-ordered stops and step-backs (R5-3, initial charged-event path) -------------------------

#[test]
fn an_initial_charged_event_that_stops_and_steps_back_its_unit_is_clean() {
    let lab = lab(EDGE);
    let mut record = edge(&lab, vec![unit_event(8.0)]);
    step_back_unit_zero(&mut record, "e");
    assert_eq!(rules(&judged(&lab, &record)), Vec::<&str>::new());
}

#[test]
fn a_charged_event_without_a_step_back_is_refused() {
    let lab = lab(EDGE);
    let record = edge(&lab, vec![unit_event(8.0)]);
    assert_eq!(rules(&judged(&lab, &record)), vec!["event-step-back"]);
}

#[test]
fn a_charged_learning_event_must_name_its_newest_step() {
    let lab = lab(EDGE);
    let record = edge(&lab, vec![Ev::co("e", 8.0, 0)]);
    assert_eq!(rules(&judged(&lab, &record)), vec!["event-step-identity"]);
}

#[test]
fn a_step_back_that_names_another_event_does_not_count() {
    let lab = lab(EDGE);
    let mut record = edge(&lab, vec![unit_event(8.0)]);
    step_back_unit_zero(&mut record, "another");
    assert_eq!(rules(&judged(&lab, &record)), vec!["event-step-back"]);
}

#[test]
fn a_step_back_that_precedes_its_event_does_not_count() {
    let lab = lab(EDGE);
    // The step-back is at day 8, the event at day 9: it cannot answer an event that has not happened.
    // The event also restarts the second promotion's hold, which leaves no verified clean interval.
    let mut record = edge(&lab, vec![unit_event(9.0)]);
    step_back_unit_zero(&mut record, "e");
    assert_eq!(
        rules(&judged(&lab, &record)),
        vec!["promotion-verified-hold", "event-step-back"]
    );
}

#[test_case(FinishReason::CpuSuspectStop, 20.0; "a cpu stop dated after the event")]
#[test_case(FinishReason::AllowanceExhausted, 20.0; "an exhaustion stop dated after the event")]
#[test_case(FinishReason::Ceiling, 20.0; "a ceiling finish dated after the event")]
fn a_finish_after_the_event_does_not_stop_the_unit_at_the_event(reason: FinishReason, day: f64) {
    let lab = lab(EDGE);
    let mut record = edge(&lab, vec![unit_event(8.0)]);
    step_back_unit_zero(&mut record, "e");
    record.units[0].finish = Some(UnitFinish {
        unit: uid(0),
        reason,
        day,
        witnesses: vec!["late".into()],
    });
    assert_eq!(rules(&judged(&lab, &record)), vec!["event-stop"]);
}

#[test_case(FinishReason::Ceiling; "an earlier ceiling finish")]
#[test_case(FinishReason::CpuSuspectStop; "a finish at the event")]
fn a_finish_at_or_before_the_event_stops_the_unit(reason: FinishReason) {
    let lab = lab(EDGE);
    let mut record = edge(&lab, vec![unit_event(8.0)]);
    step_back_unit_zero(&mut record, "e");
    record.units[0].finish = Some(UnitFinish {
        unit: uid(0),
        reason,
        day: 8.0,
        witnesses: vec!["e".into()],
    });
    assert_eq!(rules(&judged(&lab, &record)), Vec::<&str>::new());
}

/// A later finish date cannot reopen the unit: the promotion after the event is refused by itself.
#[test]
fn a_promotion_after_the_event_is_refused_whatever_the_summary_finish() {
    let lab = lab(EDGE);
    let mut record = edge(&lab, vec![unit_event(8.0)]);
    let before = record.steps[1].after;
    let cores = record.provenance.membership[0].cores.clone();
    let mut after = before;
    for core in &cores {
        after.set(*core, 8);
    }
    let StepKind::Climb { qualification, .. } = &record.steps[1].kind else {
        unreachable!("the second application is a promotion")
    };
    let mut qualification = qualification.clone();
    qualification.id = "q-0b".into();
    qualification.day = 22.0;
    qualification.profile = after;
    record.steps.push(super::ProfileStep {
        id: "climb-0b".into(),
        day: 22.0,
        before,
        after,
        kind: StepKind::Climb {
            unit: uid(0),
            ceiling_depth: 10,
            clean_hold_hours: 50.0,
            qualification,
        },
    });
    let mut tail = record.intervals.pop().expect("a last interval");
    let mut head = tail.clone();
    head.end_day = 22.0;
    head.verified_daily_active_hours = 56.0;
    tail.start_day = 22.0;
    tail.profile = after;
    record.intervals.extend([head, tail]);
    record.units[0].promotions = 2;
    record.units[0].finish = Some(UnitFinish {
        unit: uid(0),
        reason: FinishReason::CpuSuspectStop,
        day: 30.0,
        witnesses: vec!["late".into()],
    });
    assert_eq!(
        rules(&judged(&lab, &record)),
        vec!["event-stop", "event-step-back", "promotion-after-stop"]
    );
}

/// Same-day ordering of one application against its event: the event's own step never trails the
/// event, only a later application does.
#[test_case(10.0, Some(2), 3, 9.0, false; "an earlier day is before the event")]
#[test_case(10.0, Some(2), 3, 11.0, true; "a later day is after the event")]
#[test_case(10.0, Some(2), 3, 10.0, true; "the same day, a later application")]
#[test_case(10.0, Some(2), 2, 10.0, false; "the same day, the event's own step")]
#[test_case(10.0, Some(2), 1, 10.0, false; "the same day, an earlier application")]
#[test_case(10.0, None, 3, 10.0, false; "the same day, an unknown cause")]
#[test_case(10.0, Some(2), 3, 10.0 + 1e-10, true; "inside the tolerance, ordered by application")]
#[test_case(10.0, Some(2), 3, 10.0 - 1e-10, true; "inside the tolerance below, ordered by application")]
#[test_case(10.0, Some(2), 2, 10.0 + 1e-10, false; "inside the tolerance, the own step")]
fn applications_follow_an_event_by_day_then_by_order(
    event_day: f64,
    cause: Option<usize>,
    index: usize,
    step_day: f64,
    follows: bool,
) {
    let lab = lab(EDGE);
    let record = edge(&lab, vec![]);
    let rules = Rules::new(&lab.registration);
    let audit = Audit {
        record: &record,
        rules: &rules,
        result: LearningAudit::default(),
    };
    assert_eq!(audit.follows(event_day, cause, index, step_day), follows);
}

// ---- Same-day charge ordering (both step identities) ------------------------------------------

fn same_day_events(first: &str, second: &str, day: f64) -> Vec<Ev> {
    vec![
        Ev {
            newest_step: identity(first),
            ..Ev::co("a", day, 0)
        },
        Ev {
            newest_step: identity(second),
            ..Ev::co("b", day, 1)
        },
    ]
}

/// The second promotion is at day 15 (step 1). A same-day charge counts against it when its newest
/// step is an earlier application or unknown; the promotion's own step never counts.
#[test_case("climb-0", "climb-0", &["promotion-after-exhaustion"]; "two charges from earlier steps")]
#[test_case("none", "climb-0", &["promotion-after-exhaustion"]; "an unknown step counts as earlier")]
#[test_case("climb-0", "climb-1", &[]; "one earlier charge and the own step")]
#[test_case("climb-1", "climb-1", &[]; "two charges of the own step")]
fn a_same_day_charge_counts_against_the_next_promotion_only(
    first: &str,
    second: &str,
    expected: &[&str],
) {
    let lab = lab(EDGE);
    let record = edge(&lab, same_day_events(first, second, 15.0));
    assert_eq!(rules(&judged(&lab, &record)), expected);
}

/// The restoration is step 3 (day 189). The same rule applies with a restoration identity: the crash
/// that the restoration itself caused never counts against it.
#[test_case(&[], &[]; "only the crash of the restoration itself")]
#[test_case(&["climb-0"], &[]; "one earlier same-day charge")]
#[test_case(&["climb-0", "climb-0"], &["restoration-exhausted"]; "two earlier same-day charges")]
#[test_case(&["none", "climb-1"], &["restoration-exhausted"]; "an unknown step and an earlier step")]
fn a_same_day_charge_counts_against_a_restoration_but_its_own_crash_does_not(
    extras: &[&str],
    expected: &[&str],
) {
    let lab = lab(FIRMWARE);
    let mut record = firmware(&lab, Restore::Crashed, vec![]);
    let crash = record
        .events
        .iter()
        .position(|event| event.policy_event.id == "crash")
        .expect("the crash event");
    let application = 189.0;
    record.events[crash].policy_event.day = application;
    for (number, name) in extras.iter().enumerate() {
        let mut extra = record.events[crash].clone();
        extra.policy_event.id = format!("extra-{number}");
        extra.policy_event.newest_step = identity(name);
        extra.restoration_learning = None;
        record.events.push(extra);
    }
    record.events[crash].restoration_learning = Some("fw".into());
    assert_eq!(rules(&judged(&lab, &record)), expected);
}

// ---- Restoration crash obligations from events (R5-2) ------------------------------------------

#[test_case(Classification::CpuSuspect, false, None, true, &["restoration-crash-undeclared", "restoration-step-back", "restoration-crash-stop"]; "cpu suspect")]
#[test_case(Classification::Unexplained, false, None, true, &["restoration-crash-undeclared", "restoration-step-back", "restoration-crash-stop"]; "unexplained")]
#[test_case(Classification::Unanswered, true, None, true, &["restoration-crash-undeclared", "restoration-step-back", "restoration-crash-stop"]; "provisional unanswered")]
#[test_case(Classification::NonCpu, true, Some(Classification::Unanswered), true, &["restoration-crash-undeclared", "restoration-step-back", "restoration-crash-stop"]; "reclassified to non cpu keeps its charge")]
#[test_case(Classification::NonCpu, false, None, false, &[]; "an uncharged non cpu event")]
#[test_case(Classification::Unreported, false, None, false, &[]; "an unreported absence")]
fn a_charged_suspect_event_on_the_restored_profile_owes_a_step_back_and_stops(
    classification: Classification,
    provisional: bool,
    reclassified_from: Option<Classification>,
    charged: bool,
    expected: &[&str],
) {
    let lab = lab(FIRMWARE);
    let event = Ev {
        classification,
        charged,
        provisional,
        reclassified_from,
        ..Ev::co("late", 200.0, 0)
    };
    let record = firmware(&lab, Restore::Retained, vec![event]);
    assert_eq!(rules(&judged(&lab, &record)), expected);
}

#[test]
fn a_crash_after_the_step_back_is_not_a_restoration_event() {
    let lab = lab(FIRMWARE);
    // The crash undoes the restoration at day 191; a later charged event is an ordinary post-finish event.
    let event = Ev::co("after", 200.0, 0);
    let record = firmware(&lab, Restore::Crashed, vec![event]);
    assert_eq!(rules(&judged(&lab, &record)), Vec::<&str>::new());
}

// ---- Restoration: reset vector, qualification, allowance and crash (R5-1, R5-5) -----------------

#[test]
fn a_restoration_must_save_the_profile_deployed_before_the_reset() {
    let lab = lab(FIRMWARE);
    let mut record = firmware(&lab, Restore::Retained, vec![]);
    assert_eq!(rules(&judged(&lab, &record)), Vec::<&str>::new());
    // The reset really starts from depth 5; the record claims, qualifies and restores depth 3.
    let claimed = uniform(&record, 3);
    record.restorations[0].previous_profile = claimed;
    qualification(&mut record).profile = claimed;
    for step in &mut record.steps {
        if matches!(&step.kind, StepKind::Restoration { .. }) {
            step.after = claimed;
        }
    }
    for interval in record
        .intervals
        .iter_mut()
        .filter(|item| item.start_day >= 189.0)
    {
        interval.profile = claimed;
    }
    let result = judged(&lab, &record);
    assert_eq!(rules(&result), vec!["restoration-previous-profile"]);
    assert_eq!(subjects(&result), vec!["fw"]);
}

#[test_case(|record| qualification(record).fresh_witnesses.clear(), &["restoration-new-context"]; "no fresh witness")]
#[test_case(|record| qualification(record).stock_controls.clear(), &["restoration-new-context"]; "no stock control")]
#[test_case(|record| qualification(record).reused_witnesses.push("old".into()), &["restoration-new-context"]; "a reused witness")]
#[test_case(|record| qualification(record).epoch = "before".into(), &["restoration-new-context"]; "the old context")]
#[test_case(|record| qualification(record).safety_checks_satisfied = false, &["restoration-safety"]; "unsatisfied safety checks")]
#[test_case(|record| qualification(record).day = 190.0, &["restoration-order"]; "a qualification after the application")]
#[test_case(|record| qualification(record).day = 180.0, &["restoration-order"]; "a qualification before the stock interval")]
#[test_case(|record| { let stock = uniform(record, 0); qualification(record).profile = stock }, &["restoration-exact-profile"]; "a qualification of another vector")]
#[test_case(|record| *clean_stock_hours(record) = 49.0, &["restoration-stock-hours"]; "too few claimed clean hours")]
#[test_case(|record| record.intervals.iter_mut().find(|item| (item.start_day - 181.0).abs() < 1e-9).expect("stock interval").verified_daily_active_hours = 10.0, &["restoration-stock-interval"]; "too few verified stock hours")]
#[test_case(|record| record.units[0].ceiling_evidence.push(CeilingEvidence { day: 185.0, epoch: "after".into(), edge: EdgeObservation::CensoredWriteLimit {}, ceiling_depth: 2, witnesses: vec!["adverse".into()] }), &["restoration-adverse-ceiling"]; "adverse new context ceiling")]
fn a_restoration_that_breaks_a_rule_is_refused(edit: fn(&mut Record), expected: &[&str]) {
    let lab = lab(FIRMWARE);
    let mut record = firmware(&lab, Restore::Retained, vec![]);
    assert_eq!(record.restorations[0].epoch, "after", "fixture epoch id");
    edit(&mut record);
    assert_eq!(rules(&judged(&lab, &record)), expected);
}

#[test_case(&[("c1", 100.0)], &[]; "one charge before the restoration")]
#[test_case(&[("c1", 100.0), ("c2", 101.0)], &["restoration-exhausted"]; "the allowance is spent")]
fn a_restoration_needs_allowance_left(charges: &[(&str, f64)], expected: &[&str]) {
    let lab = lab(FIRMWARE);
    let events = charges
        .iter()
        .map(|(id, day)| Ev::co(id, *day, 0))
        .collect();
    let record = firmware(&lab, Restore::Retained, events);
    assert_eq!(rules(&judged(&lab, &record)), expected);
}

#[test_case(|record| match result(record) { RestorationResult::Crashed { event, .. } => *event = "missing".into(), RestorationResult::Retained {} => unreachable!("a crash") }, &["restoration-crash-event", "restoration-crash-undeclared"]; "an unknown crash event")]
#[test_case(|record| match result(record) { RestorationResult::Crashed { stock_step_back_day, .. } => *stock_step_back_day = 189.5, RestorationResult::Retained {} => unreachable!("a crash") }, &["restoration-crash-order", "restoration-step-back"]; "a step back before the crash")]
#[test_case(|record| match result(record) { RestorationResult::Crashed { witnesses, .. } => witnesses.clear(), RestorationResult::Retained {} => unreachable!("a crash") }, &["restoration-crash-witness"]; "no stop witness")]
#[test_case(|record| { let restored = record.restorations[0].previous_profile; match result(record) { RestorationResult::Crashed { stock_readback, .. } => *stock_readback = restored, RestorationResult::Retained {} => unreachable!("a crash") } }, &["restoration-step-back"]; "a step back that is not stock")]
#[test_case(|record| record.events[0].policy_event.charged = false, &["charge-refund", "restoration-crash-charge"]; "an uncharged suspect crash")]
#[test_case(|record| { record.events[0].policy_event.charged = false; record.events[0].policy_event.classification = Classification::NonCpu }, &["restoration-crash-charge"]; "a non cpu crash")]
fn a_restoration_crash_that_breaks_a_rule_is_refused(edit: fn(&mut Record), expected: &[&str]) {
    let lab = lab(FIRMWARE);
    let mut record = firmware(&lab, Restore::Crashed, vec![]);
    assert_eq!(rules(&judged(&lab, &record)), Vec::<&str>::new());
    assert_eq!(
        record.events[0].policy_event.id, "crash",
        "fixture crash event"
    );
    edit(&mut record);
    assert_eq!(rules(&judged(&lab, &record)), expected);
}

// ---- Restoration-learning tags (R5-4) ----------------------------------------------------------

#[test]
fn stripping_the_restoration_learning_tags_is_refused() {
    let lab = lab(FIRMWARE);
    let mut record = firmware(&lab, Restore::Retained, vec![]);
    assert_eq!(rules(&judged(&lab, &record)), Vec::<&str>::new());
    for interval in &mut record.intervals {
        interval.restoration_learning = None;
    }
    let result = judged(&lab, &record);
    assert_eq!(rules(&result), vec!["restoration-tag"]);
    assert_eq!(subjects(&result), vec!["interval at day 189"]);
}

#[test]
fn an_interval_before_the_application_cannot_carry_the_tag() {
    let lab = lab(FIRMWARE);
    let mut record = firmware(&lab, Restore::Retained, vec![]);
    record
        .intervals
        .iter_mut()
        .find(|interval| (interval.start_day - 181.0).abs() < 1e-9)
        .expect("the stock interval")
        .restoration_learning = Some("fw".into());
    let result = judged(&lab, &record);
    assert_eq!(rules(&result), vec!["restoration-tag"]);
    assert_eq!(subjects(&result), vec!["interval at day 181"]);
}

#[test]
fn an_interval_after_the_step_back_cannot_carry_the_tag() {
    let lab = lab(FIRMWARE);
    let mut record = firmware(&lab, Restore::Crashed, vec![]);
    assert_eq!(rules(&judged(&lab, &record)), Vec::<&str>::new());
    record
        .intervals
        .last_mut()
        .expect("the final interval")
        .restoration_learning = Some("fw".into());
    let result = judged(&lab, &record);
    assert_eq!(rules(&result), vec!["restoration-tag"]);
    assert_eq!(subjects(&result), vec!["interval at day 191"]);
}

#[test]
fn a_blocked_restoration_governs_no_interval() {
    let lab = lab(FIRMWARE);
    let mut record = firmware(&lab, Restore::Blocked, vec![]);
    assert_eq!(rules(&judged(&lab, &record)), Vec::<&str>::new());
    record
        .intervals
        .last_mut()
        .expect("the final interval")
        .restoration_learning = Some("fw".into());
    let result = judged(&lab, &record);
    assert_eq!(rules(&result), vec!["restoration-tag"]);
    assert_eq!(subjects(&result), vec!["interval at day 181"]);
}

#[test]
fn the_crash_event_carries_the_tag_and_losing_it_is_refused() {
    let lab = lab(FIRMWARE);
    let mut record = firmware(&lab, Restore::Crashed, vec![]);
    record.events[0].restoration_learning = None;
    let result = judged(&lab, &record);
    assert_eq!(rules(&result), vec!["restoration-tag"]);
    assert_eq!(subjects(&result), vec!["event crash"]);
}

#[test_case(200.0, Some("fw"), 0; "inside the span and tagged")]
#[test_case(200.0, None, 1; "inside the span and untagged")]
#[test_case(200.0, Some("other"), 1; "inside the span and tagged with another id")]
#[test_case(100.0, Some("fw"), 1; "before the application and tagged")]
#[test_case(100.0, None, 0; "before the application and untagged")]
fn a_daily_event_carries_the_tag_exactly_inside_the_restoration_span(
    day: f64,
    tag: Option<&str>,
    violations: usize,
) {
    let lab = lab(FIRMWARE);
    let mut record = firmware(&lab, Restore::Retained, vec![Ev::background("b", day)]);
    record.events[0].restoration_learning = tag.map(str::to_owned);
    let result = judged(&lab, &record);
    assert_eq!(
        (result.violations.len(), rules(&result).first().copied()),
        (violations, (violations > 0).then_some("restoration-tag")),
        "{:?}",
        result.violations
    );
}

// ---- Qualification context (observable context, not hidden truth epoch id) --------------------------

fn first_climb_qualification(record: &mut Record) -> &mut Qualification {
    match &mut record.steps[0].kind {
        StepKind::Climb { qualification, .. } => qualification,
        StepKind::Restoration { .. } | StepKind::Retreat { .. } | StepKind::StockReset { .. } => {
            unreachable!("the first application is a promotion")
        }
    }
}

/// A moving-limit epoch keeps the observable context, so citing the other truth epoch is clean.
#[test]
fn a_qualification_citing_another_truth_epoch_of_the_same_context_is_clean() {
    let lab = lab("successor-moving-cap");
    let mut record = Spec::new("successor-moving-cap", 20_001, SLOPE).build(&lab);
    assert_eq!(record.epochs[0].context, record.epochs[1].context);
    let other = record.epochs[1].id.clone();
    first_climb_qualification(&mut record).epoch = other;
    assert_eq!(rules(&judged(&lab, &record)), Vec::<&str>::new());
}

/// A firmware change opens a new context, so citing the other side's epoch fails.
#[test]
fn a_qualification_citing_the_other_firmware_context_is_refused() {
    let lab = lab(FIRMWARE);
    let mut record = firmware(&lab, Restore::Retained, vec![]);
    assert_ne!(record.epochs[0].context, record.epochs[1].context);
    let other = record.epochs[1].id.clone();
    first_climb_qualification(&mut record).epoch = other;
    assert!(rules(&judged(&lab, &record)).contains(&"qualification-context"));
}

#[test]
fn a_qualification_citing_an_unknown_epoch_is_refused() {
    let lab = lab(EDGE);
    let mut record = edge(&lab, vec![]);
    first_climb_qualification(&mut record).epoch = "unknown".into();
    assert!(rules(&judged(&lab, &record)).contains(&"qualification-context"));
}
