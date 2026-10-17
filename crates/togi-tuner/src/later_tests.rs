use proptest::prelude::*;
use togi_journal::{Activity, Decision, HuntEnd, HuntOutcome, Outcome, Seq, TunerDecision};
use togi_machine::{Condition, Regime, Signal};

use crate::later::WINDOW;
use crate::schedule_tests::decision;
use crate::testkit::{Journal, R7Report, Start};
use crate::tests::{core, off};
use crate::{Action, Depth, Load, TrialContext};

pub(crate) fn session(offsets: &[i64]) -> Journal {
    let ids: Vec<u8> = (0..offsets.len())
        .map(|id| u8::try_from(id).unwrap())
        .collect();
    let starts: Vec<Start> = offsets
        .iter()
        .map(|&offset| Start::at_limit(offset).fail(offset - 1).pass(offset))
        .collect();
    let mut j = Journal::new(&ids, &starts).resuming_every(1);
    j.ranking(&ids);
    let change = j.next_decision();
    j.decide(change);
    j
}

pub(crate) fn start_cycle(j: &mut Journal) -> u32 {
    j.open_default_cycle();
    j.run().checking.last_cycle().unwrap().get()
}

pub(crate) fn end_cycle(j: &mut Journal) -> Seq {
    let cycle = j.run().checking.last_cycle().unwrap().get();
    j.cycle_end(cycle, true, true)
}

pub(crate) fn pass_cycles(j: &mut Journal, count: u32) -> Seq {
    let mut end = None;
    for _ in 0..count {
        start_cycle(j);
        end = Some(end_cycle(j));
    }
    end.expect("at least one cycle")
}

pub(crate) fn later_session(offsets: &[i64]) -> Journal {
    let mut j = session(offsets);
    pass_cycles(&mut j, 3);
    assert!(j.run().later_gate().is_some(), "the gate is closed");
    j
}

pub(crate) fn current(j: &Journal) -> Vec<i64> {
    j.offsets().iter().map(|(_, offset)| offset.get()).collect()
}

pub(crate) fn fail_core(j: &mut Journal, id: u8) -> Seq {
    let duration = j.run().config.checking_trial.get();
    let plan = j.manual(
        Regime::R1,
        0,
        Load::Single(core(id)),
        Condition::Together,
        &current(j),
        duration,
    );
    let (intent, trial) = j.intent(&plan);
    let mut end = Journal::end_payload(&trial, Outcome::Failure, Some(Signal::ComputationError));
    end.core = Some(core(id));
    j.append(end.into(), &[intent]);
    let attribution = j.next_decision();
    assert!(matches!(
        attribution.payload,
        crate::DecisionPayload::Failure(_)
    ));
    j.decide(attribution)
}

pub(crate) fn fail_loading_only(j: &mut Journal, id: u8) -> Seq {
    let duration = j.run().config.checking_trial.get();
    let plan = j.manual(
        Regime::R1,
        0,
        Load::Single(core(id)),
        Condition::Together,
        &current(j),
        duration,
    );
    j.trial(&plan, Outcome::Failure);
    let attribution = j.next_decision();
    assert!(matches!(
        &attribution.payload,
        crate::DecisionPayload::Failure(f) if f.attribution == togi_journal::Attribution::Unattributed
    ));
    j.decide(attribution)
}

pub(crate) fn idle_failure(j: &mut Journal) -> Seq {
    let profile = j.offsets().offsets();
    j.idle_failure(profile, Condition::Together)
}

pub(crate) fn next(j: &Journal) -> (TunerDecision, Vec<Seq>) {
    let action = j.next_decision();
    (decision(&action).clone(), action.cause)
}

pub(crate) fn is_hold(decision: &TunerDecision) -> bool {
    decision.activity == Activity::Checking
        && decision.decision == Decision::Hold
        && decision.from_offset == decision.to_offset
        && decision.reason.starts_with("holds")
}

