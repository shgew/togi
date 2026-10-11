use togi_journal::{
    Activity, Attribution, CorePhase, Decision, Event, Failure, Outcome, Payload, RoundEvent,
    TunerDecision,
};
use togi_machine::{Condition, CoreId, CoreSet, PerCore, Regime, Signal};

use crate::testkit::{Journal, Start, fresh, set};
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

fn drive(j: &mut Journal, limit: usize, mut done: impl FnMut(&Journal) -> bool) {
    for _ in 0..limit {
        if done(j) {
            return;
        }
        j.step(&mut |_| Outcome::Pass);
    }
    panic!("never reached the awaited state");
}

fn kinds(events: &[Event]) -> Vec<&'static str> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::DeepeningRound(round) => Some(match round.event {
                RoundEvent::Start => "round-start",
                RoundEvent::End => "round-end",
            }),
            Payload::CheckingCycle(cycle) => Some(match cycle.event {
                togi_journal::CycleEvent::Start => "cycle-start",
                togi_journal::CycleEvent::End => "cycle-end",
            }),
            _ => None,
        })
        .collect()
}

#[test]
fn phase2_runs_rounds_then_always_the_confirmation_cycle() {
    let mut j = solo_cores(&[-10, -10], -11);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    let from = j.events.len();
    drive(&mut j, 4000, |j| j.run().phases.clean_cycles() == 1);
    let got = kinds(&j.events[from..]);
    assert_eq!(
        got,
        ["round-start", "round-end", "cycle-start", "cycle-end"]
    );
    assert_eq!(j.run().phases.passed(), Some(&j.profile(&[-11, -11])));
}

fn until_round_checks(j: &mut Journal, cores: usize) {
    drive(j, 4000, |j| {
        j.run()
            .rounds
            .active()
            .is_some_and(|round| round.cores().len() == cores)
            && matches!(j.next(), Action::RunTrial(_))
    });
}

fn fail_alone(j: &mut Journal, id: u8) -> togi_journal::Seq {
    loop {
        let action = j.next();
        match action {
            Action::RunTrial(plan)
                if plan.activity == Activity::Deepening
                    && plan.condition == Condition::Alone
                    && plan.regime == Regime::R1
                    && plan.load == Load::Single(core(id)) =>
            {
                j.trial(&plan, Outcome::Failure);
                let attribution = j.next_decision();
                return j.decide(attribution);
            }
            _ => {
                j.step(&mut |_| Outcome::Pass);
            }
        }
    }
}

fn tuner_decision(action: &DecisionAction) -> &TunerDecision {
    match &action.payload {
        DecisionPayload::TunerDecision(decision) => decision,
        other => panic!("expected a tuner decision, got {other:?}"),
    }
}

#[test]
fn zero_movable_candidates_still_run_the_full_confirmation_cycle() {
    let mut j = Journal::new(&[0, 1], &[Start::solo(-10, -10), Start::solo(-12, -12)]);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    let phase1_end = j.run().phases.phase1_end().unwrap();
    let from = j.events.len();
    drive(&mut j, 4000, |j| j.run().phases.clean_cycles() == 1);
    assert_eq!(kinds(&j.events[from..]), ["cycle-start", "cycle-end"]);
    assert!(j.run().phases.concluded().unwrap() > phase1_end);
}

