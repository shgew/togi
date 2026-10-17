use proptest::prelude::*;
use togi_journal::{
    Activity, Attribution, DeadEndCondition, Decision, HuntOutcome, Outcome, Payload, RoundEvent,
    Seq, TunerDecision,
};
use togi_machine::{Condition, Regime};

use crate::later_tests::{fail_core, is_hold, later_session, start_cycle};
use crate::prop_tests::seeded;
use crate::testkit::{Journal, Start, config, fresh, set};
use crate::tests::{core, off};
use crate::{Action, DecisionAction, DecisionPayload, Load, TrialPlan};

fn solo_cores(offsets: &[i64], solo: i64) -> Journal {
    let ids: Vec<u8> = (0..offsets.len())
        .map(|id| u8::try_from(id).unwrap())
        .collect();
    let starts: Vec<Start> = offsets
        .iter()
        .map(|&offset| Start::solo(offset, solo))
        .collect();
    Journal::new(&ids, &starts)
}

fn drive(j: &mut Journal, done: impl Fn(&Journal) -> bool) {
    for _ in 0..4000 {
        if done(j) {
            return;
        }
        j.step(&mut |_| Outcome::Pass);
    }
    panic!("never reached the awaited state");
}

fn decided(action: Option<DecisionAction>) -> DecisionAction {
    action.expect("drain has a decision")
}

fn tuner_decision(action: &DecisionAction) -> &TunerDecision {
    let DecisionPayload::TunerDecision(decision) = &action.payload else {
        panic!("expected a tuner decision, got {:?}", action.payload);
    };
    decision
}

#[test]
fn drain_stops_at_trial_boundary() {
    let mut j = Journal::search_at(&[-10]);
    let plan = j.next_trial();
    j.trial(&plan, Outcome::Failure);
    let attribution = decided(j.state.drain());
    let DecisionPayload::Failure(failure) = &attribution.payload else {
        panic!("drain attribution: {attribution:?}");
    };
    assert_eq!(failure.core, Some(core(0)));
    j.decide(attribution);
    let back = decided(j.state.drain());
    let backoff = tuner_decision(&back);
    assert_eq!(backoff.to_offset, off(-5));
    assert_eq!(backoff.failure_point, Some(off(-10)));
    j.decide(back);
    assert_eq!(j.state.drain(), None);
    let Action::RunTrial(next) = j.next() else {
        panic!("next lost search after drain");
    };
    assert_eq!(next.profile.get(core(0)), Some(off(-5)));
}

#[test]
fn drain_rejects_unattributed_failure_at_zero() {
    let mut j = Journal::has_room(&[0, 0]);
    let failure = j.idle_failure(j.offsets().offsets(), Condition::Together);
    let drained = decided(j.state.drain());
    let DecisionPayload::DeadEnd(dead_end) = &drained.payload else {
        panic!("drain missed the all-zero dead end: {drained:?}");
    };
    assert_eq!(dead_end.condition, DeadEndCondition::FailureAtZero);
    assert_eq!(dead_end.core, None);
    assert_eq!(drained.cause, [failure]);
    assert_eq!(j.next(), Action::Decide(drained));
}

#[test]
fn phase2_blame_in_drain_matches_next() {
    let mut j = solo_cores(&[-10, -10], -11);
    drive(&mut j, |j| {
        j.run().phases.confirm_start().is_some() && j.run().checking.open().is_some()
    });
    j.idle_failure(j.profile(&[-11, -11]).offsets(), Condition::Together);
    let drained = decided(j.state.drain());
    assert_eq!(j.next(), Action::Decide(drained.clone()));
    assert_eq!(tuner_decision(&drained).activity, Activity::Deepening);
}

#[test]
fn later_drain_emits_the_same_hold_and_pair() {
    let mut j = later_session(&[-10, -12]);
    start_cycle(&mut j);
    fail_core(&mut j, 0);
    let drained = decided(j.state.drain());
    assert_eq!(j.next(), Action::Decide(drained.clone()));
    assert!(is_hold(tuner_decision(&drained)), "{drained:?}");
    j.decide(drained);
    fail_core(&mut j, 0);
    let drained = decided(j.state.drain());
    assert_eq!(j.next(), Action::Decide(drained.clone()));
    assert_eq!(drained.cause.len(), 2, "{drained:?}");
}

const SHORT: u32 = 120;

