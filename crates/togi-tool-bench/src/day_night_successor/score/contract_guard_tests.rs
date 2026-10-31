//! Negative guards of the learning audit that no other test pins: the ordinary climb qualification,
//! structural application rules, hold claims and restarts, sticky finishes, firmware reset and
//! restoration application rules, crash stops and night restoration tags.
//!
//! Every case audits one synthetic record and names the exact set of rules the audit must raise, written
//! from the contract in #1457 (§2 and §4), never copied from a scorer run. Coherent records are checked
//! with `Record::validate` first, so a case fails for the rule it targets and not for a broken
//! representation. The few cases that cannot be coherent say so where they are.

use test_case::test_case;
use togi_machine::PerCore;
use togi_simrun::day_night::successor::{
    Classification, FinishReason, Period, ProfileStep, Qualification, Record, RestorationOutcome,
    RestorationResult, StepIdentity, StepKind, UnitFinish,
};

use super::super::fixture::{Ev, Lab, Restore, Spec, resync, uid};
use super::{Rules, audit};

const EDGE: &str = "successor-edge-deep";
const FIRMWARE: &str = "successor-firmware-change";
const SLOPE: f64 = 1.5;
/// Day of the extra application appended to a fixture record, inside its last interval.
const LATE: f64 = 30.0;

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

/// The distinct rules the audit raises, compared as a sorted set.
fn assert_rules(lab: &Lab, record: &Record, expected: &[&str]) {
    let mut found: Vec<&str> = audit(record, &Rules::new(&lab.registration))
        .violations
        .iter()
        .map(|violation| violation.rule)
        .collect();
    found.sort_unstable();
    found.dedup();
    let mut wanted = expected.to_vec();
    wanted.sort_unstable();
    wanted.dedup();
    assert_eq!(found, wanted);
}

fn coherent(record: &Record) {
    record
        .validate()
        .unwrap_or_else(|error| panic!("the synthetic record is invalid: {error}"));
}

fn uniform(record: &Record, depth: u8) -> PerCore<u8> {
    record
        .provenance
        .topology
        .iter()
        .map(|core| (core.core, depth))
        .collect()
}

/// `base` with every core of one tuning unit set to `depth`.
fn with_unit(record: &Record, base: PerCore<u8>, unit: usize, depth: u8) -> PerCore<u8> {
    let mut profile = base;
    for core in &record.provenance.membership[unit].cores {
        profile.set(*core, depth);
    }
    profile
}

fn climb_qualification(record: &mut Record, index: usize) -> &mut Qualification {
    match &mut record.steps[index].kind {
        StepKind::Climb { qualification, .. } => qualification,
        StepKind::Restoration { .. }
        | StepKind::ReclassifiedRestoration { .. }
        | StepKind::Retreat { .. }
        | StepKind::StockReset { .. } => {
            unreachable!("a promotion")
        }
    }
}

fn climb_hold(record: &mut Record, index: usize) -> &mut f64 {
    match &mut record.steps[index].kind {
        StepKind::Climb {
            clean_hold_hours, ..
        } => clean_hold_hours,
        StepKind::Restoration { .. }
        | StepKind::ReclassifiedRestoration { .. }
        | StepKind::Retreat { .. }
        | StepKind::StockReset { .. } => {
            unreachable!("a promotion")
        }
    }
}

/// Appends one application at day [`LATE`] from the final deployed profile and splits the last
/// interval there, so the chain, the hours and the accounts stay coherent.
fn append_step(record: &mut Record, kind: StepKind, after: PerCore<u8>) {
    let mut tail = record.intervals.pop().expect("a last interval");
    let mut head = tail.clone();
    let share = (LATE - tail.start_day) / (tail.end_day - tail.start_day);
    head.end_day = LATE;
    head.verified_daily_active_hours *= share;
    for risk in &mut head.sources {
        risk.expected_failures *= share;
    }
    tail.start_day = LATE;
    tail.profile = after;
    tail.verified_daily_active_hours *= 1.0 - share;
    for risk in &mut tail.sources {
        risk.expected_failures *= 1.0 - share;
    }
    record.steps.push(ProfileStep {
        id: "extra".into(),
        day: LATE,
        before: head.profile,
        after,
        kind,
    });
    record.intervals.extend([head, tail]);
    resync(record);
}

