//! Closing-day membership of a restoration lifetime.
//!
//! An event at the day a restoration is closed happened on the restored profile, the physical
//! pre-interruption profile of that boundary, whatever event the closing retreat claims to answer.
//! Each case is a coherent record that passes `Record::validate`: a charged CPU-suspect event on the
//! restored depth-5 profile at day 200, closed by a depth-4 retreat at the same day, with the result
//! declared retained and no unit stopped.

use test_case::test_case;
use togi_machine::PerCore;
use togi_simrun::day_night::successor::{ProfileStep, Record, StepIdentity, StepKind};

use super::super::fixture::{Ev, Lab, Restore, Spec, resync, uid};
use super::{Rules, audit};

const FIRMWARE: &str = "successor-firmware-change";
const SLOPE: f64 = 1.5;
const APPLIED: f64 = 189.0;
const CLOSED: f64 = 200.0;

fn uniform(record: &Record, depth: u8) -> PerCore<u8> {
    record
        .provenance
        .topology
        .iter()
        .map(|core| (core.core, depth))
        .collect()
}

/// A validated record whose restoration at day 189 is closed at day 200 by a retreat to depth 4 that
/// names `retreat_event`, while the charged event `c` of day 200 ran on the restored depth-5 profile.
fn closed_on_charged_event(lab: &Lab, retreat_event: Option<&str>, tagged: bool) -> Record {
    let crash = Ev {
        newest_step: Some(StepIdentity::Restoration {
            restoration: "fw".into(),
            units: vec![uid(0), uid(1)],
        }),
        ..Ev::co("c", CLOSED, 0)
    };
    let mut spec = Spec::new(FIRMWARE, 20_001, SLOPE).restore(Restore::Retained);
    spec.events = vec![Ev::background("other", 195.0), crash];
    let mut record = spec.build(lab);

    let at = record
        .intervals
        .iter()
        .position(|interval| interval.start_day == APPLIED)
        .expect("the restored interval");
    let whole = record.intervals[at].clone();
    let head = (CLOSED - whole.start_day) / (whole.end_day - whole.start_day);
    let mut first = whole.clone();
    first.end_day = CLOSED;
    first.verified_daily_active_hours *= head;
    for risk in &mut first.sources {
        risk.expected_failures *= head;
    }
    let mut second = whole;
    second.start_day = CLOSED;
    second.profile = uniform(&record, 4);
    second.restoration_learning = None;
    second.verified_daily_active_hours *= 1.0 - head;
    for risk in &mut second.sources {
        risk.expected_failures *= 1.0 - head;
    }
    record.intervals.splice(at..=at, [first, second]);

    let before = record.steps.last().expect("the restoration step").after;
    let units = vec![uid(0), uid(1)];
    record.steps.push(ProfileStep {
        id: "retreat-200".into(),
        day: CLOSED,
        before,
        after: uniform(&record, 4),
        kind: StepKind::Retreat {
            event: retreat_event.map(str::to_owned),
            units: units.clone(),
        },
    });
    for summary in &mut record.units {
        if units.contains(&summary.unit) {
            summary.retreats.steps += 1;
        }
    }
    record.policy.readback = Some(uniform(&record, 4));
    if !tagged {
        record
            .events
            .iter_mut()
            .find(|event| event.policy_event.id == "c")
            .expect("the charged event")
            .restoration_learning = None;
    }
    resync(&mut record);
    record
}

/// The retreat's claimed event never decides whether the charged event happened on the restored
/// profile: an event-free retreat and a retreat naming another event owe the same crash obligations.
#[test_case(None; "an event free closing retreat")]
#[test_case(Some("other"); "a closing retreat naming a different event")]
fn a_charged_event_on_the_restored_profile_at_the_closing_day_owes_the_crash_whatever_the_retreat_names(
    retreat_event: Option<&str>,
) {
    let lab = Lab::new(&[FIRMWARE], 1);
    let record = closed_on_charged_event(&lab, retreat_event, false);
    record.validate().expect("a coherent record");
    let result = audit(&record, &Rules::new(&lab.registration));
    let rules: Vec<&str> = result
        .violations
        .iter()
        .map(|violation| violation.rule)
        .collect();
    assert_eq!(
        rules,
        [
            "restoration-crash-undeclared",
            "restoration-step-back",
            "restoration-crash-stop",
            "restoration-tag",
        ],
        "{:?}",
        result.violations
    );
    assert_eq!(
        result
            .violations
            .iter()
            .filter(|violation| violation.rule == "restoration-tag")
            .map(|violation| violation.subject.as_str())
            .collect::<Vec<_>>(),
        ["event c"]
    );
}

/// Tagging the event the way the restoration span requires removes only the tag violation.
#[test_case(None; "an event free closing retreat")]
#[test_case(Some("other"); "a closing retreat naming a different event")]
fn the_closing_day_event_is_part_of_the_span_so_its_tag_is_required(retreat_event: Option<&str>) {
    let lab = Lab::new(&[FIRMWARE], 1);
    let record = closed_on_charged_event(&lab, retreat_event, true);
    record.validate().expect("a coherent record");
    let result = audit(&record, &Rules::new(&lab.registration));
    let rules: Vec<&str> = result
        .violations
        .iter()
        .map(|violation| violation.rule)
        .collect();
    assert_eq!(
        rules,
        [
            "restoration-crash-undeclared",
            "restoration-step-back",
            "restoration-crash-stop",
        ],
        "{:?}",
        result.violations
    );
}