fn limit_session(count: u8) -> Journal {
    let ids: Vec<u8> = (0..count).collect();
    let starts = vec![Start::at_limit(-30).fail(-31); usize::from(count)];
    let mut j = Journal::new(&ids, &starts);
    j.profile_change(&vec![-30; usize::from(count)]);
    j.ranking(&ids);
    j
}

fn idle_config(j: &mut Journal, secs: u32) {
    if j.run().config.checking_idle.get() != secs {
        j.load(&config(&format!(
            r#"{{"durations":{{"checking_idle_s":{secs}}}}}"#
        )));
    }
}

fn attribute(j: &mut Journal) -> Seq {
    let decision = j.next_decision();
    assert!(
        matches!(&decision.payload, DecisionPayload::Failure(f) if f.attribution == Attribution::Unattributed),
        "{decision:?}"
    );
    j.decide(decision)
}

fn fail_all_together(j: &mut Journal, duration: u32) -> Seq {
    idle_config(j, duration);
    let profile: Vec<i64> = j.offsets().iter().map(|(_, o)| o.get()).collect();
    let ids: Vec<u8> = (0..u8::try_from(profile.len()).unwrap()).collect();
    let plan = j.manual(
        Regime::R6,
        0,
        Load::Many(set(&ids)),
        Condition::Together,
        &profile,
        duration,
    );
    j.trial(&plan, Outcome::Failure);
    attribute(j)
}

fn start_hunt(j: &mut Journal) {
    let Action::Decide(start) = j.next() else {
        panic!("expected the hunt start");
    };
    assert!(matches!(start.payload, DecisionPayload::HuntStart(_)));
    j.decide(start);
}

fn carry_parked_failure_naming(j: &mut Journal, named: u8) {
    let class = togi_journal::TrialClass {
        regime: Regime::R6,
        workload: togi_machine::WorkloadId::new(Regime::R6, 0),
        cores: set(&[0, 1]),
        duration_s: SHORT,
    };
    let mut carried = j.carried_trial(
        "20230101T000000Z",
        class,
        Condition::Parked,
        &[0, -30],
        Outcome::Failure,
        None,
    );
    carried.core = Some(core(named));
    j.append(carried.into(), &[]);
}

#[test]
fn drain_commits_resolved_hunt_without_starting_groups() {
    let mut j = limit_session(2);
    carry_parked_failure_naming(&mut j, 1);
    fail_all_together(&mut j, 600);
    start_hunt(&mut j);
    let mut end = None;
    for _ in 0..20 {
        let Action::Decide(decision) = j.next() else {
            panic!("the resolving hunt started a group trial");
        };
        let drained = j.state.drain();
        if let DecisionPayload::HuntEnd(found) = &decision.payload {
            assert_eq!(drained.as_ref(), Some(&decision));
            end = Some(found.clone());
            j.decide(decision);
            break;
        }
        assert_eq!(drained, None, "a group plan is not drained");
        j.decide(decision);
    }
    let end = end.expect("the known failure resolved the hunt without a trial");
    assert_eq!(end.result, HuntOutcome::Culprit);
    assert_eq!(
        end.cores
            .iter()
            .map(togi_machine::CoreId::get)
            .collect::<Vec<_>>(),
        [1]
    );
    let commitment = decided(j.state.drain());
    assert_eq!(j.next(), Action::Decide(commitment.clone()));
    let moved = tuner_decision(&commitment);
    assert_eq!(moved.decision, Decision::Backoff);
    assert_eq!((moved.core, moved.to_offset), (core(1), off(-29)));
    j.decide(commitment);
    assert_eq!(j.state.drain(), None);
    j.assert_resumes();
}

