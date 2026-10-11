use togi_journal::{Activity, CorePhase, Decision, Seq, TunerDecision};
use togi_machine::CoreId;

use crate::later::WINDOW;
use crate::later_tests::{
    current_of, end_cycle, fail_core, fail_loading_only, idle_failure, is_hold, later_session,
    next, pass_cycles, session, settle, start_cycle,
};
use crate::schedule_tests::transition;
use crate::testkit::{Journal, R7Report, Start};
use crate::tests::{core, off};

fn raw(id: u8, activity: Activity, decision: Decision, from: i64, to: i64) -> TunerDecision {
    TunerDecision {
        core: core(id),
        activity,
        decision,
        from_offset: off(from),
        to_offset: off(to),
        pass: Some(off(from.max(to))),
        failure_point: (decision == Decision::Backoff).then(|| off(from)),
        workloads: Vec::new(),
        reason: "test".to_owned(),
    }
}

fn assert_shown(
    j: &Journal,
    offsets: &[i64],
    confirmed: Seq,
    unconfirmed: &[u8],
    since: Option<Seq>,
) {
    for state in [j.run().bios_profile(), j.resumed().run().bios_profile()] {
        assert_eq!(current_of(&state.shown), offsets);
        assert_eq!(state.confirmed.map(|c| c.seq), Some(confirmed));
        let cores: Vec<CoreId> = unconfirmed.iter().map(|&id| core(id)).collect();
        assert_eq!(
            state.unconfirmed.iter().map(|m| m.core).collect::<Vec<_>>(),
            cores
        );
        assert_eq!(state.since(), since);
        assert_eq!(state.first_result, unconfirmed.is_empty());
    }
}

#[test]
fn bios_profile_tracks_the_confirmed_profile() {
    let mut j = session(&[-10, -12]);
    let before = j.run().bios_profile();
    assert!(before.confirmed.is_none() && before.unconfirmed.is_empty());
    assert!(!before.first_result);
    start_cycle(&mut j);
    let phase1 = end_cycle(&mut j);
    assert_shown(&j, &[-10, -12], phase1, &[], None);
    j.decision(raw(0, Activity::Deepening, Decision::Deepen, -10, -11));
    assert_shown(&j, &[-10, -12], phase1, &[], None);
    j.decision(raw(0, Activity::Checking, Decision::Backoff, -11, -9));
    let stepped = j.events.last().unwrap().seq;
    assert_shown(&j, &[-9, -12], phase1, &[0], Some(stepped));
    j.reset(1);
    j.core_phase(transition(1, CorePhase::Search, 0));
    let state = j.run().bios_profile();
    assert!(state.confirmed.is_none());
    assert_eq!(current_of(&state.shown), [-9, 0]);
}

#[test]
fn bios_profile_marks_a_step_back_since_its_decision() {
    let mut j = later_session(&[-10, -12]);
    let confirmed = j.run().bios_profile().confirmed.unwrap().seq;
    j.decision(raw(1, Activity::Checking, Decision::Backoff, -12, -11));
    let at = j.events.last().unwrap().seq;
    assert_shown(&j, &[-10, -11], confirmed, &[1], Some(at));
}

struct HoldCase {
    name: &'static str,
    offsets: &'static [i64],
    shown: &'static [i64],
    unconfirmed: &'static [u8],
    hold: fn(&mut Journal) -> Seq,
}

fn hold_attributed(j: &mut Journal) -> Seq {
    fail_core(j, 0);
    let action = j.next_decision();
    j.decide(action)
}

fn hold_unattributed(j: &mut Journal) -> Seq {
    idle_failure(j);
    let action = j.next_decision();
    assert!(matches!(
        action.payload,
        crate::DecisionPayload::HuntSkipped(_)
    ));
    j.decide(action)
}

const HOLDS: [HoldCase; 2] = [
    HoldCase {
        name: "attributed",
        offsets: &[-10, -12],
        shown: &[-9, -12],
        unconfirmed: &[0],
        hold: hold_attributed,
    },
    HoldCase {
        name: "unattributed steps back its nonzero candidates and leaves cores at 0",
        offsets: &[-10, 0, -12],
        shown: &[-9, 0, -11],
        unconfirmed: &[0, 2],
        hold: hold_unattributed,
    },
];

#[test]
fn bios_profile_shows_a_held_failure_stepped_back_at_once() {
    for case in &HOLDS {
        let mut j = later_session(case.offsets);
        let confirmed = j.run().bios_profile().confirmed.unwrap().seq;
        start_cycle(&mut j);
        let hold = (case.hold)(&mut j);
        assert_shown(&j, case.shown, confirmed, case.unconfirmed, Some(hold));
        assert_eq!(
            crate::later_tests::current(&j),
            case.offsets,
            "{}: a hold moved a core",
            case.name
        );
        j.assert_resumes();
    }
}

#[test]
fn bios_profile_returns_to_the_held_offset_when_the_hold_expires() {
    for case in &HOLDS {
        let mut j = later_session(case.offsets);
        start_cycle(&mut j);
        let hold = (case.hold)(&mut j);
        end_cycle(&mut j);
        let last = pass_cycles(&mut j, WINDOW - 1);
        assert_shown(&j, case.shown, last, case.unconfirmed, Some(hold));
        start_cycle(&mut j);
        assert_shown(&j, case.offsets, last, &[], None);
        j.assert_resumes();
    }
}