fn refinish(record: &mut Record, unit: usize, edit: impl FnOnce(&mut UnitFinish)) {
    edit(record.units[unit].finish.as_mut().expect("a unit finish"));
    record.policy.finishes = record
        .units
        .iter()
        .filter_map(|summary| summary.finish.clone())
        .collect();
}

fn deploy_from(record: &mut Record, day: f64, profile: PerCore<u8>) {
    for interval in record
        .intervals
        .iter_mut()
        .filter(|interval| interval.start_day >= day)
    {
        interval.profile = profile;
    }
}

fn strip_tags(record: &mut Record) {
    for interval in &mut record.intervals {
        interval.restoration_learning = None;
    }
    for event in &mut record.events {
        event.restoration_learning = None;
    }
}

fn reset_step(record: &mut Record) -> &mut ProfileStep {
    record
        .steps
        .iter_mut()
        .find(|step| matches!(&step.kind, StepKind::StockReset { .. }))
        .expect("the stock-reset step")
}

fn restore_position(record: &Record) -> usize {
    record
        .steps
        .iter()
        .position(|step| matches!(&step.kind, StepKind::Restoration { .. }))
        .expect("the restoration step")
}

fn crash_result(record: &mut Record) -> &mut RestorationResult {
    match &mut record.restorations[0].outcome {
        RestorationOutcome::Applied { result, .. } => result,
        RestorationOutcome::Blocked { .. } => unreachable!("an applied restoration"),
    }
}

/// Moves the recorded application day, and the qualification with it, away from its step.
fn reapply(record: &mut Record, applied: f64) {
    if let RestorationOutcome::Applied {
        day, qualification, ..
    } = &mut record.restorations[0].outcome
    {
        *day = applied;
        qualification.day = applied;
    }
}

// ---- Baselines: every coherent record the cases below break --------------------------------------

#[test]
fn the_edge_record_is_a_coherent_clean_baseline() {
    let lab = lab(EDGE);
    let record = edge(&lab, vec![]);
    coherent(&record);
    assert_rules(&lab, &record, &[]);
}

/// A retained restoration's tagged interval starts at the application day, where the hold ends: it only
/// touches the stock hold at its end and is not part of it.
#[test_case(Restore::Retained; "retained")]
#[test_case(Restore::Blocked; "blocked")]
#[test_case(Restore::Crashed; "crashed")]
fn the_firmware_records_are_coherent_clean_baselines(restore: Restore) {
    let lab = lab(FIRMWARE);
    let record = firmware(&lab, restore, vec![]);
    coherent(&record);
    assert_rules(&lab, &record, &[]);
}

// ---- Ordinary climb qualification (not the firmware restoration qualification) -------------------

#[test_case(|record| { let stale = record.steps[0].after; climb_qualification(record, 1).profile = stale }, &["qualification-profile"]; "a qualification of the previous profile")]
#[test_case(|record| { let stock = uniform(record, 0); climb_qualification(record, 1).profile = stock }, &["qualification-profile"]; "a qualification of the stock profile")]
#[test_case(|record| climb_qualification(record, 1).safety_checks_satisfied = false, &["qualification-safety"]; "unsatisfied safety checks")]
#[test_case(|record| { let item = climb_qualification(record, 1); item.fresh_witnesses.clear(); item.reused_witnesses.clear() }, &["qualification-witness"]; "no witness at all")]
#[test_case(|record| { let item = climb_qualification(record, 1); item.fresh_witnesses.clear(); item.reused_witnesses.push("old".into()) }, &[]; "a reused witness stands in for a fresh one")]
#[test_case(|record| climb_qualification(record, 1).day += 1.0, &["qualification-order"]; "a qualification a day after its application")]
#[test_case(|record| climb_qualification(record, 1).day += 1e-6, &["qualification-order"]; "a qualification just past the tolerance")]
#[test_case(|record| climb_qualification(record, 1).day += 5e-10, &[]; "a qualification inside the tolerance")]
#[test_case(|record| climb_qualification(record, 1).day -= 3.0, &[]; "a qualification before its application")]
fn an_ordinary_climb_qualification_that_breaks_a_rule_is_refused(
    edit: fn(&mut Record),
    expected: &[&str],
) {
    let lab = lab(EDGE);
    let mut record = edge(&lab, vec![]);
    edit(&mut record);
    coherent(&record);
    assert_rules(&lab, &record, expected);
}

