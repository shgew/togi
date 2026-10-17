use togi_journal::{
    Activity, Attribution, CorePhase, CoreTransition, Decision, HuntEnd, HuntOutcome, Member,
    Outcome, Seq, TunerDecision,
};
use togi_machine::{CoreId, Signal};

use crate::later::WINDOW;
use crate::later_tests::{
    current_of, drive_to_hunt_start, end_cycle, fail_core, fail_loading_only, held, hunt_end,
    idle_failure, is_hold, later_session, next, parked_failure, pass_cycles, run_group, session,
    settle, start_cycle, strikes,
};
use crate::schedule_tests::transition;
use crate::testkit::{Journal, R7Report, Start};
use crate::tests::{core, off};
use crate::{Action, DecisionPayload, Depth, Profile};

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
    assert_eq!(held(&j), [first]);
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
    assert_eq!(held(&j), [first]);
    j.assert_resumes();
}

fn confirmed_seq(j: &Journal) -> Seq {
    j.run().bios_profile().confirmed.unwrap().seq
}

/// Holds a first idle failure of cores 00 and 01 at [-10, -12], then starts the hunt its second
/// failure pairs with. Returns the hold and the hunt start.
fn paired_hunt() -> (Journal, Seq, Seq) {
    let mut j = later_session(&[-10, -12]);
    start_cycle(&mut j);
    idle_failure(&mut j);
    let hold = j.next_decision();
    let hold = j.decide(hold);
    idle_failure(&mut j);
    let start = drive_to_hunt_start(&mut j);
    let start = j.decide(start);
    (j, hold, start)
}

/// Answers every group of the hunt, failing those `fails` names, and records its end.
fn run_paired_hunt(j: &mut Journal, mut fails: impl FnMut(&[i64]) -> bool) -> HuntEnd {
    for _ in 0..200 {
        match j.next() {
            Action::Decide(decision) => {
                if let DecisionPayload::HuntEnd(end) = &decision.payload {
                    let end = end.clone();
                    j.decide(decision);
                    return end;
                }
                assert!(
                    matches!(decision.payload, DecisionPayload::HuntGroup(_)),
                    "unexpected {decision:?}"
                );
                j.decide(decision);
            }
            Action::RunTrial(plan) => {
                let fail = fails(&current_of(&plan.profile));
                run_group(j, &plan, fail);
            }
            Action::ReadRanking => panic!("unexpected ranking"),
        }
    }
    panic!("the hunt did not end");
}

fn reached(j: &Journal, shown: &Profile) -> bool {
    j.run().constraints.reaches(shown) != Ok(None)
}

/// The shown profile reaches no recorded constraint and leaves some member of `failing`
/// shallower than its failing offset, live and after a fresh fold.
fn assert_shown_avoids(j: &Journal, step: &str, failing: &[(u8, i64)]) {
    let shown = j.run().bios_profile();
    assert!(!reached(j, &shown.shown), "{step}: {:?}", shown.shown);
    if !failing.is_empty() {
        assert!(
            failing
                .iter()
                .any(|&(id, offset)| shown.shown.get(core(id)).unwrap().get() > offset),
            "{step}: {:?} reaches {failing:?}",
            shown.shown
        );
    }
    assert_eq!(j.resumed().run().bios_profile(), shown, "{step}");
}

#[test]
fn a_paired_hunts_hold_shows_until_its_commitment() {
    let mut j = later_session(&[-10, -12]);
    let confirmed = confirmed_seq(&j);
    start_cycle(&mut j);
    idle_failure(&mut j);
    let hold = j.next_decision();
    let hold = j.decide(hold);
    idle_failure(&mut j);
    let shown = |j: &Journal| assert_shown(j, &[-9, -11], confirmed, &[0, 1], Some(hold));
    shown(&j);
    let start = drive_to_hunt_start(&mut j);
    let started = j.decide(start);
    shown(&j);
    assert!(j.run().hunts.active.as_ref().unwrap().paired.is_some());
    if let Action::Decide(group) = j.next() {
        if matches!(group.payload, DecisionPayload::HuntGroup(_)) {
            j.decide(group);
            shown(&j);
        }
    }
    let end = hunt_end(&mut j, started, HuntOutcome::Culprit, &[0]);
    shown(&j);
    let (backoff, cause) = next(&j);
    assert_eq!((backoff.activity, backoff.core), (Activity::Hunt, core(0)));
    assert_eq!(cause[0], end);
    shown(&j);
    let commit = j.decide(crate::DecisionAction::new(backoff, cause));
    assert_shown(&j, &[-9, -12], confirmed, &[0], Some(commit));
    j.assert_resumes();
}