pub(crate) fn settle(j: &mut Journal) {
    for _ in 0..40 {
        let Some(Action::Decide(action)) = j.state.next().ok() else {
            return;
        };
        j.decide(action);
    }
    panic!("the tuner kept deciding");
}

/// The failures whose holds still stand: the holds whose step-back the BIOS profile shows, in
/// hold order.
pub(crate) fn held(j: &Journal) -> Vec<Seq> {
    let shown: Vec<Seq> = j
        .state
        .project()
        .bios_profile
        .into_iter()
        .flat_map(|profile| profile.unconfirmed)
        .map(|mark| mark.since)
        .collect();
    j.events
        .iter()
        .filter(|event| shown.contains(&event.seq))
        .filter_map(|event| {
            if let togi_journal::Payload::TunerDecision(decision) = &event.payload
                && decision.decision == Decision::Hold
            {
                return event.cause.first().copied();
            }
            if let togi_journal::Payload::HuntSkipped(skip) = &event.payload {
                return Some(skip.failure);
            }
            None
        })
        .collect()
}

pub(crate) fn strikes(j: &Journal) -> usize {
    held(j).len()
}

fn tokens(reason: &str, wanted: &[&str]) {
    let missing: Vec<&&str> = wanted
        .iter()
        .filter(|token| !reason.contains(**token))
        .collect();
    assert!(missing.is_empty(), "{reason:?} lacks {missing:?}");
}

#[test]
fn gate_closed_steps_back_at_once() {
    for (name, cycles) in [
        ("before phase 1 ends", 0),
        ("in the confirmation", 1),
        ("first cycle after the conclusion", 2),
    ] {
        let mut j = session(&[-10, -12]);
        if cycles > 0 {
            pass_cycles(&mut j, cycles);
        }
        start_cycle(&mut j);
        let failure = fail_core(&mut j, 0);
        let (step, cause) = next(&j);
        assert_eq!(
            (step.from_offset.get(), step.to_offset.get()),
            (-10, -9),
            "{name}"
        );
        assert_eq!(step.decision, Decision::Backoff, "{name}");
        assert!(!step.reason.contains("second failure"), "{name}");
        assert_eq!(cause, [failure], "{name}");
        let action = j.next_decision();
        j.decide(action);
        assert_eq!(strikes(&j), 0, "{name}");
        j.assert_resumes();
    }
}

#[test]
fn first_failure_holds_and_second_steps_back_naming_both() {
    let mut j = later_session(&[-10, -12]);
    start_cycle(&mut j);
    let first = fail_core(&mut j, 0);
    let (hold, cause) = next(&j);
    assert!(is_hold(&hold), "{hold:?}");
    assert_eq!(
        (hold.core, hold.from_offset, hold.to_offset),
        (core(0), off(-10), off(-10))
    );
    assert_eq!(hold.pass, Some(off(-10)));
    assert_eq!(hold.failure_point, Some(off(-11)));
    tokens(
        &hold.reason,
        &[
            &format!("#{first}"),
            "cycle 4",
            "cycle 8",
            "K = 5",
            "core 00",
        ],
    );
    assert_eq!(cause[0], first);
    let action = j.next_decision();
    let held = j.decide(action);
    assert!(j.run().reruns.is_empty());
    assert_eq!(j.run().core(core(0)).offset, off(-10));
    assert_eq!(j.run().failure_point(core(0)), Some(off(-11)));
    let shown = j.run().bios_profile();
    assert_eq!(current_of(&shown.shown), [-9, -12]);
    assert_eq!(shown.since(), Some(held));
    assert!(matches!(
        j.next(),
        Action::RunTrial(plan) if !matches!(plan.requirement.context, TrialContext::Rerun { .. })
    ));
    j.assert_resumes();

    let second = fail_core(&mut j, 0);
    let (step, cause) = next(&j);
    assert!(!is_hold(&step));
    assert_eq!(step.decision, Decision::Backoff);
    assert_eq!((step.from_offset.get(), step.to_offset.get()), (-10, -9));
    assert_eq!(cause, [second, first]);
    tokens(
        &step.reason,
        &[
            &format!("#{first}"),
            &format!("#{second}"),
            "within 5 cycles",
            "(cycle 4, held)",
        ],
    );
    assert!(step.reason.ends_with(&format!(
        "second failure within 5 cycles: #{first} (cycle 4, held) and #{second} (cycle 4)"
    )));
    let action = j.next_decision();
    let stepped = j.decide(action);
    assert_eq!(j.run().reruns.head().map(|o| o.failure), Some(second));
    let shown = j.run().bios_profile();
    assert_eq!(current_of(&shown.shown), [-9, -12]);
    assert_eq!(shown.since(), Some(stepped));
    assert_eq!(strikes(&j), 0);
    settle(&mut j);
    assert_eq!(
        j.run().checking.profile().and_then(|p| p.get(core(0))),
        Some(off(-9))
    );
    let end = end_cycle(&mut j);
    let shown = j.run().bios_profile();
    assert!(shown.unconfirmed.is_empty());
    assert_eq!(shown.confirmed.map(|c| c.seq), Some(end));
    assert_eq!(current_of(&shown.shown), [-9, -12]);
    j.assert_resumes();
}