#[test]
fn a_blamed_round_failure_yields_every_unloaded_mover_then_backs_off_the_loaded_one() {
    let mut j = solo_cores(&[-10, -10, -10, -10], -11);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    until_round_checks(&mut j, 4);
    let failure = fail_alone(&mut j, 0);
    let end = j.next_decision();
    let DecisionPayload::DeepeningRound(round) = &end.payload else {
        panic!("expected the round end, got {end:?}");
    };
    assert!(!round.passed);
    assert_eq!(end.cause, [failure]);
    j.decide(end);
    let mut yielded = Vec::new();
    for _ in 0..3 {
        let action = j.next_decision();
        let decision = tuner_decision(&action);
        assert_eq!(decision.activity, Activity::Deepening);
        assert_eq!(decision.decision, Decision::Yield);
        assert_eq!(decision.to_offset.get(), -10);
        assert_eq!(action.cause, [failure]);
        yielded.push(decision.core.get());
        j.decide(action);
    }
    yielded.sort_unstable();
    assert_eq!(yielded, [1, 2, 3]);
    let action = j.next_decision();
    let back = tuner_decision(&action);
    assert_eq!(back.activity, Activity::Deepening);
    assert_eq!(back.decision, Decision::Backoff);
    assert_eq!(back.core, core(0));
    assert_eq!(back.to_offset.get(), -10);
    assert_eq!(back.failure_point, Some(off(-11)));
    assert_eq!(action.cause, [failure]);
    j.decide(action);
    assert_eq!(j.offsets(), j.profile(&[-10, -10, -10, -10]));
    assert!(j.run().failures.queue_is_empty());
    assert_eq!(j.run().phases.finished(), set(&[1, 2, 3]));
    let from = j.events.len();
    drive(&mut j, 4000, |j| j.run().phases.confirm_start().is_some());
    assert!(!kinds(&j.events[from..]).contains(&"round-start"));
    assert!(
        !j.events[from..]
            .iter()
            .any(|event| matches!(event.payload, Payload::HuntStart(_)))
    );
    j.assert_resumes();
}

fn phase1_done(starts: &[Start]) -> Journal {
    let ids: Vec<u8> = (0..starts.len())
        .map(|id| u8::try_from(id).unwrap())
        .collect();
    let mut j = Journal::new(&ids, starts);
    let offsets: Vec<i64> = starts.iter().map(|start| start.offset).collect();
    j.profile_change(&offsets);
    j.cycle_start(1, &[Regime::R1]);
    j.cycle_end(1, true, true);
    j
}

fn until_confirmation(j: &mut Journal) {
    drive(j, 4000, |j| {
        j.run().phases.confirm_start().is_some() && j.run().checking.open().is_some()
    });
}

fn crash(j: &mut Journal, profile: &[i64]) -> togi_journal::Seq {
    let profile = j.profile(profile).offsets();
    j.idle_failure(profile, Condition::Together)
}

fn named_crash(j: &mut Journal, id: u8, at: i64, profile: &[i64]) -> togi_journal::Seq {
    let profile = j.profile(profile).offsets();
    j.failure(
        Failure {
            signal: Signal::Crash,
            attribution: Attribution::Attributed,
            core: Some(core(id)),
            offset: Some(off(at)),
            trial: None,
            regime: Some(Regime::R6),
            condition: Some(Condition::Together),
            profile: Some(profile),
            known_failure: None,
            reason: None,
            round: None,
        },
        &[],
    )
}

fn expect_revert(j: &mut Journal, failure: togi_journal::Seq, id: u8, to: i64, point: i64) {
    let action = j.next_decision();
    let decision = tuner_decision(&action);
    assert_eq!(decision.activity, Activity::Deepening);
    assert_eq!(decision.decision, Decision::Backoff);
    assert_eq!(decision.core, core(id));
    assert_eq!(decision.to_offset.get(), to);
    assert_eq!(decision.failure_point, Some(off(point)));
    assert_eq!(action.cause, [failure]);
    j.decide(action);
}

fn round_starts(events: &[Event]) -> Vec<&togi_journal::DeepeningRound> {
    events
        .iter()
        .filter_map(|event| match &event.payload {
            Payload::DeepeningRound(round) if round.event == RoundEvent::Start => Some(round),
            _ => None,
        })
        .collect()
}

fn finish_moves(j: &mut Journal) {
    while let Some(action) = j.run().round_move_action() {
        j.decide(action);
    }
}

fn until_round(j: &mut Journal) {
    drive(j, 4000, |j| j.run().rounds.active().is_some());
}