#[test]
fn drain_records_fallback_combination_before_backoff() {
    let mut j = limit_session(2);
    fail_all_together(&mut j, SHORT);
    start_hunt(&mut j);
    for _ in 0..100 {
        let action = j.next();
        if let Action::Decide(decision) = &action {
            if matches!(&decision.payload, DecisionPayload::HuntEnd(end) if end.result == HuntOutcome::Fallback)
            {
                assert_eq!(j.state.drain().as_ref(), Some(decision));
                break;
            }
        }
        j.step(&mut |_| Outcome::Pass);
    }
    let closing = decided(j.state.drain());
    assert!(
        matches!(&closing.payload, DecisionPayload::HuntEnd(end) if end.result == HuntOutcome::Fallback),
        "drain did not close the unresolved hunt: {closing:?}"
    );
    j.decide(closing);
    let record = decided(j.state.drain());
    assert_eq!(j.next(), Action::Decide(record.clone()));
    let DecisionPayload::Combination(combination) = &record.payload else {
        panic!("drain lost the fallback combination: {record:?}");
    };
    assert!(combination.fallback);
    let members: Vec<_> = combination
        .members
        .iter()
        .map(|member| (member.core, member.offset))
        .collect();
    assert_eq!(members, [(core(0), off(-30)), (core(1), off(-30))]);
    j.decide(record);
    let back = decided(j.state.drain());
    assert_eq!(j.next(), Action::Decide(back.clone()));
    let moved = tuner_decision(&back);
    assert_eq!(moved.decision, Decision::Backoff);
    assert_eq!(moved.to_offset, off(-29));
    j.decide(back);
    assert_eq!(
        j.run().constraints.reaches(&j.run().offsets()).unwrap(),
        None,
        "drain left the together offsets reaching the combination"
    );
    assert_eq!(j.state.drain(), None);
    j.assert_resumes();
}

#[test]
fn zero_movable_candidates_never_conclude_at_phase1_end() {
    let mut j = Journal::new(&[0], &[Start::solo(-10, -10)]);
    drive(&mut j, |j| j.run().phases.phase1_end().is_some());
    assert_eq!(j.state.clean_cycles(), 0);
    assert!(!j.run().round_due());
    let plan = j.state.phase_plan().unwrap();
    assert!(plan.confirming);
    assert_eq!(plan.concluded, None);
    let DecisionPayload::CheckingCycle(cycle) = j.next_decision().payload else {
        panic!("expected the confirmation cycle to start");
    };
    assert_eq!(cycle.event, togi_journal::CycleEvent::Start);
    assert!(
        cycle
            .reason
            .as_deref()
            .unwrap_or_default()
            .contains("confirmation"),
        "{cycle:?}"
    );
}

#[test]
fn active_deepening_projection() {
    for close in ["passed", "cancelled"] {
        let mut j = solo_cores(&[-10, -10, -10, -10], -11);
        drive(&mut j, |j| j.run().phases.phase1_end().is_some());
        let begin = j.round_start(2, &[-15, -9, -10, -10], &[0, 1], 2);
        let plan = j.state.phase_plan().unwrap();
        assert_eq!(plan.round, Some(2), "{close}");
        let moving: Vec<_> = plan
            .candidates
            .iter()
            .filter(|candidate| candidate.moving)
            .map(|candidate| (candidate.core, candidate.offset))
            .collect();
        assert_eq!(moving, [(core(0), off(-10))], "{close}");
        assert_eq!(fresh(&j.events).phase_plan(), Some(plan));
        if close == "cancelled" {
            j.round_end(2, begin);
        } else {
            drive(&mut j, |j| j.run().rounds.active().is_none());
        }
        assert_eq!(j.state.phase_plan().unwrap().round, None, "{close}");
        assert_eq!(fresh(&j.events).phase_plan(), j.state.phase_plan());
    }
}

fn outcome(byte: u8) -> Outcome {
    match byte {
        0..=179 => Outcome::Pass,
        180..=239 => Outcome::Failure,
        _ => Outcome::Inconclusive,
    }
}

fn scripted_session(shape: u8) -> Journal {
    match shape % 3 {
        0 => Journal::new(
            &[0, 1, 2],
            &[Start::search(-10), Start::search(-30), Start::search(-5)],
        ),
        1 => Journal::new(
            &[0, 1, 2],
            &[
                Start::solo(-10, -12),
                Start::solo(-8, -9),
                Start::solo(-10, -11),
            ],
        ),
        _ => limit_session(3),
    }
    .resuming_every(7)
}

fn applies(j: &mut Journal, op: u8, byte: u8) -> bool {
    match op % 24 {
        0 | 1 => {
            j.idle_failure(j.offsets().offsets(), Condition::Together);
        }
        2 => {
            j.reset(op / 24 % 3);
        }
        _ => {
            let action = j.step(&mut |_: &TrialPlan| outcome(byte));
            if matches!(
                action,
                Action::Decide(DecisionAction {
                    payload: DecisionPayload::DeadEnd(_),
                    ..
                })
            ) {
                return false;
            }
        }
    }
    true
}