#[test]
fn a_paired_hunts_pending_backoff_shows_past_its_combination() {
    let (mut j, hold, _) = paired_hunt();
    let confirmed = confirmed_seq(&j);
    run_paired_hunt(&mut j, |p| p[0] <= -9 && p[1] <= -11);
    let combination = j.next_decision();
    let DecisionPayload::Combination(recorded) = &combination.payload else {
        panic!("{combination:?}");
    };
    let members: Vec<(CoreId, i64)> = recorded
        .members
        .iter()
        .map(|m| (m.core, m.offset.get()))
        .collect();
    assert_eq!(members, [(core(0), -9), (core(1), -11)]);
    j.decide(combination);
    let (back, cause) = next(&j);
    assert_eq!(
        (back.activity, back.decision),
        (Activity::Hunt, Decision::Backoff)
    );
    assert!(back.to_offset.shallower_than(back.from_offset), "{back:?}");
    let pending = j.run().bios_profile();
    assert!(!reached(&j, &pending.shown), "{:?}", pending.shown);
    assert_eq!(pending.shown.get(back.core), Some(back.to_offset));
    assert_eq!(pending.since(), Some(hold));
    assert_eq!(
        pending
            .unconfirmed
            .iter()
            .map(|m| m.core)
            .collect::<Vec<_>>(),
        [core(0), core(1)]
    );
    assert_eq!(j.resumed().run().bios_profile(), pending);
    let commit = j.decide(crate::DecisionAction::new(back.clone(), cause));
    assert!(j.run().hunts.active.is_none());
    let after = j.run().bios_profile();
    assert_eq!(after.shown.get(back.core), Some(back.to_offset));
    assert_eq!(after.confirmed.map(|c| c.seq), Some(confirmed));
    assert_eq!(
        after.unconfirmed.iter().map(|m| m.core).collect::<Vec<_>>(),
        [back.core]
    );
    assert_eq!(after.since(), Some(commit));
    j.assert_resumes();
}

#[test]
fn a_paired_hunts_direct_backoff_shows_before_it_is_recorded() {
    let (mut j, hold, _) = paired_hunt();
    let confirmed = confirmed_seq(&j);
    parked_failure(&mut j, 1, -11, &[-9, -11]);
    let end = j.next_decision();
    assert!(
        matches!(&end.payload, DecisionPayload::HuntEnd(end) if end.result == HuntOutcome::Direct),
        "{end:?}"
    );
    j.decide(end);
    assert_eq!(current_of(&j.offsets()), [-10, -12]);
    assert_shown(&j, &[-9, -10], confirmed, &[0, 1], Some(hold));
    let (back, cause) = next(&j);
    assert_eq!(
        (back.activity, back.core, back.to_offset),
        (Activity::Hunt, core(1), off(-10))
    );
    let commit = j.decide(crate::DecisionAction::new(back, cause));
    assert_shown(&j, &[-10, -10], confirmed, &[1], Some(commit));
    assert!(j.run().hunts.active.is_none());
    assert_eq!(strikes(&j), 0);
    j.assert_resumes();
}

/// A hunt end and the members it names failing together.
type EndCase<'a> = (&'a str, HuntOutcome, &'a [(u8, i64)]);