fn until_r7_check(j: &mut Journal) -> TrialPlan {
    for _ in 0..400 {
        match j.next() {
            Action::RunTrial(plan)
                if plan.regime == Regime::R7 && plan.activity == Activity::Deepening =>
            {
                return plan;
            }
            _ => {
                j.step(&mut |_| Outcome::Pass);
            }
        }
    }
    panic!("the round never ran an R7 check");
}

fn fail_r7_and_blame(j: &mut Journal) -> (Vec<CoreId>, togi_journal::Seq, Vec<TunerDecision>) {
    let plan = until_r7_check(j);
    let loaded: Vec<CoreId> = plan.load.cores().iter().collect();
    j.trial(&plan, Outcome::Failure);
    let attribution = j.next_decision();
    let failure = j.decide(attribution);
    let end = j.next_decision();
    assert!(
        matches!(end.payload, DecisionPayload::DeepeningRound(_)),
        "{end:?}"
    );
    j.decide(end);
    let mut decided = Vec::new();
    for _ in 0..8 {
        let action = j.next_decision();
        let decision = tuner_decision(&action).clone();
        assert_eq!(decision.activity, Activity::Deepening);
        j.decide(action);
        let done = decision.decision == Decision::Backoff;
        decided.push(decision);
        if done {
            break;
        }
    }
    (loaded, failure, decided)
}

#[test]
fn a_failed_r7_round_check_blames_the_least_preferred_loaded_core() {
    let mut j = solo_cores(&[-10, -10, -10, -10], -11);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    until_round(&mut j);
    finish_moves(&mut j);
    let round_cores = j.run().rounds.active().unwrap().cores();
    let (loaded, _, decided) = fail_r7_and_blame(&mut j);
    let blamed = decided.last().unwrap();
    assert!(loaded.contains(&blamed.core));
    assert_eq!(Some(&blamed.core), loaded.iter().max());
    let mut yielded: Vec<CoreId> = decided[..decided.len() - 1]
        .iter()
        .map(|d| d.core)
        .collect();
    yielded.sort_unstable();
    let others: Vec<CoreId> = round_cores.iter().filter(|&id| id != blamed.core).collect();
    assert_eq!(yielded, others);
}

#[test]
fn a_yielded_mover_is_finished_and_never_moves_again() {
    let mut j = solo_cores(&[-10, -10, -10, -10], -11);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    until_round(&mut j);
    finish_moves(&mut j);
    let (_, _, decided) = fail_r7_and_blame(&mut j);
    let yielded: Vec<CoreId> = decided
        .iter()
        .filter(|d| d.decision == Decision::Yield)
        .map(|d| d.core)
        .collect();
    assert!(!yielded.is_empty());
    let from = j.events.len();
    drive(&mut j, 4000, |j| j.run().phases.confirm_start().is_some());
    for round in round_starts(&j.events[from..]) {
        for &id in &yielded {
            assert!(!round.cores.contains(id));
            assert_eq!(
                round.profile.get(id).map(togi_machine::TargetOffset::get),
                Some(-10)
            );
        }
    }
    for &id in &yielded {
        assert_eq!(j.run().core(id).offset.get(), -10);
    }
}

fn end_round_by_hand(j: &mut Journal) {
    let start = j.events.iter().rposition(|event| {
        matches!(&event.payload, Payload::DeepeningRound(r) if r.event == RoundEvent::Start)
    });
    let start = j.events[start.unwrap()].seq;
    let round = j.run().rounds.active().unwrap().id();
    j.append(
        togi_journal::DeepeningRound {
            round,
            event: RoundEvent::End,
            base: PerCore::default(),
            base_seq: None,
            target: PerCore::default(),
            profile: PerCore::default(),
            cores: CoreSet::default(),
            ranking: Vec::new(),
            trials: None,
            trial_s: None,
            passed: false,
            reason: Some("a failure needs a hunt".to_owned()),
        }
        .into(),
        &[start],
    );
}