#[test]
fn a_pair_owes_one_rerun_for_the_second_failure_and_consumes_the_hold_once() {
    let mut j = later_session(&[-10, -12]);
    start_cycle(&mut j);
    let first = fail_core(&mut j, 0);
    let action = j.next_decision();
    j.decide(action);
    assert!(j.run().failures.pending(first).is_none());
    assert_eq!(strikes(&j), 1);

    let second = fail_core(&mut j, 0);
    let class = j.run().failures.pending(second).unwrap().fact.class;
    let (_, cause) = next(&j);
    assert_eq!(cause.first(), Some(&second));
    assert_eq!(cause.last(), Some(&first));
    let action = j.next_decision();
    j.decide(action);
    let owed: Vec<_> = j
        .run()
        .reruns
        .owed()
        .map(|owed| (owed.class, owed.failure))
        .collect();
    assert_eq!(owed, [(class, second)]);
    assert_eq!(strikes(&j), 0);
    j.assert_resumes();

    settle(&mut j);
    let third = fail_core(&mut j, 0);
    let (step, cause) = next(&j);
    assert_eq!(step.decision, Decision::Backoff);
    assert_eq!(cause, [third]);
    assert!(!step.reason.contains("second failure"), "{}", step.reason);
}

pub(crate) fn current_of(profile: &crate::Profile) -> Vec<i64> {
    profile.iter().map(|(_, offset)| offset.get()).collect()
}

#[test]
fn hold_keeps_showing_its_step_back_after_a_passed_cycle() {
    let mut j = later_session(&[-10, -12]);
    start_cycle(&mut j);
    fail_core(&mut j, 0);
    let action = j.next_decision();
    j.decide(action);
    let end = end_cycle(&mut j);
    let shown = j.run().bios_profile();
    assert_eq!(shown.confirmed.map(|c| c.seq), Some(end));
    assert_eq!(current_of(&shown.shown), [-9, -12]);
    assert_eq!(
        shown.unconfirmed.iter().map(|m| m.core).collect::<Vec<_>>(),
        [core(0)]
    );
    assert_eq!(strikes(&j), 1);
    j.assert_resumes();
}

#[test]
fn failures_on_different_cores_hold_separately() {
    let mut j = later_session(&[-10, -12]);
    start_cycle(&mut j);
    fail_core(&mut j, 0);
    let action = j.next_decision();
    j.decide(action);
    fail_core(&mut j, 1);
    let (second, _) = next(&j);
    assert!(is_hold(&second) && second.core == core(1), "{second:?}");
    let action = j.next_decision();
    j.decide(action);
    assert_eq!(strikes(&j), 2);
    let marked: Vec<_> = j
        .run()
        .bios_profile()
        .unconfirmed
        .iter()
        .map(|m| m.core)
        .collect();
    assert_eq!(marked, [core(0), core(1)]);
    j.assert_resumes();
}

