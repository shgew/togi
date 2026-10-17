use togi_journal::{
    Attribution, CrashDetected, DeadEndCondition, Decision, Mce, Outcome, Payload, Seq, TrialId,
};
use togi_machine::{BankType, Condition, CpuId, Regime, ResetKind, Signal};

use crate::schedule_tests::transition;
use crate::testkit::{Journal, set};
use crate::tests::core;
use crate::{Action, DecisionAction, DecisionPayload, FoldError, Load, State};

fn failure_of(action: &DecisionAction) -> &togi_journal::Failure {
    let DecisionPayload::Failure(failure) = &action.payload else {
        panic!("expected a failure, got {:?}", action.payload);
    };
    failure
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

fn mce_on(trial: &togi_journal::TrialId, core_id: u8) -> Mce {
    mce(trial, core_id)
}

fn mce(trial: &togi_journal::TrialId, core_id: u8) -> Mce {
    Mce {
        cpu: CpuId::from(u16::from(core_id)),
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
    let mut j = Journal::new(&[0], &[crate::testkit::Start::search(-10).checking()]);
    let plan = j.next_trial();
    assert!(plan.requirement.needed.get() > 1, "{plan:?}");
    let (intent, trial) = j.intent(&plan);
    assert!(!j.state.forecast().branches.is_empty());
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
    assert!(j.state.forecast().branches.is_empty());
    assert_eq!(
        j.requirement().needed,
        u32::from(plan.requirement.needed.get())
    );
    j.end(intent, &trial, Outcome::Failure);
    assert_eq!(j.state.requirement(), None);
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
fn repeated_decisions_are_equal_and_leave_the_state_alone() {
    let mut j = Journal::search_at(&[-10, -20]);
    let mut seen = 0;
    for _ in 0..40 {
        let before = j.state.clone();
        let first = j.state.next().ok();
        let second = j.state.next().ok();
        assert_eq!(first, second);
        assert_eq!(j.state, before);
        let mut oracle = |_: &crate::TrialPlan| Outcome::Pass;
        j.step(&mut oracle);
        seen += 1;
    }
    assert_eq!(seen, 40);
    let _ = (Decision::Hold, Action::ReadRanking);
}

type Case = Box<dyn Fn(&TrialId, Seq) -> (Payload, Box<dyn Fn(&FoldError) -> bool>)>;
type MakePayload = Box<dyn Fn(&TrialId) -> Payload>;

fn poisoned(j: &Journal, state: &mut State) {
    let next = j.event(
        togi_journal::SessionNotice {
            notice: togi_journal::Notice::NonzeroBaseline,
            cores: togi_machine::CoreSet::default(),
        }
        .into(),
        &[],
    );
    assert!(matches!(state.fold(&next), Err(FoldError::Poisoned)));
    assert_eq!(state.next().ok(), None);
}

#[test]
fn together_failures_name_the_core_the_end_or_one_cited_mce_names() {
    use crate::failures::together_attribution;
    let topology = crate::tests::topology(&[0, 8]);
    let profile = crate::tests::profile(&topology, &[0, -12]);
    let both = crate::tests::profile(&topology, &[-3, -12]);
    let cores = |ids: &[u8]| set(ids);
    let named = |attribution, id: u8, offset| {
        (attribution, Some(core(id)), Some(crate::tests::off(offset)))
    };
    let none = (Attribution::Unattributed, None, None);
    assert_eq!(
        together_attribution(None, cores(&[]), &profile, false),
        named(Attribution::Attributed, 8, -12)
    );
    assert_eq!(together_attribution(None, cores(&[]), &both, false), none);
    assert_eq!(
        together_attribution(Some(core(0)), cores(&[8]), &both, false),
        named(Attribution::Attributed, 0, -3)
    );
    assert_eq!(
        together_attribution(None, cores(&[0]), &profile, false),
        named(Attribution::Attributed, 0, 0)
    );
    assert_eq!(
        together_attribution(None, cores(&[0, 8]), &profile, false),
        none
    );
    assert_eq!(together_attribution(None, cores(&[]), &profile, true), none);
    assert_eq!(
        together_attribution(Some(core(5)), cores(&[]), &profile, false),
        none
    );
}

#[test]
fn only_the_mces_a_trial_end_cites_name_a_core() {
    use crate::flight::cited_mces;
    let seen: std::collections::BTreeMap<Seq, _> = [
        (Seq::new(5).unwrap(), core(1)),
        (Seq::new(7).unwrap(), core(2)),
    ]
    .into_iter()
    .collect();
    let cause = |seqs: &[u64]| -> Vec<Seq> { seqs.iter().map(|&n| Seq::new(n).unwrap()).collect() };
    assert_eq!(cited_mces(&seen, &cause(&[5])), set(&[1]));
    assert_eq!(cited_mces(&seen, &cause(&[3, 7])), set(&[2]));
    assert_eq!(cited_mces(&seen, &cause(&[3])), set(&[]));
}

#[test]
fn a_between_trials_mce_is_not_remembered() {
    let mut j = Journal::has_room(&[-10, -10]);
    let plan = j.manual(
        Regime::R6,
        0,
        Load::Many(set(&[0, 1])),
        Condition::Together,
        &[-10, -10],
        900,
    );
    let between = j.append(
        Mce {
            between_trials: true,
            trial: None,
            ..mce(&togi_journal::TrialId::from("none".to_owned()), 0)
        }
        .into(),
        &[],
    );
    let (intent, trial) = j.intent(&plan);
    assert_eq!(
        crate::fold_tests::cited_core(&j, intent, &trial, &[between]),
        None
    );
}

#[test]
fn a_second_session_start_poisons_the_state() {
    let j = Journal::search_at(&[-10]);
    let mut state = j.state.clone();
    let mut again = j.events[0].clone();
    again.seq = j.next_seq();
    assert!(matches!(state.fold(&again), Err(FoldError::DuplicateStart)));
    poisoned(&j, &mut state);
}

#[test]
fn events_before_the_start_are_refused() {
    let j = Journal::search_at(&[-10]);
    let mut state = State::new();
    assert!(matches!(
        state.fold(&j.events[1]),
        Err(FoldError::NotStarted)
    ));
    poisoned(&j, &mut state);
}

#[test]
fn a_fold_error_poisons_the_state() {
    let other = togi_journal::TrialId::from("elsewhere".to_owned());
    let outside = core(9);
    let failure =
        |trial: Option<&togi_journal::TrialId>, known: Option<Seq>| togi_journal::Failure {
            signal: Signal::Crash,
            attribution: Attribution::Unattributed,
            core: None,
            offset: None,
            trial: trial.cloned(),
            regime: None,
            condition: Some(Condition::Together),
            profile: None,
            known_failure: known,
            reason: None,
            round: None,
        };
    let cases: Vec<Case> = vec![
        Box::new(move |_, _| {
            (
                transition(9, togi_journal::CorePhase::Search, 0).into(),
                Box::new(
                    move |error| matches!(error, FoldError::UnknownCore(found) if *found == outside),
                ),
            )
        }),
        Box::new({
            let other = other.clone();
            move |_, _| {
                let expected = other.clone();
                (
                    Journal::end_payload(&other, Outcome::Pass, None).into(),
                    Box::new(
                        move |error| matches!(error, FoldError::NoOpenTrial(found) if *found == expected),
                    ),
                )
            }
        }),
        Box::new(|trial, _| {
            (
                Journal::end_payload(trial, Outcome::Failure, None).into(),
                Box::new(|error| matches!(error, FoldError::MissingSignal)),
            )
        }),
        Box::new({
            let other = other.clone();
            move |_, _| {
                let expected = other.clone();
                (
                    failure(Some(&other), None).into(),
                    Box::new(
                        move |error| matches!(error, FoldError::NotAwaiting(found) if *found == expected),
                    ),
                )
            }
        }),
        Box::new({
            move |_, intent| {
                (
                    failure(None, Some(intent)).into(),
                    Box::new(
                        move |error| matches!(error, FoldError::UnknownFailure(found) if *found == intent),
                    ),
                )
            }
        }),
        Box::new(move |_, _| {
            (
                togi_journal::CommandReset {
                    target: togi_journal::ResetTarget::Core(outside),
                }
                .into(),
                Box::new(
                    move |error| matches!(error, FoldError::UnknownCore(found) if *found == outside),
                ),
            )
        }),
    ];
    for case in cases {
        let mut j = Journal::search_at(&[-10, -10]);
        let plan = j.next_trial();
        let (intent, trial) = j.intent(&plan);
        let (payload, expected) = case(&trial, intent);
        assert!(matches!(j.try_append(payload, &[]), Err(error) if expected(&error)));
        let mut state = j.state.clone();
        poisoned(&j, &mut state);
    }
    let mut j = Journal::search_at(&[-10, -10]);
    let plan = j.next_trial();
    let (intent, trial) = j.intent(&plan);
    let mut stale = j.event(
        Journal::end_payload(&trial, Outcome::Pass, None).into(),
        &[],
    );
    stale.seq = intent;
    assert!(matches!(
        j.state.fold(&stale),
        Err(FoldError::OutOfOrder { last, seq }) if last == intent && seq == intent
    ));
    let mut state = j.state.clone();
    poisoned(&j, &mut state);
}

#[test]
fn every_core_a_payload_names_must_be_in_the_topology() {
    let outside = core(9);
    let cases: Vec<(&str, MakePayload)> = vec![
        (
            "end core",
            Box::new(move |trial| {
                let mut end = Journal::end_payload(trial, Outcome::Pass, None);
                end.core = Some(outside);
                end.into()
            }),
        ),
        (
            "end stalled core",
            Box::new(move |trial| {
                let mut end = Journal::end_payload(trial, Outcome::Pass, None);
                end.stalled_core = Some(outside);
                end.into()
            }),
        ),
        (
            "end top requesters",
            Box::new(move |trial| {
                let mut end = Journal::end_payload(trial, Outcome::Pass, None);
                end.top_requesters = set(&[9]);
                end.into()
            }),
        ),
        (
            "end request lanes",
            Box::new(move |trial| {
                let mut end = Journal::end_payload(trial, Outcome::Pass, None);
                end.voltage_requests_v = [(outside, 1.0)].into_iter().collect();
                end.into()
            }),
        ),
        ("mce core", Box::new(move |trial| mce_on(trial, 9).into())),
        (
            "progress core",
            Box::new(move |trial| {
                togi_journal::TrialProgress {
                    trial: trial.clone(),
                    detail: String::new(),
                    signal: None,
                    core: Some(outside),
                }
                .into()
            }),
        ),
    ];
    for (name, make) in cases {
        let mut j = Journal::search_at(&[-10]);
        let plan = j.next_trial();
        let (_, trial) = j.intent(&plan);
        assert!(
            matches!(
                j.try_append(make(&trial), &[]),
                Err(FoldError::UnknownCore(found)) if found == outside
            ),
            "{name}"
        );
    }
}

#[test]
fn the_state_exposes_its_topology_and_constraints_until_it_is_poisoned() {
    let mut state = State::new();
    assert!(state.topology().is_none() && state.constraints().is_none());
    let j = Journal::has_room(&[-10, -12]);
    assert_eq!(j.state.topology().unwrap().all(), set(&[0, 1]));
    assert!(j.state.constraints().unwrap().combinations().is_empty());
    let mut poisoned = j.state.clone();
    poisoned.fold(&j.events[0]).unwrap_err();
    assert!(poisoned.topology().is_none() && poisoned.constraints().is_none());
    let _ = &mut state;
}