#[test]
fn a_round_ended_without_blame_keeps_its_movers_for_a_recheck_in_place() {
    let mut j = solo_cores(&[-10, -10, -10, -10], -11);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    until_round(&mut j);
    finish_moves(&mut j);
    let moved = j.offsets();
    end_round_by_hand(&mut j);
    until_round(&mut j);
    let second = *round_starts(&j.events).last().unwrap();
    assert_eq!(second.cores, set(&[0, 1, 2, 3]));
    assert_eq!(second.profile, moved.offsets());
    assert!(second.reason.as_deref().unwrap().contains("checked again"));
    assert!(j.run().phases.finished().is_empty());
    j.assert_resumes();
}

#[test]
fn a_round_failure_moves_nothing_into_a_recorded_failure_point() {
    let mut j = solo_cores(&[-10, -10], -11);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    until_round_checks(&mut j, 2);
    fail_alone(&mut j, 0);
    drive(&mut j, 4000, |j| j.run().phases.confirm_start().is_some());
    let state = j.run().core(core(0));
    assert_eq!(state.offset.get(), -10);
    for round in round_starts(&j.events) {
        let deeper = round.profile.get(core(0)).is_some_and(|o| o.get() < -10);
        assert!(!(round.round.get() > 1 && round.cores.contains(core(0)) && deeper));
    }
}

#[test]
fn confirmation_failure_reverts_deepened_cores_top_requester_first() {
    let mut j = solo_cores(&[-10, -10, -10, -10], -11);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    until_confirmation(&mut j);
    let first = crash(&mut j, &[-11, -11, -11, -11]);
    expect_revert(&mut j, first, 3, -10, -11);
    let second = crash(&mut j, &[-11, -11, -11, -10]);
    expect_revert(&mut j, second, 2, -10, -11);
    let third = crash(&mut j, &[-11, -11, -10, -10]);
    expect_revert(&mut j, third, 1, -10, -11);
    let fourth = crash(&mut j, &[-11, -10, -10, -10]);
    expect_revert(&mut j, fourth, 0, -10, -11);
    let last = crash(&mut j, &[-10, -10, -10, -10]);
    let next = j.next();
    assert!(!matches!(
        &next,
        Action::Decide(DecisionAction { payload: DecisionPayload::TunerDecision(d), .. })
            if d.activity == Activity::Deepening
    ));
    assert_eq!(j.run().failures.queue().next(), Some(last));
    j.assert_resumes();
}

#[test]
fn confirmation_resumes_across_revert_and_rerun() {
    let mut j = solo_cores(&[-10, -10], -11);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    until_confirmation(&mut j);
    crash(&mut j, &[-11, -11]);
    let mut reverted = false;
    drive(&mut j, 4000, |j| {
        reverted |= matches!(
            j.events.last().map(|e| &e.payload),
            Some(Payload::TunerDecision(d)) if d.activity == Activity::Deepening
        );
        reverted && j.run().phases.clean_cycles() == 1
    });
    assert!(j.run().phases.concluded().is_some());
    assert!(j.run().reruns.is_empty());
}

#[test]
fn confirmation_failure_reverts_the_deepest_moved_core_first() {
    let mut j = phase1_done(&[Start::solo(-10, -11), Start::solo(-10, -12)]);
    until_confirmation(&mut j);
    assert_eq!(j.offsets(), j.profile(&[-11, -12]));
    let failure = crash(&mut j, &[-11, -12]);
    expect_revert(&mut j, failure, 1, -10, -12);
    assert_eq!(j.offsets(), j.profile(&[-11, -10]));
}

