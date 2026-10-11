use togi_journal::{Attribution, CrashDetected, DeadEndCondition, Decision, Mce, Outcome, Payload};
use togi_machine::{BankType, Condition, CpuId, Regime, ResetKind, Signal};

use crate::schedule_tests::transition;
use crate::testkit::{Journal, Start, set};
use crate::tests::core;
use crate::{Action, DecisionAction, DecisionPayload, FoldError, Load, State};

fn failure_of(action: &DecisionAction) -> &togi_journal::Failure {
    match &action.payload {
        DecisionPayload::Failure(failure) => failure,
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn persistent_zero_point_stops_before_trial() {
    let mut j = Journal::search_at(&[0]);
    let mut phase = transition(0, togi_journal::CorePhase::Search, 0);
    phase.failure_point = Some(crate::tests::off(0));
    let seq = j.core_phase(phase);
    let stop = j.next_decision();
    let DecisionPayload::DeadEnd(dead_end) = &stop.payload else {
        panic!("zero failure point did not stop tuning: {stop:?}");
    };
    assert_eq!(dead_end.condition, DeadEndCondition::FailureAtZero);
    assert_eq!(dead_end.core, Some(core(0)));
    assert_eq!(stop.cause, [seq]);
}

#[test]
fn an_alone_failure_at_zero_is_a_dead_end() {
    let mut j = Journal::search_at(&[0]);
    let plan = j.next_trial();
    j.trial(&plan, Outcome::Failure);
    let attribution = j.next_decision();
    j.decide(attribution);
    let stop = j.next_decision();
    let DecisionPayload::DeadEnd(dead_end) = &stop.payload else {
        panic!("expected a dead end, got {stop:?}");
    };
    assert_eq!(dead_end.condition, DeadEndCondition::FailureAtZero);
    j.decide(stop);
    assert_eq!(j.run().core(core(0)).pending, None);
    let state = j.run().core(core(0));
    assert_eq!(state.offset.get(), 0);
}

fn mce(trial: &togi_journal::TrialId, core_id: u8) -> Mce {
    Mce {
        cpu: CpuId(u16::from(core_id)),
        core: Some(core(core_id)),
        bank: 0,
        bank_type: BankType::LoadStore,
        corrected: false,
        monotonic_ns: None,
        from_boot: None,
        trial: Some(trial.clone()),
        between_trials: false,
        lines: Vec::new(),
    }
}

#[test]
fn thermal_reason_yields_to_mce_evidence() {
    let mut j = Journal::search_at(&[-10]);
    let plan = j.next_trial();
    let (intent, trial) = j.intent(&plan);
    let evidence = j.append(mce(&trial, 0).into(), &[]);
    j.append(
        CrashDetected {
            previous_boot: togi_machine::BootId::from("boot".to_owned()),
            in_flight: Some(intent),
            stray: false,
            condition: None,
            reset_reason: Some(ResetKind::ThermalTrip),
            reset_reason_raw: Vec::new(),
            inconclusive: false,
            pstore: None,
        }
        .into(),
        &[evidence],
    );
    let end = Journal::end_payload(&trial, Outcome::Failure, Some(Signal::Crash));
    let end_seq = j.append(end.into(), &[intent, evidence]);
    let action = j.next_decision();
    let failure = failure_of(&action);
    assert_eq!(failure.attribution, Attribution::Attributed);
    assert_eq!(failure.core, Some(core(0)));
    assert_eq!(action.cause, [end_seq, intent, evidence]);
}

#[test]
fn a_crash_keeps_the_trial_in_flight_until_its_end() {
    let mut j = Journal::search_at(&[-10]);
    let plan = j.next_trial();
    let (intent, trial) = j.intent(&plan);
    j.append(
        CrashDetected {
            previous_boot: togi_machine::BootId::from("boot".to_owned()),
            in_flight: Some(intent),
            stray: false,
            condition: None,
            reset_reason: None,
            reset_reason_raw: Vec::new(),
            inconclusive: false,
            pstore: None,
        }
        .into(),
        &[],
    );
    let open = j.run().flight.open.as_ref().unwrap();
    assert!(open.crashed);
    assert_eq!(open.requirement, plan.requirement);
    j.end(intent, &trial, Outcome::Failure);
    assert!(j.run().flight.open.is_none());
}

#[test]
fn together_crash_names_the_sole_nonzero_core_by_id() {
    let mut j = Journal::new(&[0, 8], &[Start::at_limit(0), Start::at_limit(-12)]);
    let plan = j.manual(
        Regime::R6,
        0,
        Load::Many(set(&[0, 8])),
        Condition::Together,
        &[0, -12],
        120,
    );
    j.trial(&plan, Outcome::Failure);
    let action = j.next_decision();
    let failure = failure_of(&action);
    assert_eq!(failure.attribution, Attribution::Attributed);
    assert_eq!(failure.core, Some(core(8)));
    assert_eq!(
        failure.offset.map(togi_machine::TargetOffset::get),
        Some(-12)
    );
}

#[test]
fn core_local_mces_name_the_core_of_a_together_failure() {
    let run = |cores: &[u8]| {
        let mut j = Journal::new(
            &[0, 1, 2],
            &[
                Start::at_limit(-10),
                Start::at_limit(-10),
                Start::at_limit(-10),
            ],
        );
        let plan = j.manual(
            Regime::R6,
            0,
            Load::Many(set(&[0, 1, 2])),
            Condition::Together,
            &[-10, -10, -10],
            120,
        );
        let (intent, trial) = j.intent(&plan);
        for &id in cores {
            j.append(mce(&trial, id).into(), &[]);
        }
        j.end(intent, &trial, Outcome::Failure);
        let action = j.next_decision();
        failure_of(&action).clone()
    };
    let one = run(&[1]);
    assert_eq!(
        (one.attribution, one.core),
        (Attribution::Attributed, Some(core(1)))
    );
    let two = run(&[0, 1]);
    assert_eq!(
        (two.attribution, two.core),
        (Attribution::Unattributed, None)
    );
    let none = run(&[]);
    assert_eq!(none.attribution, Attribution::Unattributed);
}

#[test]
fn class_key_distinguishes_single_full_ccd_loads() {
    let j = Journal::has_room(&[-10, -11, -12, -13]);
    let single = j.manual(
        Regime::R1,
        0,
        Load::Single(core(1)),
        Condition::Together,
        &[-10, -11, -12, -13],
        120,
    );
    let many_of_one = j.manual(
        Regime::R1,
        0,
        Load::Many(set(&[1])),
        Condition::Together,
        &[-10, -11, -12, -13],
        120,
    );
    let part = j.manual(
        Regime::R1,
        0,
        Load::Many(set(&[0, 1])),
        Condition::Together,
        &[-10, -11, -12, -13],
        120,
    );
    let all = j.manual(
        Regime::R1,
        0,
        Load::Many(set(&[0, 1, 2, 3])),
        Condition::Together,
        &[-10, -11, -12, -13],
        120,
    );
    let run = j.run();
    let classes = [&single, &part, &all].map(|plan| run.plan_class(plan));
    assert_ne!(classes[0], classes[1]);
    assert_ne!(classes[1], classes[2]);
    assert_ne!(classes[0], classes[2]);
    assert_eq!(run.plan_class(&single), run.plan_class(&many_of_one));
    assert!(!classes[2].multi_r7());
}

#[test]
fn a_second_session_start_is_an_error() {
    let j = Journal::search_at(&[-10]);
    let mut state = j.state.clone();
    let start = j.events[0].clone();
    let mut again = start;
    again.seq = j.next_seq();
    assert_eq!(state.fold(&again), Err(FoldError::DuplicateStart));
    assert_eq!(state, j.state);
}

#[test]
fn events_before_the_start_are_refused() {
    let j = Journal::search_at(&[-10]);
    let mut state = State::new();
    assert_eq!(state.fold(&j.events[1]), Err(FoldError::NotStarted));
    assert_eq!(state, State::new());
}

#[test]
fn a_fold_error_leaves_the_state_as_it_was() {
    let mut j = Journal::search_at(&[-10, -10]);
    let plan = j.next_trial();
    let (intent, trial) = j.intent(&plan);
    let other = togi_journal::TrialId::from("elsewhere".to_owned());
    let outside = core(9);
    let cases: Vec<(Payload, FoldError)> = vec![
        (
            transition(9, togi_journal::CorePhase::Search, 0).into(),
            FoldError::UnknownCore(outside),
        ),
        (
            Journal::end_payload(&other, Outcome::Pass, None).into(),
            FoldError::NoOpenTrial(other.clone()),
        ),
        (
            Journal::end_payload(&trial, Outcome::Failure, None).into(),
            FoldError::MissingSignal,
        ),
        (
            togi_journal::Failure {
                signal: Signal::Crash,
                attribution: Attribution::Unattributed,
                core: None,
                offset: None,
                trial: Some(other.clone()),
                regime: None,
                condition: Some(Condition::Together),
                profile: None,
                known_failure: None,
                reason: None,
                round: None,
            }
            .into(),
            FoldError::NotAwaiting(other.clone()),
        ),
        (
            togi_journal::Failure {
                signal: Signal::Crash,
                attribution: Attribution::Unattributed,
                core: None,
                offset: None,
                trial: None,
                regime: None,
                condition: Some(Condition::Together),
                profile: None,
                known_failure: Some(intent),
                reason: None,
                round: None,
            }
            .into(),
            FoldError::UnknownFailure(intent),
        ),
        (
            togi_journal::CommandReset {
                target: togi_journal::ResetTarget::Core(outside),
            }
            .into(),
            FoldError::UnknownCore(outside),
        ),
    ];
    for (payload, expected) in cases {
        let before = j.state.clone();
        assert_eq!(j.try_append(payload, &[]), Err(expected));
        assert_eq!(j.state, before);
    }
    let before = j.state.clone();
    let mut stale = j.event(
        Journal::end_payload(&trial, Outcome::Pass, None).into(),
        &[],
    );
    stale.seq = intent;
    assert_eq!(
        j.state.fold(&stale),
        Err(FoldError::OutOfOrder {
            last: intent,
            seq: intent
        })
    );
    assert_eq!(j.state, before);
}

#[test]
fn repeated_decisions_are_equal_and_leave_the_state_alone() {
    let mut j = Journal::search_at(&[-10, -20]);
    let mut seen = 0;
    for _ in 0..40 {
        let before = j.state.clone();
        let first = j.state.decide();
        let second = j.state.decide();
        assert_eq!(first, second);
        assert_eq!(j.state, before);
        let mut oracle = |_: &crate::TrialPlan| Outcome::Pass;
        j.step(&mut oracle);
        seen += 1;
    }
    assert_eq!(seen, 40);
    let _ = (Decision::Hold, Action::ReadRanking);
}