#[test]
fn bios_profile_shows_a_multi_count_r7_hold_at_the_voltage_target() {
    let mut j = Journal::r7_session().resuming_every(1);
    let values = crate::later_tests::current(&j);
    let passing = R7Report::default()
        .requests(&[(0, 1.1539), (1, 1.08)])
        .top(&[0]);
    for _ in 0..j.run().config.needed.get() {
        j.r7_fact(true, &[0, 1], &[-20, -30, -30, -30], &passing);
    }
    pass_cycles(&mut j, 3);
    let confirmed = j.run().bios_profile().confirmed.unwrap().seq;
    start_cycle(&mut j);
    let failing = R7Report::default()
        .requests(&[(0, 1.09447), (1, 1.08)])
        .top(&[0]);
    j.r7_failed(&[0, 1], &values, &failing);
    let (hold, _) = next(&j);
    assert!(is_hold(&hold) && hold.core == core(0), "{hold:?}");
    let action = j.next_decision();
    let at = j.decide(action);
    assert_shown(&j, &[-13, -30, -30, -30], confirmed, &[0], Some(at));
    j.assert_resumes();
}

#[test]
fn bios_profile_follows_the_actual_step_back_at_a_second_failure() {
    struct Case {
        name: &'static str,
        hold: fn(&mut Journal) -> Seq,
        second: u8,
        before: [i64; 2],
        before_marked: &'static [u8],
        after: [i64; 2],
    }
    for case in [
        Case {
            name: "same core",
            hold: hold_attributed,
            second: 0,
            before: [-9, -12],
            before_marked: &[0],
            after: [-9, -12],
        },
        Case {
            name: "attributed failure pairs with a held unattributed one",
            hold: hold_unattributed,
            second: 1,
            before: [-9, -11],
            before_marked: &[0, 1],
            after: [-10, -11],
        },
    ] {
        let mut j = later_session(&[-10, -12]);
        let confirmed = j.run().bios_profile().confirmed.unwrap().seq;
        start_cycle(&mut j);
        let hold = (case.hold)(&mut j);
        fail_core(&mut j, case.second);
        assert_shown(&j, &case.before, confirmed, case.before_marked, Some(hold));
        let (step, _) = next(&j);
        assert!(
            !is_hold(&step)
                && step.core == core(case.second)
                && step.to_offset.get() > step.from_offset.get(),
            "{}: {step:?}",
            case.name
        );
        let action = j.next_decision();
        let at = j.decide(action);
        assert_shown(&j, &case.after, confirmed, &[case.second], Some(at));
        j.assert_resumes();
    }
}

#[test]
fn a_second_failure_step_back_is_confirmed_by_the_next_passed_cycle() {
    let mut j = later_session(&[-10, -12]);
    start_cycle(&mut j);
    hold_attributed(&mut j);
    fail_core(&mut j, 0);
    let action = j.next_decision();
    j.decide(action);
    settle(&mut j);
    let end = end_cycle(&mut j);
    assert_shown(&j, &[-9, -12], end, &[], None);
    assert!(j.run().first_result());
}

#[test]
fn first_result_needs_a_confirmed_profile() {
    let mut j = Journal::new(&[0], &[Start::at_limit(-10).fail(-11).pass(-10)]).resuming_every(1);
    assert!(!j.run().first_result());
    let change = j.next_decision();
    j.decide(change);
    start_cycle(&mut j);
    end_cycle(&mut j);
    assert!(j.run().first_result(), "{:?}", j.run().bios_profile());
    pass_cycles(&mut j, 2);
    start_cycle(&mut j);
    fail_core(&mut j, 0);
    let action = j.next_decision();
    j.decide(action);
    assert!(!j.run().first_result());
    assert!(!j.run().bios_profile().unconfirmed.is_empty());
    end_cycle(&mut j);
    pass_cycles(&mut j, WINDOW - 1);
    assert!(
        !j.run().first_result(),
        "a valid hold's stepped-back profile is a first result"
    );
    start_cycle(&mut j);
    assert!(
        j.run().first_result(),
        "the expired hold did not return to the confirmed profile"
    );
}

#[test]
fn a_hold_target_stops_showing_for_a_core_that_stepped_back_and_was_confirmed() {
    let mut j = later_session(&[-10, -12]);
    start_cycle(&mut j);
    let first = fail_loading_only(&mut j, 0);
    let action = j.next_decision();
    assert!(matches!(
        &action.payload,
        crate::DecisionPayload::HuntSkipped(skip) if skip.failure == first
    ));
    let hold = j.decide(action);
    let confirmed = j.run().bios_profile().confirmed.unwrap().seq;
    assert_eq!(
        j.run()
            .later
            .strikes()
            .map(|k| k.cores.len())
            .collect::<Vec<_>>(),
        [1]
    );
    assert_shown(&j, &[-9, -11], confirmed, &[0, 1], Some(hold));
    fail_core(&mut j, 1);
    let (own, _) = next(&j);
    assert!(is_hold(&own) && own.core == core(1), "{own:?}");
    let action = j.next_decision();
    j.decide(action);
    fail_core(&mut j, 1);
    let (step, _) = next(&j);
    assert!(!is_hold(&step) && step.to_offset == off(-11), "{step:?}");
    let action = j.next_decision();
    j.decide(action);
    settle(&mut j);
    let end = end_cycle(&mut j);
    assert_shown(&j, &[-9, -11], end, &[0], Some(hold));
    assert_eq!(
        j.run()
            .later
            .strikes()
            .map(|k| k.failure)
            .collect::<Vec<_>>(),
        [first]
    );
    j.assert_resumes();
}