#[test]
fn an_idle_failure_after_the_moved_profile_is_applied_blames_the_deepened_cores() {
    let mut j = solo_cores(&[-10, -10, -10, -10], -11);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    until_round(&mut j);
    finish_moves(&mut j);
    let round = j.run().rounds.active().unwrap().id();
    let moved: Vec<i64> = vec![-11; 4];
    let failure = crash(&mut j, &moved);
    let action = j.next_decision();
    let DecisionPayload::DeepeningRound(end) = &action.payload else {
        panic!("expected the round end, got {action:?}");
    };
    assert_eq!(end.round, round);
    assert!(
        end.reason
            .as_deref()
            .unwrap()
            .contains("blame goes to the deepened cores")
    );
    assert_eq!(action.cause, [failure]);
    j.decide(action);
    for _ in 0..6 {
        let action = j.next_decision();
        let backoff = tuner_decision(&action).decision == Decision::Backoff;
        j.decide(action);
        if backoff {
            break;
        }
    }
    assert_eq!(j.offsets(), j.profile(&[-10, -10, -10, -10]));
}

#[test]
fn a_fresh_movers_round_baseline_is_its_current_offset() {
    let mut j = phase1_done(&[Start::solo(-10, -12), Start::solo(-10, -12)]);
    j.decision(TunerDecision {
        core: core(0),
        activity: Activity::Checking,
        decision: Decision::Backoff,
        from_offset: off(-10),
        to_offset: off(-7),
        pass: Some(off(-12)),
        failure_point: Some(off(-13)),
        workloads: Vec::new(),
        reason: "test".to_owned(),
    });
    until_round(&mut j);
    let round = j.run().rounds.active().unwrap();
    let again = fresh(&j.events);
    assert_eq!(again.run().unwrap().rounds.active(), Some(round));
}

#[test]
fn a_confirmation_failure_naming_a_phase1_core_reverts_only_the_deepened_core() {
    let mut j = phase1_done(&[Start::solo(-10, -11), Start::solo(-10, -10)]);
    until_confirmation(&mut j);
    assert_eq!(j.offsets(), j.profile(&[-11, -10]));
    let first = named_crash(&mut j, 1, -10, &[-11, -10]);
    assert_eq!(j.run().core(core(1)).pending, Some(first));
    expect_revert(&mut j, first, 0, -10, -11);
    let named = j.run().core(core(1));
    assert_eq!(named.pending, None);
    assert_eq!(named.offset.get(), -10);
    for _ in 0..40 {
        let action = j.next();
        if let Action::Decide(DecisionAction {
            payload: DecisionPayload::TunerDecision(d),
            ..
        }) = &action
        {
            assert!(!(d.core == core(1) && d.decision == Decision::Backoff));
        }
        if !matches!(
            action,
            Action::Decide(DecisionAction {
                payload: DecisionPayload::TunerDecision(_) | DecisionPayload::Failure(_),
                ..
            })
        ) {
            break;
        }
        j.step(&mut |_| Outcome::Pass);
    }
    let again = named_crash(&mut j, 1, -10, &[-10, -10]);
    let action = j.next_decision();
    let decision = tuner_decision(&action);
    assert_ne!(decision.activity, Activity::Deepening);
    assert_eq!(decision.decision, Decision::Backoff);
    assert_eq!(decision.core, core(1));
    assert_eq!(decision.to_offset.get(), -9);
    assert_eq!(action.cause, [again]);
}

#[test]
fn alone_checks_follow_the_topology_order_not_core_ids() {
    let ids = [0, 1, 2, 3];
    let starts = [Start::solo(-10, -11); 4];
    let mut j = Journal::on(&ids, crate::testkit::pairs(&ids), &starts);
    j.profile_change(&[-10; 4]);
    j.cycle_start(1, &[Regime::R1]);
    j.cycle_end(1, true, true);
    until_round_checks(&mut j, 4);
    let order: Vec<CoreId> = j.run().topo.order().to_vec();
    let alone: Vec<CoreId> = j
        .run()
        .rounds
        .active()
        .unwrap()
        .checks()
        .iter()
        .filter(|check| check.condition == Condition::Alone)
        .map(|check| check.class.loaded().iter().next().unwrap())
        .collect();
    let mut unique = alone.clone();
    unique.dedup();
    assert_eq!(unique, order);
    assert_ne!(order, [core(0), core(1), core(2), core(3)]);
}

