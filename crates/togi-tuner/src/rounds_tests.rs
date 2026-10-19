use togi_journal::{
    Activity, Attribution, CorePhase, Decision, Event, Failure, Outcome, Payload, RoundEvent,
    TunerDecision,
};
use togi_machine::{Condition, CoreId, CoreSet, PerCore, Regime, Signal};

use crate::testkit::{Journal, R7Report, Start, set};
use crate::tests::{core, off};
use crate::{Action, DecisionAction, DecisionPayload, Load, TrialContext, TrialPlan};

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
            Payload::SessionStart(_)
            | Payload::SessionContext(_)
            | Payload::SessionBaseline(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::SessionArchived(_)
            | Payload::SessionCarried(_)
            | Payload::ConfigLoaded(_)
            | Payload::PreflightCheck(_)
            | Payload::SmuIntent(_)
            | Payload::SmuWrite(_)
            | Payload::SmuReadback(_)
            | Payload::SmuError(_)
            | Payload::ProfileApplied(_)
            | Payload::ProfileChange(_)
            | Payload::ProfileRestored(_)
            | Payload::TrialIntent(_)
            | Payload::TrialStart(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::TrialEnd(_)
            | Payload::TrialCarried(_)
            | Payload::Failure(_)
            | Payload::FailureCarried(_)
            | Payload::Mce(_)
            | Payload::CrashDetected(_)
            | Payload::TunerDecision(_)
            | Payload::CorePhase(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntStart(_)
            | Payload::HuntGroup(_)
            | Payload::HuntEnd(_)
            | Payload::HuntSkipped(_)
            | Payload::Combination(_)
            | Payload::TunerWarning(_)
            | Payload::BackendRetry(_)
            | Payload::CommandReset(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::DeadEnd(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::Shutdown(_)
            | Payload::JournalTorn(_)
            | Payload::StateRebuilt(_) => None,
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
            Action::RunTrial(_) | Action::Decide(_) | Action::ReadRanking => {
                j.step(&mut |_| Outcome::Pass);
            }
        }
    }
}

fn tuner_decision(action: &DecisionAction) -> &TunerDecision {
    let DecisionPayload::TunerDecision(decision) = &action.payload else {
        panic!("expected a tuner decision, got {:?}", action.payload);
    };
    decision
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
fn zero_movable_candidates_never_conclude_at_phase1_end() {
    let mut j = Journal::new(&[0], &[Start::solo(-10, -10)]);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    assert_eq!(j.run().phases.clean_cycles(), 0);
    assert_eq!(j.run().phases.concluded(), None);
    assert!(!j.run().round_due());
    let next = j.next_decision();
    let DecisionPayload::CheckingCycle(cycle) = &next.payload else {
        panic!("expected the confirmation cycle to start, got {next:?}");
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
fn phase2_round_1_moves_every_candidate_one_count_from_the_phase1_cycle() {
    let mut j = solo_cores(&[-10, -10, -10, -10], -11);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    let phase1_end = j.run().phases.phase1_end().unwrap();
    drive(&mut j, 4000, |j| j.run().rounds.active().is_some());
    let start = round_starts(&j.events)[0];
    assert_eq!(start.round.get(), 1);
    assert_eq!(start.cores, set(&[0, 1, 2, 3]));
    assert_eq!(start.profile, j.profile(&[-11, -11, -11, -11]).offsets());
    assert_eq!(start.target, j.profile(&[-11, -11, -11, -11]).offsets());
    assert_eq!(start.base_seq, Some(phase1_end));
    j.assert_resumes();
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
    let from = j.events.len();
    until_round(&mut j);
    let next = *round_starts(&j.events).last().unwrap();
    assert_eq!(next.cores, set(&[1, 2, 3]));
    assert_eq!(next.profile, j.profile(&[-10, -11, -11, -11]).offsets());
    finish_moves(&mut j);
    let plan = j.run().phase_plan();
    assert_eq!(plan.rounds_left, 1);
    assert!(
        plan.candidates
            .iter()
            .all(|candidate| candidate.moving && !candidate.carried)
    );
    let next_trial = first_trial_after(&mut j);
    assert_eq!(next_trial.regime, Regime::R1);
    assert_eq!(
        next_trial.workload_at(0),
        togi_machine::pick_workload(Regime::R1, 0).id()
    );
    assert_eq!(next_trial.condition, Condition::Alone);
    assert_eq!(next_trial.load, Load::Single(core(1)));
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
        .filter_map(|event| {
            if let Payload::DeepeningRound(round) = &event.payload
                && round.event == RoundEvent::Start
            {
                Some(round)
            } else {
                None
            }
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
            Action::RunTrial(_) | Action::Decide(_) | Action::ReadRanking => {
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
fn yielded_movers_retry_without_repeating_their_valid_own_checks() {
    let mut j = phase1_done(&[Start::solo(-10, -11); 4]).resuming_every(1);
    until_round(&mut j);
    finish_moves(&mut j);
    let first = until_r7_check(&mut j);
    let needed = j.run().config.needed.get();
    for _ in 0..needed {
        j.trial(&first, Outcome::Pass);
    }
    let first_class = first.requirement.class;
    assert_eq!(first_class.loaded(), set(&[0, 1, 2, 3]));
    assert_eq!(
        first_class.workload(),
        togi_machine::pick_workload(Regime::R7, 0).id()
    );
    let origins: Vec<_> = (0..4)
        .map(|id| j.run().phases.mark(core(id)).unwrap())
        .collect();
    let failed_class = until_r7_check(&mut j).requirement.class;
    assert_eq!(failed_class.loaded(), first_class.loaded());
    assert_eq!(
        failed_class.workload(),
        togi_machine::pick_workload(Regime::R7, 1).id()
    );
    let (_, _, decided) = fail_r7_and_blame(&mut j);
    let blamed = decided.last().unwrap().core;
    let yielded: Vec<CoreId> = decided
        .iter()
        .filter(|d| d.decision == Decision::Yield)
        .map(|d| d.core)
        .collect();
    assert!(!yielded.is_empty());
    until_round(&mut j);
    let round = j.run().rounds.active().unwrap();
    let retry_round = round.id();
    let retry_profile = j.profile(&[-11, -11, -11, -10]);
    assert_eq!(blamed, core(3));
    assert_eq!(round.profile(), &retry_profile);
    assert!(!round.cores().contains(blamed));
    for &id in &yielded {
        assert!(round.cores().contains(id));
        let mark = j.run().phases.mark(id).unwrap();
        let origin = origins[usize::from(id.get())];
        assert_eq!(
            (mark.round, mark.seq, mark.offset),
            (origin.round, origin.seq, origin.offset)
        );
        assert_eq!(mark.attempt, round.id());
    }
    finish_moves(&mut j);
    // The blamed core now tops the full CCD, exposing a new own-partial for the
    // other three cores. Full-CCD passes cannot answer that different class.
    let partial = set(&[0, 1, 2]);
    let retry_classes: Vec<_> = togi_machine::workloads(Regime::R7)
        .iter()
        .flat_map(|workload| {
            [first_class.loaded(), partial]
                .map(|loaded| crate::ClassKey::new(workload.id(), loaded, first_class.duration()))
        })
        .filter(|&class| class != first_class)
        .collect();
    assert_eq!(
        retry_classes[0],
        crate::ClassKey::new(first_class.workload(), partial, first_class.duration())
    );
    assert_eq!(retry_classes[1], failed_class);
    // No R1/R2 checks or already-passed full-CCD class repeat. The failed class
    // still owes all n passes: its baseline rerun does not cover these movers.
    for class in retry_classes {
        for _ in 0..needed {
            let next = first_trial_after(&mut j);
            assert_eq!(next.regime, Regime::R7);
            assert_eq!(next.activity, Activity::Deepening);
            assert_eq!(next.requirement.class, class);
            assert_eq!(next.profile, retry_profile);
            j.trial(&next, Outcome::Pass);
        }
    }
    let end = j.next_decision();
    assert!(matches!(
        &end.payload,
        DecisionPayload::DeepeningRound(round)
            if round.round == retry_round && round.event == RoundEvent::End && round.passed
    ));
    j.decide(end);
    for &id in &yielded {
        assert_eq!(j.run().core(id).offset.get(), -11);
        assert_eq!(j.run().phases.mark(id), None);
    }
    assert_eq!(j.run().failure_point(blamed), Some(off(-11)));
    assert_eq!(j.run().core(blamed).offset, off(-10));
    drive(&mut j, 4000, |j| j.run().phases.confirm_start().is_some());
    assert_eq!(round_starts(&j.events).len(), 2);
    j.assert_resumes();
}

#[test]
fn repeated_failed_rounds_exhaust_the_blamed_movers_not_the_innocent_ones() {
    let mut j = phase1_done(&[Start::solo(-10, -11); 4]).resuming_every(1);
    for id in 0_u8..4 {
        until_round_checks(&mut j, usize::from(4 - id));
        let round = j.run().rounds.active().unwrap().id();
        assert_eq!(round.get(), u32::from(id) + 1);
        fail_alone(&mut j, id);
        drive(&mut j, 4000, |j| {
            j.run()
                .rounds
                .active()
                .is_some_and(|next| next.id() > round)
                || j.run().phases.confirm_start().is_some()
        });
        assert_eq!(j.run().failure_point(core(id)), Some(off(-11)));
        assert_eq!(j.run().core(core(id)).offset, off(-10));
    }
    assert_eq!(round_starts(&j.events).len(), 4);
    assert!(j.run().phases.confirm_start().is_some());
    drive(&mut j, 4000, |j| j.run().phases.concluded().is_some());
    j.assert_resumes();
}

#[test]
fn a_retry_of_a_yielded_offset_still_respects_combinations() {
    let mut j = phase1_done(&[Start::solo(-10, -11); 3]).resuming_every(1);
    until_round_checks(&mut j, 3);
    let first = j.run().rounds.active().unwrap().id();
    fail_alone(&mut j, 0);
    drive(&mut j, 100, |j| j.run().phases.blame().is_none());
    j.combination(1, &[(1, -11), (2, -11)]);
    until_round(&mut j);
    let round = j.run().rounds.active().unwrap();
    assert!(round.id() > first);
    assert_eq!(round.cores(), set(&[1]));
    assert_eq!(round.profile(), &j.profile(&[-10, -11, -10]));
    drive(&mut j, 4000, |j| j.run().phases.concluded().is_some());
    assert_eq!(round_starts(&j.events).len(), 2);
    j.assert_resumes();
}

#[test]
fn a_different_offset_starts_new_checks_and_a_reset_drops_retry_origins() {
    let mut j = phase1_done(&[Start::solo(-10, -12); 2]).resuming_every(1);
    until_round_checks(&mut j, 2);
    fail_alone(&mut j, 0);
    drive(&mut j, 4000, |j| {
        j.run()
            .rounds
            .active()
            .is_some_and(|round| round.id().get() == 3)
    });
    let round = j.run().rounds.active().unwrap();
    let mark = j.run().phases.mark(core(1)).unwrap();
    assert_eq!(
        (mark.round, mark.attempt, mark.seq),
        (round.id(), round.id(), round.start())
    );
    assert_eq!(mark.offset, off(-12));
    finish_moves(&mut j);
    let trial = first_trial_after(&mut j);
    assert_eq!(trial.condition, Condition::Alone);
    assert_eq!(trial.regime, Regime::R1);
    assert_eq!(
        trial.workload_at(0),
        togi_machine::pick_workload(Regime::R1, 2).id()
    );
    j.reset(1);
    drive(&mut j, 100, |j| j.run().phases.phase1_end().is_none());
    assert_eq!(j.run().phases.mark(core(1)), None);
    assert_eq!(j.run().core(core(1)).phase.solo_limit(), None);
    assert!(j.run().rounds.active().is_none());
    j.assert_resumes();
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
    let plan = j.run().phase_plan();
    assert!(
        plan.candidates
            .iter()
            .all(|candidate| candidate.carried && !candidate.moving)
    );
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
    let mut j = solo_cores(&[-10, -10], -11).resuming_every(1);
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
    j.round_start(1, &[-8, -11], &[0, 1], 5);
    let shape = j.run().rounds.active().unwrap().shape();
    assert_eq!(shape.baseline(), &j.profile(&[-7, -10]));
    assert!(shape.deepened(core(0)));
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
    assert!(alone.contains(&core(0)));
    let failure = named_crash(&mut j, 0, -8, &[-8, -11]);
    let action = j.next_decision();
    assert!(
        matches!(action.payload, DecisionPayload::DeepeningRound(_)),
        "{action:?}"
    );
    assert_eq!(action.cause, [failure]);
    j.decide(action);
    loop {
        let action = j.next_decision();
        if tuner_decision(&action).decision == Decision::Backoff {
            let back = tuner_decision(&action);
            assert_eq!((back.core, back.to_offset), (core(0), off(-7)));
            assert_eq!(back.failure_point, Some(off(-8)));
            j.decide(action);
            break;
        }
        j.decide(action);
    }
    j.assert_resumes();
}

#[test]
fn a_malformed_round_base_is_a_fold_error_that_changes_nothing() {
    let mut j = phase1_done(&[Start::solo(-10, -12), Start::solo(-10, -12)]);
    let before = j.state.run().cloned();
    let result = j.try_append(
        togi_journal::DeepeningRound {
            round: std::num::NonZeroU32::MIN,
            event: RoundEvent::Start,
            base: PerCore::default(),
            base_seq: None,
            target: j.profile(&[-11, -11]).offsets(),
            profile: j.profile(&[-11, -11]).offsets(),
            cores: set(&[0, 1]),
            ranking: Vec::new(),
            trials: Some(5),
            trial_s: Some(120),
            passed: false,
            reason: None,
        }
        .into(),
        &[],
    );
    assert!(result.is_err());
    assert_eq!(j.state.run().cloned(), before);
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
            assert_ne!((d.core, d.decision), (core(1), Decision::Backoff));
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

fn round_check_retry() -> (Journal, TrialPlan) {
    let mut j = solo_cores(&[-10, -10], -11);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    until_round_checks(&mut j, 2);
    let first = j.next_trial();
    j.trial(&first, Outcome::Inconclusive);
    let retry = j.next_trial();
    assert!(retry.retry);
    assert_eq!(retry.requirement, first.requirement);
    assert_eq!(retry.requirement.context, first.requirement.context);
    assert!(j.run().retry.is_some());
    (j, retry)
}

fn first_trial_after(j: &mut Journal) -> TrialPlan {
    for _ in 0..10 {
        match j.next() {
            Action::RunTrial(plan) => return plan,
            Action::Decide(decision) => {
                j.decide(decision);
            }
            Action::ReadRanking => panic!("no ranking was asked for"),
        }
    }
    panic!("no trial after the transition");
}

#[test]
fn a_cancelled_round_check_retry_does_not_run_after_its_round_ends() {
    let (mut j, retry) = round_check_retry();
    let TrialContext::Round { round } = retry.requirement.context else {
        panic!("the retry is not a round check: {retry:?}");
    };
    j.reset(1);
    assert_eq!(
        j.state
            .next()
            .ok()
            .map(|a| matches!(a, Action::RunTrial(_))),
        Some(false)
    );
    let end = j.next_decision();
    assert!(
        matches!(&end.payload, DecisionPayload::DeepeningRound(r) if r.event == RoundEvent::End && r.round == round),
        "reset must cancel the round first: {end:?}"
    );
    j.decide(end);
    assert!(j.run().retry.is_none());
    let next = first_trial_after(&mut j);
    assert!(!next.retry, "{next:?}");
    assert_ne!(next.requirement.context, retry.requirement.context);
    assert!(
        matches!(next.requirement.context, TrialContext::Search { core: c, .. } if c == core(1))
    );
    assert_eq!(next.activity, Activity::Search);
    assert_eq!(next.load, Load::Single(core(1)));
    j.assert_resumes();

    let (mut j, _) = round_check_retry();
    end_round_by_hand(&mut j);
    assert!(j.run().rounds.active().is_none());
    assert!(j.run().retry.is_none());
    until_round_checks(&mut j, 2);
    let next = j.next_trial();
    let TrialContext::Round { round: second } = next.requirement.context else {
        panic!("the next round's first check is not a round check: {next:?}");
    };
    assert_ne!(second, round);
    assert!(!next.retry, "{next:?}");
    assert_eq!(next.activity, Activity::Deepening);
    j.assert_resumes();

    let (mut j, retry) = round_check_retry();
    end_round_by_hand(&mut j);
    j.reset(1);
    let next = first_trial_after(&mut j);
    assert!(!next.retry, "{next:?}");
    assert_ne!(next.requirement.context, retry.requirement.context);
    assert!(
        matches!(next.requirement.context, TrialContext::Search { core: c, .. } if c == core(1))
    );
    assert_eq!(next.activity, Activity::Search);
    assert_eq!(next.load, Load::Single(core(1)));
    j.assert_resumes();
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
    let action = j.state.next().ok();
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

#[test_case::test_case(false; "not deepening")]
#[test_case::test_case(true; "deepening")]
fn a_reset_closes_the_open_round_or_cycle_before_the_core_searches(deepening: bool) {
    let mut j = phase1_done(&[Start::solo(-10, -11)]);
    j.baseline(&[(0, -7)]);
    if deepening {
        j.round_start(2, &[-30], &[0], 5);
    } else {
        j.cycle_start(4, &[Regime::R1]);
    }
    let reset = j.reset(0);
    let end = j.next_decision();
    assert_eq!(end.cause, [reset], "deepening {deepening}");
    if let DecisionPayload::DeepeningRound(round) = &end.payload
        && deepening
    {
        assert_eq!(
            (round.round.get(), round.event, round.passed),
            (2, RoundEvent::End, false)
        );
    } else if let DecisionPayload::CheckingCycle(cycle) = &end.payload
        && !deepening
    {
        assert_eq!(cycle.cycle.get(), 4);
        assert_eq!(cycle.event, togi_journal::CycleEvent::End);
        assert!(!cycle.passed && !cycle.full);
    } else {
        panic!(
            "deepening {deepening}: expected the close, got {:?}",
            end.payload
        );
    }
    j.decide(end);
    let search = j.next_decision();
    let DecisionPayload::CorePhase(phase) = &search.payload else {
        panic!("expected the core to search, got {search:?}");
    };
    assert_eq!(phase.to, CorePhase::Search);
    assert_eq!(phase.offset, off(-7));
    assert_eq!((phase.pass, phase.failure_point), (None, None));
    j.decide(search);
    assert!(j.run().rounds.active().is_none());
    assert_eq!(j.run().phases.phase1_end(), None);
    j.assert_resumes();
}

#[test]
fn deepening_rechecks_its_own_partial_for_every_r7_workload() {
    let offsets = [-20; 8];
    let mut j = quad_session(&offsets);
    carry_r7_passes(&mut j, &offsets);
    let mut deeper = offsets;
    deeper[1] -= 1;
    start_round(&mut j, 2, &deeper, &[1], 2);
    let checks = j.run().rounds.active().unwrap().checks().to_vec();
    assert_eq!(checks.len(), 5);
    for (index, workload) in togi_machine::workloads(Regime::R7).iter().enumerate() {
        let check = &checks[index + 2];
        assert_eq!(check.class.workload(), workload.id());
        assert_eq!(check.class.loaded(), set(&[1, 2, 3]));
    }
    for _ in 0..4 {
        let Some(Action::RunTrial(plan)) = j.run().round_check_action() else {
            panic!("expected a solo check");
        };
        assert_eq!(plan.condition, Condition::Alone);
        assert_eq!(plan.load, Load::Single(core(1)));
        j.trial(&plan, Outcome::Pass);
    }
    for workload in togi_machine::workloads(Regime::R7) {
        for _ in 0..2 {
            let Some(Action::RunTrial(plan)) = j.run().round_check_action() else {
                panic!("expected an R7 own-partial check");
            };
            assert_eq!(plan.condition, Condition::Together);
            assert_eq!(plan.requirement.class.workload(), workload.id());
            assert_eq!(plan.load, Load::Many(set(&[1, 2, 3])));
            j.trial(&plan, Outcome::Pass);
        }
    }
    let Some(Action::Decide(end)) = j.run().round_check_action() else {
        panic!("expected the round end");
    };
    let DecisionPayload::DeepeningRound(end) = end.payload else {
        panic!("expected the round end");
    };
    assert!(end.passed);
    j.assert_resumes();
}

#[test]
fn a_request_rank_change_adds_the_ccd_whole_part() {
    let offsets = [-10, -20, -30, -40, -20, -20, -20, -20];
    let mut j = quad_session(&offsets);
    carry_r7_passes(&mut j, &offsets);
    let mut moved = offsets;
    moved[0] = -50;
    moved[1] = -21;
    start_round(&mut j, 1, &moved, &[0, 1], 2);
    let want: Vec<Vec<u8>> = [vec![0, 1, 2, 3], vec![0, 2, 3]]
        .into_iter()
        .cycle()
        .take(6)
        .collect();
    assert_eq!(r7_check_cores(&j), want);
    j.assert_resumes();
}

fn until_round_started(j: &mut Journal) -> togi_journal::DeepeningRound {
    drive(j, 4000, |j| j.run().rounds.active().is_some());
    round_starts(&j.events).last().copied().unwrap().clone()
}

#[test]
fn an_idle_failure_ends_deepening_before_moves() {
    let ids = [0, 1, 2, 3];
    let mut j = Journal::new(&ids, &[Start::solo(-10, -11); 4]);
    for (index, members) in [[0, 1], [1, 2], [2, 3], [3, 0], [1, 3]].iter().enumerate() {
        let members: Vec<(u8, i64)> = members.iter().map(|&id| (id, -50)).collect();
        j.combination(u32::try_from(index).unwrap() + 1, &members);
    }
    j.profile_change(&[-10; 4]);
    j.cycle_start(1, &[Regime::R1]);
    j.cycle_end(1, true, true);
    let round = until_round_started(&mut j);
    let failure = crash(&mut j, &[-10; 4]);
    let action = j.next_decision();
    let DecisionPayload::DeepeningRound(end) = &action.payload else {
        panic!("an idle failure did not end the round: {action:?}");
    };
    assert_eq!(end.round, round.round);
    assert_eq!(end.event, RoundEvent::End);
    let reason = end.reason.as_deref().unwrap();
    assert!(
        reason.contains("failure") && reason.contains("hunt"),
        "{reason}"
    );
    assert_eq!(action.cause, [failure]);
}

#[test]
fn a_live_deepening_failure_ends_the_round_before_the_backoff() {
    let mut j = solo_cores(&[-10, -10], -11);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    let round = until_round_started(&mut j);
    finish_moves(&mut j);
    let moved: Vec<i64> = vec![-11; 2];
    j.profile_change(&moved);
    let failure = fail_alone(&mut j, 0);
    let action = j.next_decision();
    let DecisionPayload::DeepeningRound(end) = &action.payload else {
        panic!("the live failure did not close the round: {action:?}");
    };
    assert_eq!(
        (end.round, end.event, end.passed),
        (round.round, RoundEvent::End, false)
    );
    assert_eq!(action.cause, [failure]);
    j.decide(action);
    let action = j.next_decision();
    let unloaded = tuner_decision(&action);
    assert_eq!(
        (unloaded.decision, unloaded.core),
        (Decision::Yield, core(1))
    );
    j.decide(action);
    let action = j.next_decision();
    let back = tuner_decision(&action);
    assert_eq!(back.activity, Activity::Deepening);
    assert_eq!(back.to_offset.get(), -10);
    assert_eq!(back.failure_point, Some(off(-11)));
}

fn unscheduled(j: &Journal, plan: &TrialPlan) {
    let trial = togi_journal::TrialId::from("9999".to_owned());
    let event = j.event(plan.intent(trial.clone(), 0).into(), &plan.cause);
    let mut probe = j.state.clone();
    assert!(
        matches!(
            probe.fold(&event),
            Err(crate::FoldError::UnscheduledIntent(found)) if found == trial
        ),
        "{plan:?}"
    );
    assert_eq!(probe.run(), j.state.run());
}

#[test]
fn a_round_intent_is_scheduled_only_for_the_open_rounds_own_checks() {
    let mut j = solo_cores(&[-40, -40, -40, -40], -41);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    until_round_checks(&mut j, 4);
    let round = round_starts(&j.events).last().copied().unwrap().clone();
    let plan = j.next_trial();
    assert_eq!(
        plan.requirement.context,
        crate::TrialContext::Round { round: round.round }
    );
    let (intent, trial) = j.intent(&plan);
    assert_eq!(
        j.requirement().needed,
        u32::from(plan.requirement.needed.get())
    );
    let other = std::num::NonZeroU32::new(round.round.get() + 1).unwrap();
    j.end(intent, &trial, Outcome::Pass);
    let next = j.next_trial();
    unscheduled(
        &j,
        &TrialPlan {
            requirement: crate::Requirement {
                context: crate::TrialContext::Round { round: other },
                ..next.requirement
            },
            ..next.clone()
        },
    );
    unscheduled(
        &j,
        &TrialPlan {
            activity: Activity::Checking,
            ..next.clone()
        },
    );
    j.assert_resumes();
}

#[test_case::test_case(|plan, trial, other| {
    let mut intent = plan.intent(trial, 0);
    intent.profile = plan.profile.with(other, off(plan.profile.get(other).unwrap().get() - 1)).offsets();
    intent
}; "another core profile")]
#[test_case::test_case(|plan, trial, _| {
    let mut intent = plan.intent(trial, 0);
    intent.offset = intent.offset.map(|at| off(at.get() - 1));
    intent
}; "target offset")]
fn a_round_intent_must_carry_the_checks_profile_and_target_offset(
    make_intent: fn(&TrialPlan, togi_journal::TrialId, CoreId) -> togi_journal::TrialIntent,
) {
    let mut j = solo_cores(&[-40, -40, -40, -40], -41);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    until_round_checks(&mut j, 4);
    let plan = j.next_trial();
    assert_eq!(plan.condition, Condition::Alone);
    let trial = togi_journal::TrialId::from("9999".to_owned());
    let own = plan.load.cores().first().unwrap();
    let other = plan
        .profile
        .cores()
        .iter()
        .find(|&core| core != own)
        .unwrap();
    let intent = make_intent(&plan, trial, other);
    let trial = intent.trial.clone();
    let event = j.event(intent.into(), &plan.cause);
    let mut probe = j.state.clone();
    assert!(matches!(
        probe.fold(&event),
        Err(crate::FoldError::UnscheduledIntent(found)) if found == trial
    ));
    assert_eq!(probe.run(), j.state.run());

    let (_, _) = j.intent(&plan);
    j.assert_resumes();
}

#[test]
fn carried_passes_satisfy_every_round_check_and_are_cited() {
    let mut j = Journal::new(&[0], &[Start::has_room(-10)]);
    j.profile_change(&[-10]);
    let n = usize::from(j.run().config.needed.get());
    let short = j.run().config.short_trial.get();
    let mut facts = Vec::new();
    for (regime, condition) in [
        (Regime::R1, Condition::Alone),
        (Regime::R2, Condition::Alone),
        (Regime::R7, Condition::Together),
    ] {
        for _ in 0..n {
            facts.push(j.carry(
                "20261002T004254Z",
                (regime, 0, &[0], short),
                condition,
                &[-11],
                Outcome::Pass,
            ));
        }
    }
    for workload in 1..togi_machine::workloads(Regime::R7).len() {
        for _ in 0..n {
            facts.push(j.carry(
                "20261002T004254Z",
                (Regime::R7, workload, &[0], short),
                Condition::Together,
                &[-11],
                Outcome::Pass,
            ));
        }
    }
    let boundary = start_round(&mut j, 1, &[-11], &[0], u32::try_from(n).unwrap());
    let Some(Action::Decide(end)) = j.run().round_check_action() else {
        panic!("carried passes scheduled a live trial");
    };
    let DecisionPayload::DeepeningRound(round) = &end.payload else {
        panic!("expected the round end, got {end:?}");
    };
    assert!(round.passed);
    let mut want = vec![boundary];
    want.extend(facts);
    assert_eq!(end.cause, want);
    let reason = round.reason.as_deref().unwrap();
    assert!(reason.contains("20261002T004254Z"), "{reason}");
}

#[test]
fn a_skipped_deepening_check_closes_the_round_before_its_attributed_backoff() {
    let mut j = Journal::new(&[0, 1], &[Start::has_room(-19), Start::at_limit(-20)]);
    let change = j.next_decision();
    j.decide(change);
    let short = j.run().config.short_trial.get();
    let class = togi_journal::TrialClass {
        regime: Regime::R1,
        workload: togi_machine::WorkloadId::new(Regime::R1, 0),
        cores: set(&[0]),
        duration_s: short,
    };
    let mut carried = j.carried_trial(
        "20230101T000000Z",
        class,
        Condition::Alone,
        &[-20, 0],
        Outcome::Failure,
        None,
    );
    carried.core = Some(core(0));
    let failure = j.append(carried.into(), &[]);
    start_round(&mut j, 1, &[-20, -20], &[0], 5);
    j.decision(TunerDecision {
        core: core(0),
        activity: Activity::Deepening,
        decision: Decision::Deepen,
        from_offset: off(-19),
        to_offset: off(-20),
        pass: None,
        failure_point: None,
        workloads: Vec::new(),
        reason: "test".to_owned(),
    });
    j.profile_change(&[-20, -20]);
    let skip = j.next_decision();
    let DecisionPayload::Failure(known) = &skip.payload else {
        panic!("a known failure scheduled a live check: {skip:?}");
    };
    assert_eq!(known.known_failure, Some(failure));
    j.decide(skip);
    let action = j.next_decision();
    let DecisionPayload::DeepeningRound(end) = &action.payload else {
        panic!("the skipped check did not close the round: {action:?}");
    };
    assert_eq!(end.event, RoundEvent::End);
    assert!(!end.passed);
    assert!(action.cause.contains(&failure));
    j.decide(action);
    let action = j.next_decision();
    let back = tuner_decision(&action);
    assert_eq!(back.decision, Decision::Backoff);
    assert_eq!(back.activity, Activity::Deepening);
    assert_eq!(back.to_offset.get(), -19);
    assert!(action.cause.contains(&failure));
    j.decide(action);
    j.assert_resumes();
}

#[test]
fn a_yield_answers_no_failure_pending_on_the_yielding_core() {
    let mut j = solo_cores(&[-10, -10, -10, -10], -11);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    until_round_checks(&mut j, 4);
    let first = fail_alone(&mut j, 0);
    let end = j.next_decision();
    j.decide(end);
    let second = named_crash(&mut j, 2, -11, &[-11, -11, -11, -11]);
    assert_eq!(j.run().core(core(2)).pending, Some(second));
    loop {
        let action = j.next_decision();
        let decision = tuner_decision(&action);
        assert_eq!(action.cause, [first]);
        let yielded = (decision.decision, decision.core) == (Decision::Yield, core(2));
        let done = decision.decision == Decision::Backoff;
        j.decide(action);
        if yielded {
            assert_eq!(j.run().core(core(2)).pending, Some(second));
            assert!(j.run().failures.pending(second).is_some());
            break;
        }
        assert!(!done);
    }
    j.assert_resumes();
}

#[test]
fn a_saved_round_retry_is_served_to_its_own_check_alone() {
    let mut j = solo_cores(&[-10, -10], -11);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    until_round_checks(&mut j, 2);
    let first = j.next_trial();
    j.trial(&first, Outcome::Inconclusive);
    let round = j.run().rounds.active().unwrap();
    let other = round
        .checks()
        .iter()
        .find(|check| check.class != first.requirement.class)
        .unwrap()
        .class;
    let elsewhere = crate::Requirement {
        class: other,
        ..first.requirement
    };
    assert_eq!(j.run().serve_round_retry(&elsewhere, Vec::new()), None);
    let served = j
        .run()
        .serve_round_retry(&first.requirement, Vec::new())
        .unwrap();
    assert!(served.retry);
    assert_eq!(served.requirement, first.requirement);
}

#[test]
fn phase2_blames_and_backoffs_never_count_toward_escalation() {
    let mut j = solo_cores(&[-10, -10, -10, -10], -11);
    drive(&mut j, 4000, |j| j.run().phases.phase1_end().is_some());
    until_round(&mut j);
    finish_moves(&mut j);
    let (_, _, decided) = fail_r7_and_blame(&mut j);
    assert_eq!(decided.last().unwrap().decision, Decision::Backoff);
    assert_eq!(
        j.run().r7.strikes,
        crate::r7::escalation::Strikes::default()
    );
    j.assert_resumes();
}

#[test]
fn round_check_progress_visits_distinct_classes_not_pass_volume() {
    let visits = |passes: usize| {
        let mut j = Journal::new(&[0, 1], &[Start::has_room(-10), Start::has_room(-10)]);
        j.profile_change(&[-10, -10]);
        let short = j.run().config.short_trial.get();
        for _ in 0..passes {
            j.carry(
                "20261002T004254Z",
                (Regime::R1, 0, &[0], short),
                Condition::Alone,
                &[-11, 0],
                Outcome::Pass,
            );
        }
        j.round_start(1, &[-11, -11], &[0, 1], 5);
        j.run().evidence.take_visits();
        let _ = j.run().round_check_action();
        let first = j.run().evidence.take_visits();
        let _ = j.state.next().ok();
        let decided = j.run().evidence.take_visits();
        (first, decided)
    };
    let small = visits(100);
    assert!(small.0 > 0);
    assert_eq!(visits(10_000), small);
}

#[test]
fn a_round_beyond_the_catalog_length_wraps_its_workload_index() {
    let lengths = [Regime::R1, Regime::R2].map(|regime| togi_machine::workloads(regime).len());
    let round = u32::try_from(lengths[0] * lengths[1] + 1).unwrap();
    let mut j = Journal::new(&[0], &[Start::has_room(-10)]);
    j.profile_change(&[-10]);
    j.round_start(round, &[-11], &[0], 5);
    let alone: Vec<_> = j
        .run()
        .rounds
        .active()
        .unwrap()
        .checks()
        .iter()
        .filter(|check| check.condition == Condition::Alone)
        .map(|check| check.class.workload())
        .collect();
    let first = [Regime::R1, Regime::R2].map(|regime| togi_machine::workloads(regime)[0].id());
    assert_eq!(alone, first);
}

fn quad_session(offsets: &[i64]) -> Journal {
    let ids: Vec<u8> = (0..offsets.len())
        .map(|id| u8::try_from(id).unwrap())
        .collect();
    let starts: Vec<Start> = offsets
        .iter()
        .map(|&offset| Start::has_room(offset))
        .collect();
    let mut j = Journal::on(&ids, crate::testkit::quads(&ids), &starts);
    j.profile_change(offsets);
    j
}

fn carry_r7_passes(j: &mut Journal, offsets: &[i64]) {
    let report = R7Report::default().requests(&[(0, 1.2), (1, 1.1), (2, 1.0), (3, 0.9)]);
    for workload in 0..togi_machine::workloads(Regime::R7).len() {
        j.r7_fact_of(workload, true, &[0, 1, 2, 3], offsets, &report);
    }
}

fn start_round(
    j: &mut Journal,
    round: u32,
    profile: &[i64],
    cores: &[u8],
    trials: u32,
) -> togi_journal::Seq {
    j.round_start(round, profile, cores, trials)
}

fn r7_check_cores(j: &Journal) -> Vec<Vec<u8>> {
    j.run()
        .rounds
        .active()
        .unwrap()
        .checks()
        .iter()
        .filter(|check| check.class.regime() == Regime::R7)
        .map(|check| check.class.loaded().iter().map(CoreId::get).collect())
        .collect()
}

#[test]
fn an_r7_backoff_closes_the_open_round_before_any_offset_moves() {
    let mut j = phase1_done(&[Start::solo(-10, -11), Start::solo(-10, -11)]);
    j.ranking(&[0, 1]);
    let start = start_round(&mut j, 1, &[-11, -11], &[0, 1], 5);
    let failure = j.r7_fact(
        false,
        &[0, 1],
        &[-10, -10],
        &R7Report::default().named(0).top(&[0]),
    );
    let response = j.run().r7_decision().unwrap();
    assert!(matches!(
        &response,
        crate::r7::respond::R7Response::Backoff { .. }
    ));
    let before = j.offsets();
    let action = j.run().round_before_backoff(response.clone());
    let Action::Decide(close) = action else {
        panic!("expected the round to close before the backoff");
    };
    let DecisionPayload::DeepeningRound(end) = &close.payload else {
        panic!("expected a round end, got {close:?}");
    };
    assert_eq!(end.event, RoundEvent::End);
    assert_eq!(end.round.get(), 1);
    assert!(!end.passed);
    assert!(end.reason.as_deref().unwrap().contains("requires backoff"));
    assert_eq!(close.cause, [failure]);
    assert_eq!(j.run().rounds.active().unwrap().start(), start);
    j.decide(close);
    assert!(j.run().rounds.active().is_none());
    assert_eq!(j.offsets(), before);
    assert_eq!(
        j.run().round_before_backoff(response.clone()),
        response.into_action()
    );
    j.assert_resumes();
}

#[test]
fn a_round_reaching_a_new_combination_ends_before_its_first_move() {
    let mut j = phase1_done(&[Start::solo(-10, -11), Start::solo(-10, -11)]);
    let start = start_round(&mut j, 1, &[-11, -11], &[0, 1], 5);
    j.combination(1, &[(0, -11), (1, -11)]);
    let before = j.offsets();
    let close = j.run().round_move_action().unwrap();
    let DecisionPayload::DeepeningRound(end) = &close.payload else {
        panic!("expected the unsafe round to close, got {close:?}");
    };
    assert_eq!(end.event, RoundEvent::End);
    assert!(!end.passed);
    assert!(end.reason.as_deref().unwrap().contains("reaches"));
    assert_eq!(close.cause, [start]);
    assert_eq!(j.next_decision(), close);
    j.decide(close);
    assert!(j.run().rounds.active().is_none());
    assert_eq!(j.offsets(), before);
    j.assert_resumes();
}

#[test]
fn a_known_round_failure_uses_the_ordinary_backoff_without_fresh_blame() {
    let mut j = phase1_done(&[Start::solo(-10, -11)]);
    let short = j.run().config.short_trial.get();
    let mut carried = j.carried_trial(
        "20230101T000000Z",
        togi_journal::TrialClass {
            regime: Regime::R1,
            workload: togi_machine::WorkloadId::new(Regime::R1, 0),
            cores: set(&[0]),
            duration_s: short,
        },
        Condition::Alone,
        &[-11],
        Outcome::Failure,
        None,
    );
    carried.core = Some(core(0));
    let source = j.append(carried.into(), &[]);
    start_round(&mut j, 1, &[-11], &[0], 5);
    finish_moves(&mut j);
    j.profile_change(&[-11]);
    let mut skip = j.next_decision();
    if matches!(skip.payload, DecisionPayload::CorePhase(_)) {
        j.decide(skip);
        skip = j.next_decision();
    }
    let DecisionPayload::Failure(failure) = &skip.payload else {
        panic!("expected the known failure, got {skip:?}");
    };
    assert_eq!(failure.known_failure, Some(source));
    j.decide(skip);
    assert!(j.run().phases.blame().is_none());
    let close = j.next_decision();
    assert!(matches!(
        &close.payload,
        DecisionPayload::DeepeningRound(end) if !end.passed && end.event == RoundEvent::End
    ));
    j.decide(close);
    let backoff = j.next_decision();
    let backoff = tuner_decision(&backoff);
    assert_eq!(backoff.decision, Decision::Backoff);
    assert_eq!(backoff.activity, Activity::Deepening);
    assert_eq!(backoff.to_offset, off(-10));
    j.assert_resumes();
}