#[test]
fn the_shown_profile_never_reaches_what_a_paired_hunt_found_failing_while_its_commitment_is_pending()
 {
    let both = [(0, -10), (1, -12)];
    let cases: [EndCase<'_>; 3] = [
        ("culprit", HuntOutcome::Culprit, &[(0, -10)]),
        (
            "combination with probed members",
            HuntOutcome::Combination,
            &[(0, -9), (1, -11)],
        ),
        ("fallback combination", HuntOutcome::Fallback, &both),
    ];
    for (name, result, failing) in cases {
        let (mut j, _, start) = paired_hunt();
        match result {
            HuntOutcome::Culprit => {
                hunt_end(&mut j, start, result, &[0]);
            }
            HuntOutcome::Fallback => {
                hunt_end(&mut j, start, result, &[0, 1]);
            }
            HuntOutcome::Cancelled
            | HuntOutcome::Direct
            | HuntOutcome::Combination
            | HuntOutcome::Loaded => {
                let end = run_paired_hunt(&mut j, |p| p[0] <= -9 && p[1] <= -11);
                assert_eq!(end.result, result, "{name}");
            }
        }
        assert_shown_avoids(&j, &format!("{name}: after hunt.end"), failing);
        if result != HuntOutcome::Culprit {
            let combination = j.next_decision();
            assert!(
                matches!(combination.payload, DecisionPayload::Combination(_)),
                "{name}: {combination:?}"
            );
            j.decide(combination);
            assert_shown_avoids(&j, &format!("{name}: after the combination"), failing);
        }
        let (back, cause) = next(&j);
        assert_eq!(back.activity, Activity::Hunt, "{name}");
        assert!(back.to_offset.shallower_than(back.from_offset), "{name}");
        j.decide(crate::DecisionAction::new(back, cause));
        assert!(j.run().hunts.active.is_none(), "{name}");
        assert_shown_avoids(&j, &format!("{name}: after the commitment"), failing);
        j.assert_resumes();
    }

    let (mut j, _, _) = paired_hunt();
    parked_failure(&mut j, 1, -11, &[-9, -11]);
    let failing = [(1, -11)];
    assert_shown_avoids(&j, "direct: after the failure", &failing);
    let end = j.next_decision();
    assert!(
        matches!(&end.payload, DecisionPayload::HuntEnd(end) if end.result == HuntOutcome::Direct),
        "{end:?}"
    );
    j.decide(end);
    assert_shown_avoids(&j, "direct: after hunt.end", &failing);
    let back = j.next_decision();
    j.decide(back);
    assert_shown_avoids(&j, "direct: after the commitment", &failing);
    j.assert_resumes();

    let (mut j, _, start) = paired_hunt();
    j.decision(TunerDecision {
        pass: Some(off(-12)),
        failure_point: Some(off(-12)),
        ..raw(1, Activity::Checking, Decision::Backoff, -12, -11)
    });
    j.profile_change(&[-10, -11]);
    j.append(
        HuntEnd {
            hunt: std::num::NonZeroU32::MIN,
            result: HuntOutcome::Combination,
            cores: crate::testkit::set(&[0, 1]),
            members: both
                .iter()
                .map(|&(id, offset)| Member {
                    core: core(id),
                    offset: off(offset),
                })
                .collect(),
            groups: 1,
            reason: String::new(),
        }
        .into(),
        &[start],
    );
    assert_shown_avoids(&j, "already shallower: after hunt.end", &both);
    let combination = j.next_decision();
    assert!(
        matches!(&combination.payload, DecisionPayload::Combination(c) if c.reason.contains("no backoff")),
        "{combination:?}"
    );
    j.decide(combination);
    assert!(j.run().hunts.active.is_none());
    assert_shown_avoids(&j, "already shallower: after the combination", &both);
    j.assert_resumes();

    let (mut j, _, start) = paired_hunt();
    j.append(
        HuntEnd {
            hunt: std::num::NonZeroU32::MIN,
            result: HuntOutcome::Cancelled,
            cores: crate::testkit::set(&[]),
            members: Vec::new(),
            groups: 0,
            reason: String::new(),
        }
        .into(),
        &[start],
    );
    assert!(j.run().hunts.active.is_none());
    assert_shown_avoids(&j, "after cancellation", &[]);
    j.assert_resumes();

    let (mut j, _, start) = paired_hunt();
    hunt_end(&mut j, start, HuntOutcome::Culprit, &[0]);
    j.reset(0);
    j.core_phase(CoreTransition {
        from: Some(CorePhase::HasRoom),
        ..transition(0, CorePhase::Search, 0)
    });
    assert!(j.run().hunts.active.is_none());
    assert_shown_avoids(&j, "after the reset", &[]);
    j.assert_resumes();
}

#[test]
fn the_shown_profile_never_reaches_a_failure_a_paired_hunt_observes_before_it_ends() {
    let (mut j, _, _) = paired_hunt();
    let fails = |p: &[i64]| p[0] <= -9 && p[1] <= -11;
    let mut observed: Vec<Profile> = Vec::new();
    let check = |j: &Journal, observed: &[Profile], step: &str| {
        let shown = j.run().bios_profile();
        for profile in observed {
            assert!(
                !shown.shown.covers(profile),
                "{step}: {:?} reaches the observed failure {profile:?}",
                shown.shown
            );
        }
        assert!(!reached(j, &shown.shown), "{step}: {:?}", shown.shown);
        assert_eq!(current_of(&j.offsets()), [-10, -12], "{step}");
        assert_eq!(j.resumed().run().bios_profile(), shown, "{step}");
    };
    check(&j, &observed, "after hunt.start");
    let mut stepped = false;
    for _ in 0..200 {
        match j.next() {
            Action::Decide(decision) => {
                let ended = matches!(decision.payload, DecisionPayload::HuntEnd(_));
                assert!(
                    ended || matches!(decision.payload, DecisionPayload::HuntGroup(_)),
                    "unexpected {decision:?}"
                );
                j.decide(decision);
                if ended {
                    check(&j, &observed, "after hunt.end");
                    break;
                }
                check(&j, &observed, "after hunt.group");
            }
            Action::RunTrial(plan) => {
                let fail = fails(&current_of(&plan.profile));
                let (intent, trial) = j.intent(&plan);
                check(&j, &observed, "after trial.intent");
                let mut end = Journal::end_payload(
                    &trial,
                    if fail {
                        Outcome::Failure
                    } else {
                        Outcome::Pass
                    },
                    fail.then_some(Signal::Crash),
                );
                end.duration_s = if fail { 5 } else { 120 };
                if fail {
                    observed.push(plan.profile);
                }
                j.append(end.into(), &[intent]);
                check(&j, &observed, "after trial.end");
                if fail {
                    let attribution = j.next_decision();
                    assert!(
                        matches!(&attribution.payload, DecisionPayload::Failure(f) if f.attribution == Attribution::Unattributed),
                        "{attribution:?}"
                    );
                    j.decide(attribution);
                    check(&j, &observed, "after the group failure");
                    stepped |= j.run().bios_profile().shown.get(core(0)).unwrap().get() > -9;
                }
            }
            Action::ReadRanking => panic!("unexpected ranking"),
        }
    }
    assert!(
        stepped,
        "no observed failure stepped the shown profile past a reached failing profile"
    );
    j.assert_resumes();
}