/// The firmware fixture has two truth epochs, the first climb is at day 8 in `before`.
#[test_case(|record| climb_qualification(record, 0).epoch = "after".into(), &["qualification-context", "qualification-date-context"]; "the epoch of the other context")]
#[test_case(|record| { let item = climb_qualification(record, 0); item.epoch = "after".into(); item.day = 185.0 }, &["qualification-context", "qualification-order"]; "the other epoch with a date inside it")]
#[test_case(|record| climb_qualification(record, 0).day = 185.0, &["qualification-date-context", "qualification-order"]; "the right epoch dated in the other context")]
fn an_ordinary_climb_qualification_must_belong_to_the_application_context(
    edit: fn(&mut Record),
    expected: &[&str],
) {
    let lab = lab(FIRMWARE);
    let mut record = firmware(&lab, Restore::Retained, vec![]);
    edit(&mut record);
    coherent(&record);
    assert_rules(&lab, &record, expected);
}

// ---- Structural step guards ------------------------------------------------------------------

#[test_case(6, &["retreat-direction"]; "a retreat that deepens a unit")]
#[test_case(5, &[]; "a retreat that holds the profile")]
#[test_case(4, &[]; "a retreat that lowers a unit")]
fn a_retreat_may_not_deepen_a_core(depth: u8, expected: &[&str]) {
    let lab = lab(EDGE);
    let mut record = edge(&lab, vec![]);
    let last = record.intervals.last().expect("a last interval").profile;
    let after = with_unit(&record, last, 0, depth);
    let kind = StepKind::Retreat {
        event: None,
        units: vec![uid(0)],
    };
    append_step(&mut record, kind, after);
    record.units[0].retreats.steps += 1;
    coherent(&record);
    assert_rules(&lab, &record, expected);
}

#[test_case(0, &[]; "a reset to stock")]
#[test_case(1, &["stock-reset-profile"]; "a reset that leaves one count everywhere")]
#[test_case(4, &["stock-reset-profile"]; "a reset that leaves four counts everywhere")]
fn a_stock_reset_must_end_at_stock(depth: u8, expected: &[&str]) {
    let lab = lab(EDGE);
    let mut record = edge(&lab, vec![]);
    let after = uniform(&record, depth);
    let kind = StepKind::StockReset {
        epoch: "only".into(),
    };
    append_step(&mut record, kind, after);
    coherent(&record);
    assert_rules(&lab, &record, expected);
}

/// The second climb promotes unit 1 and rewrites the other unit's depth with it.
#[test_case(6, &["promotion-single-unit"]; "the other unit is deepened")]
#[test_case(4, &["promotion-single-unit"]; "the other unit is lowered")]
#[test_case(5, &[]; "the other unit keeps its depth")]
fn a_promotion_changes_only_its_own_unit(other: u8, expected: &[&str]) {
    let lab = lab(EDGE);
    let mut record = edge(&lab, vec![]);
    let widened = with_unit(&record, record.steps[1].after, 0, other);
    record.steps[1].after = widened;
    climb_qualification(&mut record, 1).profile = widened;
    record
        .intervals
        .last_mut()
        .expect("a last interval")
        .profile = widened;
    record.policy.readback = Some(widened);
    coherent(&record);
    assert_rules(&lab, &record, expected);
}

// ---- Hold claims and hold restarts ---------------------------------------------------------------