fn check_subset(j: &Journal) -> Result<(), TestCaseError> {
    let before = j.state.clone();
    let drained = j.state.drain();
    prop_assert_eq!(&before, &j.state);
    let run = j.run();
    let shared = run
        .attribution()
        .or_else(|| run.warning_decision())
        .or_else(|| run.unattributed_zero_dead_end())
        .or_else(|| run.phase2_blame_action())
        .or_else(|| {
            match run
                .ordinary_decision()
                .map(|action| run.later_decision(action))
            {
                Some(Action::Decide(decision)) => Some(decision),
                Some(Action::RunTrial(_) | Action::ReadRanking) | None => None,
            }
        })
        .or_else(|| run.hunt_closing());
    prop_assert_eq!(&drained, &shared);
    let Some(drained) = drained else {
        return Ok(());
    };
    prop_assert!(!starts_work(&drained.payload), "{drained:?}");
    let next = j
        .state
        .next()
        .expect("drain has a decision, so next has an action");
    match next {
        Action::Decide(decision) if decision == drained => {}
        Action::Decide(decision) => {
            prop_assert!(
                Some(&decision) == run.queued_reset().as_ref()
                    || Some(&decision) == run.zero_point_dead_end().as_ref(),
                "next {decision:?} differs from drain {drained:?} without a reset or zero dead end ahead of it"
            );
            prop_assert!(
                run.attribution().is_none() && run.warning_decision().is_none(),
                "steps 1 and 2 are first in both chains"
            );
        }
        Action::RunTrial(_) | Action::ReadRanking => {
            let closing = run.hunt_closing();
            prop_assert_eq!(
                Some(&drained),
                closing.as_ref(),
                "next waits on work while drain returned a decision that is not a hunt closing"
            );
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(seeded(40))]

    #[test]
    fn drain_subset_of_next(
        shape in any::<u8>(),
        script in proptest::collection::vec((any::<u8>(), any::<u8>()), 1..260),
    ) {
        let mut j = scripted_session(shape);
        for (op, byte) in script {
            check_subset(&j)?;
            if !applies(&mut j, op, byte) {
                break;
            }
        }
        check_subset(&j)?;
    }
}

fn trial_events(j: &Journal) -> usize {
    j.events
        .iter()
        .filter(|event| {
            matches!(
                event.payload,
                Payload::TrialIntent(_) | Payload::TrialEnd(_) | Payload::TrialStart(_)
            )
        })
        .count()
}

fn starts_work(payload: &DecisionPayload) -> bool {
    match payload {
        DecisionPayload::CheckingCycle(_)
        | DecisionPayload::CheckingStep(_)
        | DecisionPayload::CheckingChain(_)
        | DecisionPayload::ProfileChange(_)
        | DecisionPayload::CorePhase(_)
        | DecisionPayload::HuntStart(_)
        | DecisionPayload::HuntGroup(_) => true,
        DecisionPayload::DeepeningRound(round) => round.event == RoundEvent::Start,
        DecisionPayload::Failure(_)
        | DecisionPayload::TunerWarning(_)
        | DecisionPayload::DeadEnd(_)
        | DecisionPayload::TunerDecision(_)
        | DecisionPayload::HuntEnd(_)
        | DecisionPayload::HuntSkipped(_)
        | DecisionPayload::Combination(_) => false,
    }
}

fn drain_dry(j: &mut Journal) -> Result<(), TestCaseError> {
    let trials = trial_events(j);
    for _ in 0..64 {
        let Some(drained) = j.state.drain() else {
            prop_assert_eq!(trial_events(j), trials);
            return Ok(());
        };
        prop_assert!(!starts_work(&drained.payload), "{drained:?}");
        j.decide(drained);
    }
    Err(TestCaseError::fail("drain never ran dry"))
}

proptest! {
    #![proptest_config(seeded(40))]

    #[test]
    fn drain_never_returns_a_trial(
        shape in any::<u8>(),
        script in proptest::collection::vec((any::<u8>(), any::<u8>()), 1..260),
    ) {
        let mut j = scripted_session(shape);
        for (index, (op, byte)) in script.into_iter().enumerate() {
            if index % 9 == 8 {
                drain_dry(&mut j)?;
            }
            if !applies(&mut j, op, byte) {
                break;
            }
        }
        drain_dry(&mut j)?;
        j.assert_resumes();
    }
}