#[test]
fn window_pairs_within_k_cycles_and_expires_at_k() {
    for (name, gap, pair) in [
        ("same cycle", 0, true),
        ("last cycle of the window", WINDOW - 1, true),
        ("one past the window", WINDOW, false),
        ("long after", WINDOW + 4, false),
    ] {
        let mut j = later_session(&[-10, -12]);
        start_cycle(&mut j);
        let first = fail_core(&mut j, 0);
        let action = j.next_decision();
        j.decide(action);
        if gap > 0 {
            end_cycle(&mut j);
            pass_cycles(&mut j, gap - 1);
            assert_eq!(start_cycle(&mut j), 4 + gap, "{name}");
        }
        fail_core(&mut j, 0);
        let (step, cause) = next(&j);
        if pair {
            assert!(!is_hold(&step), "{name}: {step:?}");
            assert_eq!(step.to_offset, off(-9), "{name}");
            assert_eq!(cause.last(), Some(&first), "{name}");
            continue;
        }
        assert!(is_hold(&step), "{name}: {step:?}");
        let action = j.next_decision();
        j.decide(action);
        let held = held(&j);
        assert_eq!(held.len(), 1, "{name}");
        assert_ne!(held[0], first, "{name}");
        j.assert_resumes();
    }
}

#[test]
fn strike_is_invalid_when_its_core_moved_for_another_reason() {
    let mut j = later_session(&[-10, -12]);
    start_cycle(&mut j);
    fail_core(&mut j, 0);
    let action = j.next_decision();
    j.decide(action);
    j.decision(TunerDecision {
        core: core(0),
        activity: Activity::Hunt,
        decision: Decision::Backoff,
        from_offset: off(-10),
        to_offset: off(-9),
        pass: Some(off(-10)),
        failure_point: Some(off(-10)),
        workloads: Vec::new(),
        reason: "test".to_owned(),
    });
    settle(&mut j);
    end_cycle(&mut j);
    assert_eq!(strikes(&j), 0);
    start_cycle(&mut j);
    fail_core(&mut j, 0);
    let (after, _) = next(&j);
    assert!(is_hold(&after) && after.from_offset == off(-9), "{after:?}");
    j.assert_resumes();
}

#[test]
fn gate_reopens_only_after_the_stepped_back_profile_passes() {
    let mut j = later_session(&[-10, -12]);
    start_cycle(&mut j);
    fail_core(&mut j, 0);
    let action = j.next_decision();
    j.decide(action);
    fail_core(&mut j, 0);
    let action = j.next_decision();
    j.decide(action);
    settle(&mut j);
    fail_core(&mut j, 1);
    let (closed, _) = next(&j);
    assert!(!is_hold(&closed), "{closed:?}");
    assert_eq!((closed.core, closed.to_offset), (core(1), off(-11)));
    let action = j.next_decision();
    j.decide(action);
    settle(&mut j);
    end_cycle(&mut j);
    start_cycle(&mut j);
    fail_core(&mut j, 0);
    let (reopened, _) = next(&j);
    assert!(is_hold(&reopened), "{reopened:?}");
    j.assert_resumes();
}

#[test]
fn stale_failure_at_a_deeper_offset_is_not_a_hold() {
    let mut j = later_session(&[-10, -12]);
    start_cycle(&mut j);
    let profile = j.profile(&[-12, -12]).offsets();
    j.failure(
        togi_journal::Failure {
            signal: Signal::ComputationError,
            attribution: togi_journal::Attribution::Attributed,
            core: Some(core(0)),
            offset: Some(off(-12)),
            trial: None,
            regime: Some(Regime::R1),
            condition: Some(Condition::Together),
            profile: Some(profile),
            known_failure: None,
            reason: None,
            round: None,
        },
        &[],
    );
    let (answer, _) = next(&j);
    assert!(!is_hold(&answer));
    assert_eq!(answer.decision, Decision::Backoff);
    assert_eq!(answer.from_offset, answer.to_offset);
    assert!(answer.reason.contains("already shallower"));
    let action = j.next_decision();
    j.decide(action);
    assert_eq!(strikes(&j), 0);
}