#[test]
fn an_observed_hunt_failure_steps_only_the_cores_it_held() {
    let mut j = later_session(&[-10, -12, -14]);
    let confirmed = confirmed_seq(&j);
    start_cycle(&mut j);
    fail_core(&mut j, 0);
    let hold = j.next_decision();
    let hold = j.decide(hold);
    assert_shown(&j, &[-9, -12, -14], confirmed, &[0], Some(hold));
    idle_failure(&mut j);
    let start = drive_to_hunt_start(&mut j);
    let DecisionPayload::HuntStart(payload) = &start.payload else {
        unreachable!()
    };
    assert_eq!(start.cause.len(), 2, "{start:?}");
    assert_eq!(payload.candidates, crate::testkit::set(&[0, 1, 2]));
    j.decide(start);
    let probe = [-9, -12, 0];
    let fails = |p: &[i64]| p[0] <= -9 && p[1] <= -11;
    for _ in 0..200 {
        match j.next() {
            Action::Decide(decision)
                if matches!(decision.payload, DecisionPayload::HuntGroup(_)) =>
            {
                j.decide(decision);
            }
            Action::RunTrial(plan) => {
                let profile = current_of(&plan.profile);
                let fail = fails(&profile);
                run_group(&mut j, &plan, fail);
                if fail && profile == probe {
                    assert_shown(&j, &[-8, -11, -14], confirmed, &[0, 1], Some(hold));
                    assert_eq!(current_of(&j.offsets()), [-10, -12, -14]);
                    j.assert_resumes();
                    return;
                }
            }
            other @ (Action::Decide(_) | Action::ReadRanking) => {
                panic!("the hunt ended before its member probe {probe:?} failed: {other:?}")
            }
        }
    }
    panic!("the hunt never ran its member probe");
}

#[test]
fn a_direct_hunt_failure_previews_its_safe_backoff_before_hunt_end() {
    for (failing, shown) in [(-11, [-9, -10]), (-15, [-9, -11])] {
        let (mut j, hold, _) = paired_hunt();
        let confirmed = confirmed_seq(&j);
        parked_failure(&mut j, 1, failing, &[-9, failing]);
        let before = j.state.clone();
        let end = j.next_decision();
        assert!(
            matches!(&end.payload, DecisionPayload::HuntEnd(end) if end.result == HuntOutcome::Direct)
        );
        assert_shown(&j, &shown, confirmed, &[0, 1], Some(hold));
        assert_eq!(current_of(&j.offsets()), [-10, -12]);
        assert_eq!(j.state, before);
        j.assert_resumes();
    }
}

#[test]
fn a_paired_hunt_shows_no_move_while_a_zero_failure_awaits_its_diagnostic_or_dead_end() {
    for (profile, name) in [([-9, 0], "diagnostic"), ([0, 0], "dead end")] {
        let (mut j, hold, _) = paired_hunt();
        let confirmed = confirmed_seq(&j);
        parked_failure(&mut j, 1, 0, &profile);
        let action = j.next();
        match (&action, name) {
            (Action::RunTrial(plan), "diagnostic") => {
                assert!(matches!(
                    plan.context,
                    crate::TrialContext::ZeroRerun { .. }
                ));
            }
            (Action::Decide(decision), "dead end") => {
                assert!(matches!(decision.payload, DecisionPayload::DeadEnd(_)));
            }
            other => panic!("{name}: {other:?}"),
        }
        assert_shown(&j, &[-9, -11], confirmed, &[0, 1], Some(hold));
        j.assert_resumes();
    }
}