/// The fixture holds 56 verified clean hours before each of the two climbs; the registered hold is 50.
#[test_case(0, 49.0, &["promotion-hold"]; "a first step claiming too few hours")]
#[test_case(1, 49.0, &["promotion-hold"]; "a later step claiming too few hours")]
#[test_case(1, 0.0, &["promotion-hold"]; "a later step claiming no hold")]
#[test_case(1, 50.0 - 5e-10, &[]; "a claim inside the tolerance of the minimum")]
#[test_case(1, 56.0, &[]; "a claim equal to the verified hours")]
#[test_case(0, 57.0, &["promotion-verified-hold"]; "a first step claiming more than was verified")]
#[test_case(1, 57.0, &["promotion-verified-hold"]; "a later step claiming more than was verified")]
fn a_claimed_hold_needs_the_minimum_and_the_verified_hours(
    index: usize,
    claimed: f64,
    expected: &[&str],
) {
    let lab = lab(EDGE);
    let mut record = edge(&lab, vec![]);
    *climb_hold(&mut record, index) = claimed;
    coherent(&record);
    assert_rules(&lab, &record, expected);
}

/// An uncharged event inside a climb's hold restarts the hold at the event when it is provisional,
/// reclassified from a suspect classification or suspect, leaving no verified clean interval; a
/// non-suspect event does not. Day 4 is on the stock profile inside the first climb's hold (days 1 to 8),
/// day 11 on the first climb's profile inside the second climb's hold (days 8 to 15).
#[test_case(4.0, Classification::NonCpu, false, None, &[]; "a plain non cpu event")]
#[test_case(4.0, Classification::NonCpu, true, None, &["promotion-verified-hold"]; "a provisional event")]
#[test_case(4.0, Classification::NonCpu, false, Some(Classification::Unanswered), &["promotion-verified-hold"]; "an event reclassified from a suspect one")]
#[test_case(4.0, Classification::NonCpu, false, Some(Classification::NonCpu), &[]; "an event reclassified from a non suspect one")]
#[test_case(4.0, Classification::CpuSuspect, false, None, &["promotion-verified-hold"]; "a cpu suspect event")]
#[test_case(11.0, Classification::NonCpu, true, None, &["promotion-verified-hold"]; "a provisional event inside the later hold")]
fn a_provisional_or_reclassified_event_restarts_the_clean_hold(
    day: f64,
    classification: Classification,
    provisional: bool,
    reclassified_from: Option<Classification>,
    expected: &[&str],
) {
    let lab = lab(EDGE);
    let event = Ev {
        classification,
        provisional,
        reclassified_from,
        ..Ev::background("hold", day)
    };
    let record = edge(&lab, vec![event]);
    coherent(&record);
    assert_rules(&lab, &record, expected);
}

// ---- Sticky finish --------------------------------------------------------------------------------

/// Unit 1 finished at day 15 without any charged event. Whatever the finish reason, a later promotion
/// of that unit cannot reopen it.
#[test_case(FinishReason::Ceiling; "a ceiling finish")]
#[test_case(FinishReason::CpuSuspectStop; "a cpu stop")]
#[test_case(FinishReason::AllowanceExhausted; "an allowance stop")]
fn a_finished_unit_cannot_be_promoted_again_without_a_charged_event(reason: FinishReason) {
    let lab = lab(EDGE);
    let mut record = edge(&lab, vec![]);
    refinish(&mut record, 1, |finish| finish.reason = reason);
    let last = record.intervals.last().expect("a last interval").profile;
    let after = with_unit(&record, last, 1, 10);
    let kind = StepKind::Climb {
        unit: uid(1),
        ceiling_depth: 15,
        clean_hold_hours: 50.0,
        qualification: Qualification {
            id: "q-1b".into(),
            day: LATE,
            epoch: "only".into(),
            profile: after,
            fresh_witnesses: vec!["fresh-1b".into()],
            reused_witnesses: vec![],
            safety_checks_satisfied: true,
        },
    };
    append_step(&mut record, kind, after);
    record.units[1].promotions += 1;
    coherent(&record);
    assert_rules(&lab, &record, &["promotion-after-finish"]);
}

// ---- Firmware reset: stock readback, reset day and reset step -----------------------------------