fn r7_later() -> Journal {
    let mut j = Journal::r7_session().resuming_every(1);
    pass_cycles(&mut j, 3);
    start_cycle(&mut j);
    j
}

#[test]
fn multi_core_r7_failures_hold_without_counting_toward_escalation() {
    let mut j = r7_later();
    let values = current(&j);
    let named = R7Report::default().named(0);
    let first = j.r7_failed(&[0, 1], &values, &named);
    let (hold, cause) = next(&j);
    assert!(is_hold(&hold), "{hold:?}");
    assert_eq!((hold.core, hold.from_offset), (core(0), off(-30)));
    assert_eq!(cause[0], first);
    let action = j.next_decision();
    j.decide(action);
    let plan = j.r7_plan(&[0, 1], &values);
    let load = j.run().plan_class(&plan).load();
    assert_eq!(j.run().r7.strikes.failures(load), 0);
    assert!(j.run().reruns.is_empty());
    assert!(j.run().r7.open.is_empty(), "a held R7 failure stayed open");
    let second = j.r7_failed(&[0, 1], &values, &named);
    let (pair, cause) = next(&j);
    assert!(!is_hold(&pair), "{pair:?}");
    assert!(pair.to_offset.shallower_than(pair.from_offset));
    assert_eq!(cause[0], second);
    assert_eq!(cause.last(), Some(&first));
    tokens(&pair.reason, &[&format!("#{first}"), &format!("#{second}")]);
    let action = j.next_decision();
    j.decide(action);
    j.assert_resumes();
}

#[test]
fn offsets_never_deepen_after_conclusion() {
    let mut j = later_session(&[-10, -12]);
    start_cycle(&mut j);
    let mut before = current(&j);
    for _ in 0..6 {
        fail_core(&mut j, 0);
        fail_core(&mut j, 1);
        settle(&mut j);
        let now = current(&j);
        assert!(now.iter().zip(&before).all(|(now, before)| now >= before));
        before = now;
        end_cycle(&mut j);
        start_cycle(&mut j);
    }
    j.assert_resumes();
}

#[test]
fn a_held_failure_is_never_a_known_failure() {
    let mut j = later_session(&[-10, -12]);
    start_cycle(&mut j);
    let duration = j.run().config.checking_trial.get();
    let plan = j.manual(
        Regime::R1,
        0,
        Load::Single(core(0)),
        Condition::Together,
        &current(&j),
        duration,
    );
    fail_core(&mut j, 0);
    let action = j.next_decision();
    j.decide(action);
    let skipped = |j: &Journal| {
        matches!(
            j.run().skip_known_failure(Action::RunTrial(plan.clone())),
            Action::Decide(_)
        )
    };
    assert!(!skipped(&j));
    end_cycle(&mut j);
    pass_cycles(&mut j, WINDOW);
    assert_eq!(strikes(&j), 0, "the strike expired");
    assert!(
        !skipped(&j),
        "a held failure stays out of the known-failure rule"
    );
    j.assert_resumes();

    let mut control = session(&[-10, -12]);
    start_cycle(&mut control);
    let plan = control.manual(
        Regime::R1,
        0,
        Load::Single(core(0)),
        Condition::Together,
        &current(&control),
        duration,
    );
    fail_core(&mut control, 0);
    let action = control.next_decision();
    control.decide(action);
    assert!(matches!(
        control.run().skip_known_failure(Action::RunTrial(plan)),
        Action::Decide(_)
    ));
}