#[test]
fn a_failure_in_a_round_is_blamed_before_the_ranking_is_read() {
    let mut j = solo_cores(&[-10, -10, -10, -10], -11);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    until_round_checks(&mut j, 4);
    assert!(j.run().ranking_stale());
    fail_alone(&mut j, 0);
    for _ in 0..5 {
        assert!(j.run().ranking_stale());
        let action = j.next();
        assert!(matches!(action, Action::Decide(_)), "{action:?}");
        j.step(&mut |_| Outcome::Pass);
    }
    j.assert_resumes();
}

#[test]
fn a_cancelled_round_check_retry_does_not_run_after_its_round_ends() {
    let mut j = solo_cores(&[-10, -10], -11);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    until_round_checks(&mut j, 2);
    let first = j.next_trial();
    j.trial(&first, Outcome::Inconclusive);
    let retry = j.next_trial();
    assert!(retry.retry);
    assert_eq!(retry.requirement, first.requirement);
    assert!(j.run().retry.is_some());
    end_round_by_hand(&mut j);
    assert!(j.run().rounds.active().is_none());
    assert!(j.run().retry.is_none());
}

#[test]
fn a_round_retry_is_served_only_for_the_first_unmet_check() {
    let mut j = solo_cores(&[-10, -10], -11);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    until_round_checks(&mut j, 2);
    let first = j.next_trial();
    j.trial(&first, Outcome::Inconclusive);
    let retry = j.next_trial();
    assert!(retry.retry);
    j.trial(&retry, Outcome::Pass);
    let second = j.next_trial();
    assert!(!second.retry);
}

#[test]
fn a_failure_after_the_confirmation_cycle_closed_does_not_revert_deepened_cores() {
    let mut j = solo_cores(&[-10, -10], -11);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    until_confirmation(&mut j);
    let number = j.run().checking.open().unwrap().number.get();
    j.cycle_end(number, false, true);
    crash(&mut j, &[-11, -11]);
    let action = j.state.decide();
    assert!(
        !matches!(
            &action,
            Some(Action::Decide(DecisionAction { payload: DecisionPayload::TunerDecision(d), .. }))
                if d.activity == Activity::Deepening
        ),
        "{action:?}"
    );
}

#[test]
fn every_prefix_of_a_round_blame_and_confirmation_resumes_identically() {
    let ids = [0, 1, 2, 3];
    let starts = [Start::solo(-10, -11); 4];
    let mut j = Journal::on(&ids, crate::testkit::pairs(&ids), &starts).resuming_every(1);
    j.profile_change(&[-10; 4]);
    j.cycle_start(1, &[Regime::R1]);
    j.cycle_end(1, true, true);
    until_round_checks(&mut j, 4);
    fail_alone(&mut j, 2);
    drive(&mut j, 4000, |j| j.run().phases.confirm_start().is_some());
    drive(&mut j, 4000, |j| j.run().phases.concluded().is_some());
}

#[test]
fn a_reset_closes_the_open_round_before_the_core_searches() {
    let mut j = solo_cores(&[-10, -10], -11);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    until_round_checks(&mut j, 2);
    let reset = j.reset(0);
    let end = j.next_decision();
    assert!(
        matches!(end.payload, DecisionPayload::DeepeningRound(_)),
        "{end:?}"
    );
    assert_eq!(end.cause, [reset]);
    j.decide(end);
    assert!(j.run().rounds.active().is_none());
    let search = j.next_decision();
    let DecisionPayload::CorePhase(phase) = &search.payload else {
        panic!("expected the core to search, got {search:?}");
    };
    assert_eq!(phase.to, CorePhase::Search);
    j.decide(search);
    assert_eq!(j.run().phases.phase1_end(), None);
}