#[test_case(|record| { let readback = uniform(record, 1); record.restorations[0].stock_readback = readback }, &["restoration-stock"]; "a readback that is not stock")]
#[test_case(|record| record.restorations[0].stock_reset_day = 180.0, &["restoration-stock", "restoration-stock-step"]; "a reset dated before the transition")]
#[test_case(|record| record.restorations[0].stock_reset_day = 181.0 - 5e-10, &[]; "a reset inside the tolerance before the transition")]
#[test_case(|record| record.restorations[0].stock_reset_day = 185.0, &["restoration-stock-step"]; "a reset dated away from its step")]
#[test_case(|record| reset_step(record).kind = StepKind::StockReset { epoch: "before".into() }, &["restoration-stock-step"]; "a reset step of another epoch")]
#[test_case(|record| { let target = uniform(record, 1); reset_step(record).after = target; deploy_from(record, 181.0, target) }, &["stock-reset-profile", "restoration-stock-step"]; "a reset step that does not end at stock")]
fn a_firmware_reset_needs_a_stock_readback_and_its_reset_step(
    edit: fn(&mut Record),
    expected: &[&str],
) {
    let lab = lab(FIRMWARE);
    let mut record = firmware(&lab, Restore::Blocked, vec![]);
    edit(&mut record);
    coherent(&record);
    assert_rules(&lab, &record, expected);
}

/// A non-stock interval between the reset and the application cannot be part of the stock hold. The
/// chain rules also see it: the record is coherent in form but cannot be coherent in meaning.
#[test]
fn a_non_stock_interval_inside_the_restoration_hold_is_refused() {
    let lab = lab(FIRMWARE);
    let mut record = firmware(&lab, Restore::Retained, vec![]);
    let deeper = uniform(&record, 1);
    record
        .intervals
        .iter_mut()
        .find(|interval| (interval.start_day - 181.0).abs() < 1e-9)
        .expect("the stock interval")
        .profile = deeper;
    coherent(&record);
    assert_rules(
        &lab,
        &record,
        &["interval-chain", "step-chain", "restoration-stock-interval"],
    );
}

// ---- Restoration application: blocked, missing, duplicate and whole-profile ------------------

#[test_case(|record| { record.restorations[0].outcome = RestorationOutcome::Blocked { reason: "synthetic: blocked".into() }; strip_tags(record) }, &["restoration-blocked-applied"]; "a blocked restoration that also has its application")]
#[test_case(|record| { let stock = uniform(record, 0); record.steps.retain(|step| !matches!(&step.kind, StepKind::Restoration { .. })); deploy_from(record, 189.0, stock); strip_tags(record) }, &["restoration-single-step"]; "an applied restoration without its application")]
#[test_case(|record| { let at = restore_position(record); let mut again = record.steps[at].clone(); again.id = "restore-again".into(); again.before = again.after; record.steps.insert(at + 1, again); strip_tags(record) }, &["restoration-single-step"]; "an applied restoration applied twice")]
#[test_case(|record| { let target = uniform(record, 3); let at = restore_position(record); record.steps[at].after = target; deploy_from(record, 189.0, target) }, &["restoration-whole-profile"]; "an application of another vector than the saved profile")]
#[test_case(|record| { let before = uniform(record, 1); let at = restore_position(record); record.steps[at].before = before }, &["restoration-whole-profile", "step-chain"]; "an application that does not start at the stock readback")]
#[test_case(|record| reapply(record, 188.0), &["restoration-whole-profile"]; "an application dated away from its step")]
#[test_case(|record| reapply(record, 189.0 - 5e-10), &[]; "an application inside the tolerance of its step")]
fn a_restoration_is_one_whole_profile_application(edit: fn(&mut Record), expected: &[&str]) {
    let lab = lab(FIRMWARE);
    let mut record = firmware(&lab, Restore::Retained, vec![]);
    edit(&mut record);
    coherent(&record);
    assert_rules(&lab, &record, expected);
}

// ---- Declared and reclassified crashes, and stop membership -------------------------------------