#[test]
fn an_unattributed_first_failure_is_held_by_skipping_its_hunt() {
    let mut j = later_session(&[-10, -12]);
    start_cycle(&mut j);
    let first = idle_failure(&mut j);
    let action = j.next_decision();
    let crate::DecisionPayload::HuntSkipped(skip) = &action.payload else {
        panic!("a first unattributed failure is not hunted: {action:?}");
    };
    assert_eq!(skip.failure, first);
    assert!(skip.reason.starts_with("held"), "{}", skip.reason);
    tokens(&skip.reason, &["cores [00, 01]", "K = 5", "cycle 8"]);
    assert_eq!(action.cause, [first]);
    assert_eq!(strikes(&j), 0);
    let hold = j.decide(action);
    assert!(j.run().failures.queue_is_empty());
    assert_eq!(strikes(&j), 1);
    assert_eq!(held(&j), [first]);
    assert_eq!(j.run().bios_profile().since(), Some(hold));
    j.assert_resumes();

    let second = idle_failure(&mut j);
    let start = drive_to_hunt_start(&mut j);
    let crate::DecisionPayload::HuntStart(payload) = &start.payload else {
        unreachable!()
    };
    assert_eq!(payload.failure, second);
    assert_eq!(start.cause, [second, first]);
    tokens(
        payload.reason.as_deref().unwrap_or_default(),
        &[&format!("#{first}"), &format!("#{second}")],
    );
    let started = j.decide(start);
    let paired = j.run().hunts.active.as_ref().unwrap().paired;
    assert_eq!(paired.map(|strike| strike.failure), Some(first));
    hunt_end(&mut j, started, HuntOutcome::Culprit, &[0]);
    let (backoff, _) = next(&j);
    assert_eq!(backoff.activity, Activity::Hunt);
    tokens(
        &backoff.reason,
        &[&format!("after failures #{first} and #{second}")],
    );
    j.assert_resumes();
}

#[test]
fn a_paired_hunt_ending_direct_names_both_failures_in_the_backoff() {
    let mut j = later_session(&[-10, -12]);
    start_cycle(&mut j);
    let first = idle_failure(&mut j);
    let hold = j.next_decision();
    j.decide(hold);
    let second = idle_failure(&mut j);
    let start = drive_to_hunt_start(&mut j);
    j.decide(start);
    parked_failure(&mut j, 0, -10, &[-10, 0]);
    let end = j.next_decision();
    assert!(
        matches!(&end.payload, crate::DecisionPayload::HuntEnd(end) if end.result == HuntOutcome::Direct),
        "{end:?}"
    );
    j.decide(end);
    let (backoff, cause) = next(&j);
    assert_eq!(
        (backoff.activity, backoff.core),
        (Activity::Hunt, core(0)),
        "{backoff:?}"
    );
    tokens(
        &backoff.reason,
        &[&format!("after failures #{first} and #{second}")],
    );
    j.decide(crate::DecisionAction::new(backoff, cause));
    j.assert_resumes();
}

/// Decides every decision before the next hunt start, reading the ranking when asked, and
/// returns the start undecided.
pub(crate) fn drive_to_hunt_start(j: &mut Journal) -> crate::DecisionAction {
    for _ in 0..50 {
        match j.next() {
            Action::Decide(decision)
                if matches!(decision.payload, crate::DecisionPayload::HuntStart(_)) =>
            {
                return decision;
            }
            Action::Decide(decision) => {
                j.decide(decision);
            }
            Action::ReadRanking => {
                let ids: Vec<u8> = j.run().topo_ref().order().iter().map(|c| c.get()).collect();
                j.ranking(&ids);
            }
            other @ Action::RunTrial(_) => panic!("expected a hunt start, got {other:?}"),
        }
    }
    panic!("the hunt never started");
}