/// The crash is event 0. A crash reclassified from a suspect classification still counts as the charged
/// CPU-suspect crash and keeps its stop obligations; one reclassified from a non-suspect one does not.
#[test_case(|record| { record.events[0].policy_event.classification = Classification::NonCpu; record.events[0].policy_event.reclassified_from = Some(Classification::Unanswered); resync(record) }, &[]; "a crash reclassified from a suspect classification")]
#[test_case(|record| { record.events[0].policy_event.classification = Classification::NonCpu; record.events[0].policy_event.reclassified_from = Some(Classification::NonCpu); resync(record) }, &["restoration-crash-charge"]; "a crash reclassified from a non suspect classification")]
#[test_case(|record| { record.events[0].policy_event.classification = Classification::NonCpu; record.events[0].policy_event.reclassified_from = Some(Classification::Unanswered); record.policy.stopped_units.clear(); resync(record) }, &["restoration-crash-stop"]; "a reclassified crash that stops no unit")]
#[test_case(|record| record.policy.stopped_units.retain(|unit| *unit != uid(1)), &["restoration-crash-stop"]; "a restored unit missing from the permanent stops")]
#[test_case(|record| { if let RestorationResult::Crashed { stopped_units, .. } = crash_result(record) { *stopped_units = vec![uid(0)] } record.events[0].policy_event.newest_step = Some(StepIdentity::Restoration { restoration: "fw".into(), units: vec![uid(0)] }); for step in &mut record.steps { if let StepKind::Retreat { units, .. } = &mut step.kind { *units = vec![uid(0)] } } record.units[1].retreats.steps = 0; resync(record) }, &["restoration-crash-stop"]; "a crash that stops only one of two restored units")]
fn a_declared_crash_keeps_its_charge_and_stop_obligations(
    edit: fn(&mut Record),
    expected: &[&str],
) {
    let lab = lab(FIRMWARE);
    let mut record = firmware(&lab, Restore::Crashed, vec![]);
    assert_eq!(record.events[0].policy_event.id, "crash", "fixture crash");
    edit(&mut record);
    coherent(&record);
    assert_rules(&lab, &record, expected);
}

/// A stopped unit keeps the finish it had before the firmware change. Moving that finish past the
/// transition cannot be a coherent record (the first-finish phases move with it), so this case audits the
/// edited record without validating it.
#[test]
fn a_crash_does_not_move_the_finish_of_a_stopped_unit() {
    let lab = lab(FIRMWARE);
    let mut record = firmware(&lab, Restore::Crashed, vec![]);
    refinish(&mut record, 0, |finish| finish.day = 185.0);
    assert_rules(&lab, &record, &["restoration-crash-finish"]);
}

// ---- Night restoration tags ------------------------------------------------------------------

fn tag_night(record: &mut Record, tag: Option<&str>) {
    for event in record
        .events
        .iter_mut()
        .filter(|event| event.period == Period::Night)
    {
        event.restoration_learning = tag.map(str::to_owned);
    }
}

#[test_case(195.0, None, &[]; "an untagged night event after the application")]
#[test_case(195.0, Some("fw"), &[]; "a tag after the application")]
#[test_case(100.0, None, &[]; "an untagged night event before the application")]
#[test_case(100.0, Some("fw"), &["restoration-tag"]; "a tag before the application")]
fn a_night_event_tag_needs_an_applied_restoration_at_or_before_it(
    day: f64,
    tag: Option<&str>,
    expected: &[&str],
) {
    let lab = lab(FIRMWARE);
    let mut record = firmware(&lab, Restore::Retained, vec![Ev::night("night", day)]);
    tag_night(&mut record, tag);
    coherent(&record);
    assert_rules(&lab, &record, expected);
}

#[test]
fn a_night_event_cannot_carry_the_tag_of_a_blocked_restoration() {
    let lab = lab(FIRMWARE);
    let mut record = firmware(&lab, Restore::Blocked, vec![Ev::night("night", 200.0)]);
    tag_night(&mut record, Some("fw"));
    coherent(&record);
    assert_rules(&lab, &record, &["restoration-tag"]);
}

/// A tag naming no restoration at all fails the representation too; the audit still refuses it.
#[test]
fn a_night_event_cannot_carry_an_unknown_restoration_tag() {
    let lab = lab(FIRMWARE);
    let mut record = firmware(&lab, Restore::Retained, vec![Ev::night("night", 195.0)]);
    tag_night(&mut record, Some("ghost"));
    assert_rules(&lab, &record, &["restoration-tag"]);
}