/// Records the end of hunt 1 with `result` over `cores`, citing its start.
pub(crate) fn hunt_end(j: &mut Journal, start: Seq, result: HuntOutcome, cores: &[u8]) -> Seq {
    j.append(
        HuntEnd {
            hunt: std::num::NonZeroU32::MIN,
            result,
            cores: crate::testkit::set(cores),
            members: Vec::new(),
            groups: 1,
            reason: String::new(),
        }
        .into(),
        &[start],
    )
}

/// An attributed failure of core `id` at `offset` in a parked trial at `profile`.
pub(crate) fn parked_failure(j: &mut Journal, id: u8, offset: i64, profile: &[i64]) -> Seq {
    let profile = j.profile(profile).offsets();
    j.failure(
        togi_journal::Failure {
            signal: Signal::ComputationError,
            attribution: togi_journal::Attribution::Attributed,
            core: Some(core(id)),
            offset: Some(off(offset)),
            trial: None,
            regime: Some(Regime::R1),
            condition: Some(Condition::Parked),
            profile: Some(profile),
            known_failure: None,
            reason: None,
            round: None,
        },
        &[],
    )
}

/// Runs a hunt group's parked trial, failing it with a crash after 5 s when `fail`, and records
/// the failure's attribution.
pub(crate) fn run_group(j: &mut Journal, plan: &crate::TrialPlan, fail: bool) {
    assert_eq!(plan.condition, Condition::Parked, "{plan:?}");
    if !fail {
        j.trial(plan, Outcome::Pass);
        return;
    }
    let (intent, trial) = j.intent(plan);
    let mut end = Journal::end_payload(&trial, Outcome::Failure, Some(Signal::Crash));
    end.duration_s = 5;
    j.append(end.into(), &[intent]);
    let attribution = j.next_decision();
    assert!(
        matches!(&attribution.payload, crate::DecisionPayload::Failure(f) if f.attribution == togi_journal::Attribution::Unattributed),
        "{attribution:?}"
    );
    j.decide(attribution);
}

#[test]
fn an_unattributed_hold_shows_only_the_cores_its_hunt_would_search() {
    let mut j = later_session(&[-10, -12]);
    let decided = current(&j);
    j.profile_change(&[-10, -8]);
    pass_cycles(&mut j, 1);
    j.profile_change(&decided);
    pass_cycles(&mut j, 1);
    assert!(j.run().later_gate().is_some());
    start_cycle(&mut j);
    idle_failure(&mut j);
    let action = j.next_decision();
    assert!(matches!(
        action.payload,
        crate::DecisionPayload::HuntSkipped(_)
    ));
    j.decide(action);
    let shown = j.run().bios_profile();
    assert_eq!(
        shown
            .unconfirmed
            .iter()
            .map(|mark| mark.core)
            .collect::<Vec<_>>(),
        [core(1)],
        "core 00 passed a full cycle at its offset, so the hunt would not search it"
    );
    assert_eq!(current_of(&j.run().bios_profile().shown), [-10, -11]);
    j.assert_resumes();
}

#[test]
fn a_held_unattributed_failure_pairs_with_an_attributed_one_on_a_loaded_core() {
    let mut j = later_session(&[-10, -12]);
    start_cycle(&mut j);
    let first = idle_failure(&mut j);
    let action = j.next_decision();
    j.decide(action);
    let second = fail_core(&mut j, 1);
    let (step, cause) = next(&j);
    assert!(!is_hold(&step) && step.core == core(1), "{step:?}");
    assert_eq!(cause, [second, first]);
    tokens(&step.reason, &[&format!("#{first}")]);
    let action = j.next_decision();
    j.decide(action);
    assert_eq!(strikes(&j), 0);
    j.assert_resumes();
}

#[test]
fn the_unattributed_hold_ignores_a_failure_that_reaches_a_constraint() {
    let mut j = later_session(&[-10, -12]);
    start_cycle(&mut j);
    j.combination(1, &[(0, -10), (1, -12)]);
    idle_failure(&mut j);
    let action = j.next_decision();
    let crate::DecisionPayload::HuntSkipped(skip) = &action.payload else {
        panic!("{action:?}");
    };
    assert!(
        skip.reason.starts_with("its profile reaches"),
        "{}",
        skip.reason
    );
    j.decide(action);
    assert_eq!(strikes(&j), 0);
}

#[test]
fn strikes_stay_bounded_by_cores_and_the_window() {
    let sizes = |cycles: usize| {
        let mut j = later_session(&[-30, -30, -30, -30]);
        start_cycle(&mut j);
        for round in 0..cycles {
            fail_core(&mut j, u8::try_from(round % 4).unwrap());
            while let Ok(Action::Decide(action)) = j.state.next() {
                j.decide(action);
            }
            end_cycle(&mut j);
            start_cycle(&mut j);
        }
        // An invariant on stored state, not behavior: strikes kept in memory stay bounded,
        // including ones the BIOS profile no longer shows.
        let by_core = j.run().later.strikes().count();
        j.assert_resumes();
        by_core
    };
    let small = sizes(12);
    let large = sizes(40);
    assert!(small <= 4 * WINDOW as usize && large <= 4 * WINDOW as usize);
}

#[test]
fn repeated_decisions_are_equal_and_pure() {
    let mut j = later_session(&[-10, -12]);
    start_cycle(&mut j);
    fail_core(&mut j, 0);
    let before = j.state.clone();
    let first = j.state.next().ok();
    assert_eq!(first, j.state.next().ok());
    assert_eq!(before, j.state);
    let bios = j.run().bios_profile();
    assert_eq!(bios, j.run().bios_profile());
}

fn failure_ops() -> impl Strategy<Value = Vec<(u8, bool)>> {
    proptest::collection::vec((0..3_u8, any::<bool>()), 1..14)
}

proptest! {
    #![proptest_config(crate::prop_tests::seeded(12))]

    #[test]
    fn after_conclusion_no_ordinary_decision_deepens_any_core(ops in failure_ops()) {
        let mut j = later_session(&[-10, -12, -14]);
        start_cycle(&mut j);
        let mut before = current(&j);
        for (id, close) in ops {
            fail_core(&mut j, id);
            settle(&mut j);
            let now = current(&j);
            prop_assert!(now.iter().zip(&before).all(|(now, before)| now >= before));
            before = now;
            if close {
                end_cycle(&mut j);
                start_cycle(&mut j);
            }
        }
        j.assert_resumes();
    }
}

#[test]
fn a_failure_before_the_latest_full_cycle_is_not_held() {
    for attributed in [false, true] {
        let mut j = later_session(&[-10, -12]);
        start_cycle(&mut j);
        let failure = if attributed {
            fail_core(&mut j, 0)
        } else {
            idle_failure(&mut j)
        };
        let full = end_cycle(&mut j);
        assert!(full > failure);
        assert!(j.run().later_gate().is_some());
        if attributed {
            let action = j.run().ordinary_decision().unwrap();
            let answered = j.run().later_decision(action.clone());
            assert_eq!(answered, action);
            let Action::Decide(action) = answered else {
                panic!("expected the pending backoff");
            };
            assert_eq!(decision(&action).decision, Decision::Backoff);
            assert_eq!(decision(&action).to_offset, off(-9));
            assert_eq!(action.cause, [failure]);
        } else {
            assert!(j.run().later_hunt_hold().is_none());
            assert_eq!(j.run().failures.queue().next(), Some(failure));
        }
        assert_eq!(strikes(&j), 0);
        j.assert_resumes();
    }
}

#[test]
fn a_passed_full_cycle_before_any_checking_profile_confirms_nothing() {
    let mut j = Journal::new(&[0], &[Start::search(-10)]);
    assert!(j.run().checking.profile().is_none());
    j.cycle_start(1, &[Regime::R1]);
    j.cycle_end(1, true, true);
    assert!(j.run().bios_profile().confirmed.is_none());
    j.assert_resumes();
}
