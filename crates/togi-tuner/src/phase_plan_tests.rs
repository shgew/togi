use togi_journal::{Activity, CorePhase, Decision, Seq, TunerDecision};
use togi_machine::Regime;

use crate::phase_plan::{Candidate, PhaseNo, PhasePlan, ROUND_CAP};
use crate::schedule_tests::transition;
use crate::testkit::{Journal, Start, fresh};
use crate::tests::{core, off};
use crate::{Action, DecisionPayload, State};

fn solo_core(offset: i64, solo: i64) -> Start {
    Start::solo(offset, solo)
}

fn journal(starts: &[Start]) -> Journal {
    let ids: Vec<u8> = (0..starts.len())
        .map(|id| u8::try_from(id).unwrap())
        .collect();
    Journal::new(&ids, starts)
}

fn end_phase1(j: &mut Journal, profile: &[i64]) -> Seq {
    j.profile_change(profile);
    j.cycle_start(1, &[Regime::R1]);
    j.cycle_end(1, true, true)
}

fn drive(j: &mut Journal, done: impl Fn(&Journal) -> bool) {
    for _ in 0..4000 {
        if done(j) {
            return;
        }
        j.step(&mut |_| togi_journal::Outcome::Pass);
    }
    panic!("never reached the awaited state");
}

fn clean_cycle(offsets: &[i64], solo: i64) -> Journal {
    let starts: Vec<Start> = offsets.iter().map(|&o| solo_core(o, solo)).collect();
    let mut j = journal(&starts);
    drive(&mut j, |j| j.run().phases.phase1_end().is_some());
    j
}

fn next_round(j: &mut Journal) -> u32 {
    drive(j, |j| j.run().rounds.active().is_some());
    j.run().rounds.active().unwrap().id().get()
}

fn apply_round(j: &mut Journal) {
    loop {
        let action = j.next_decision();
        match &action.payload {
            DecisionPayload::TunerDecision(decision) if decision.decision == Decision::Deepen => {
                j.decide(action);
            }
            _ => return,
        }
    }
}

fn plan(phase: PhaseNo, phase1_end: Option<Seq>) -> PhasePlan {
    PhasePlan {
        phase,
        phase1_end,
        concluded: None,
        confirming: false,
        rounds_left: 0,
        round: None,
        candidates: Vec::new(),
    }
}

fn candidate(id: u8, offset: i64, solo: i64, gap: u32) -> Candidate {
    Candidate {
        core: core(id),
        offset: off(offset),
        solo_limit: Some(off(solo)),
        gap,
        carried: false,
        moving: false,
    }
}

fn assert_plan(j: &Journal, want: PhasePlan) {
    assert_eq!(j.state.phase_plan(), Some(want));
    assert_eq!(fresh(&j.events).phase_plan(), j.state.phase_plan());
}

#[test]
fn phase_plan_phase1_has_no_candidates() {
    let j = journal(&[solo_core(-10, -13)]);
    assert_plan(&j, plan(PhaseNo::One, None));
}

#[test]
fn phase_plan_a_passed_cycle_that_is_not_full_does_not_end_phase_1() {
    let mut j = journal(&[solo_core(-10, -13)]);
    j.profile_change(&[-10]);
    j.cycle_start(1, &[Regime::R1]);
    j.cycle_end(1, true, false);
    assert_plan(&j, plan(PhaseNo::One, None));
}

#[test]
fn phase_plan_no_core_can_move_leaves_the_confirmation_cycle() {
    let mut j = journal(&[solo_core(-10, -10), solo_core(-12, -12)]);
    let end = end_phase1(&mut j, &[-10, -12]);
    assert_plan(
        &j,
        PhasePlan {
            confirming: true,
            ..plan(PhaseNo::Two, Some(end))
        },
    );
}

#[test]
fn phase_plan_rounds_left_are_the_largest_gap_not_the_sum() {
    let mut j = journal(&[solo_core(-10, -13), solo_core(-8, -9)]);
    let end = end_phase1(&mut j, &[-10, -8]);
    assert_plan(
        &j,
        PhasePlan {
            rounds_left: 3,
            candidates: vec![candidate(0, -10, -13, 3), candidate(1, -8, -9, 1)],
            ..plan(PhaseNo::Two, Some(end))
        },
    );
}

#[test]
fn phase_plan_a_failure_point_excludes_the_counts_it_blocks() {
    let mut j = journal(&[Start {
        from: Some(CorePhase::Search),
        ..Start::has_room(-10).pass(-13).fail(-12)
    }]);
    let end = end_phase1(&mut j, &[-10]);
    assert_plan(
        &j,
        PhasePlan {
            rounds_left: 1,
            candidates: vec![candidate(0, -10, -13, 1)],
            ..plan(PhaseNo::Two, Some(end))
        },
    );
}

#[test]
fn phase_plan_an_open_round_counts_as_one_and_marks_its_movers() {
    let mut j = clean_cycle(&[-10, -10], -11);
    let round = next_round(&mut j);
    let end = j.run().phases.phase1_end();
    let moving = |id| Candidate {
        moving: true,
        ..candidate(id, -10, -11, 1)
    };
    let want = PhasePlan {
        round: Some(round),
        rounds_left: 1,
        candidates: vec![moving(0), moving(1)],
        ..plan(PhaseNo::Two, end)
    };
    assert_plan(&j, want.clone());
    apply_round(&mut j);
    assert_plan(&j, want);
}

#[test]
fn phase_plan_a_round_that_ended_unpassed_without_blame_carries_its_cores() {
    let mut j = clean_cycle(&[-10, -10], -11);
    next_round(&mut j);
    apply_round(&mut j);
    let start = j.run().rounds.active().unwrap().start();
    j.round_end(1, start);
    let end = j.run().phases.phase1_end();
    let carried = |id| Candidate {
        carried: true,
        ..candidate(id, -11, -11, 0)
    };
    assert_plan(
        &j,
        PhasePlan {
            rounds_left: 1,
            candidates: vec![carried(0), carried(1)],
            ..plan(PhaseNo::Two, end)
        },
    );
}

#[test]
fn phase_plan_a_round_rechecking_carried_cores_in_place_does_not_move_them() {
    let mut j = clean_cycle(&[-10, -10], -11);
    next_round(&mut j);
    apply_round(&mut j);
    let start = j.run().rounds.active().unwrap().start();
    j.round_end(1, start);
    let second = next_round(&mut j);
    assert_eq!(second, 2);
    let end = j.run().phases.phase1_end();
    let carried = |id| Candidate {
        carried: true,
        ..candidate(id, -11, -11, 0)
    };
    assert_plan(
        &j,
        PhasePlan {
            round: Some(second),
            rounds_left: 1,
            candidates: vec![carried(0), carried(1)],
            ..plan(PhaseNo::Two, end)
        },
    );
}

#[test]
fn phase_plan_a_fresh_mover_shows_the_rounds_baseline() {
    let mut j = journal(&[solo_core(-10, -12), solo_core(-10, -12)]);
    end_phase1(&mut j, &[-10, -10]);
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
    j.round_start(1, &[-8, -11], &[0, 1], 1);
    let plan = j.state.phase_plan().unwrap();
    let moving: Vec<_> = plan
        .candidates
        .iter()
        .filter(|candidate| candidate.moving)
        .map(|candidate| (candidate.core, candidate.offset))
        .collect();
    assert_eq!(moving, [(core(0), off(-7)), (core(1), off(-10))]);
    assert_eq!(fresh(&j.events).phase_plan(), Some(plan));
}

#[test]
fn phase_plan_the_confirmation_cycle_leaves_no_rounds() {
    let mut j = journal(&[solo_core(-10, -13)]);
    let end = end_phase1(&mut j, &[-10]);
    j.cycle_start(2, &[Regime::R1]);
    assert_plan(
        &j,
        PhasePlan {
            confirming: true,
            ..plan(PhaseNo::Two, Some(end))
        },
    );
}

#[test]
fn phase_plan_after_conclusion_there_is_no_phase() {
    let mut j = journal(&[solo_core(-10, -13)]);
    let end = end_phase1(&mut j, &[-10]);
    j.cycle_start(2, &[Regime::R1]);
    let conclusion = j.cycle_end(2, true, true);
    assert_plan(
        &j,
        PhasePlan {
            concluded: Some(conclusion),
            ..plan(PhaseNo::Concluded, Some(end))
        },
    );
}

#[test]
fn phase_plan_a_reset_returns_to_phase_1() {
    let mut j = journal(&[solo_core(-10, -13)]);
    end_phase1(&mut j, &[-10]);
    j.core_phase(togi_journal::CoreTransition {
        from: Some(CorePhase::HasRoom),
        ..transition(0, CorePhase::Search, 0)
    });
    assert_plan(&j, plan(PhaseNo::One, None));
}

#[test]
fn phase_plan_no_cores_no_plan() {
    assert_eq!(State::new().phase_plan(), None);
}

#[test]
fn phase_plan_is_none_once_the_state_is_poisoned() {
    let mut j = journal(&[solo_core(-10, -13)]);
    let stale = j.events[0].clone();
    assert!(j.state.fold(&stale).is_err());
    assert_eq!(j.state.phase_plan(), None);
    assert_eq!(j.state.profile(), None);
    assert_eq!(j.state.profile_seq(), None);
    assert_eq!(j.state.clean_cycles(), 0);
    assert!(!j.state.first_result());
    assert_eq!(j.state.bios_profile(), None);
}

fn deepest() -> Journal {
    let mut j = journal(&[solo_core(0, -50), solo_core(0, -50)]);
    end_phase1(&mut j, &[0, 0]);
    j
}

#[test]
fn phase_plan_bounded_52() {
    assert_eq!(ROUND_CAP, 52);
    let short = deepest();
    let mut long = deepest();
    for _ in 0..3000 {
        long.ranking(&[0, 1]);
    }
    let measured = short.run().outlook();
    assert_eq!(measured.iterations, long.run().outlook().iterations);
    assert_eq!(measured.iterations, 51);
    assert_eq!(measured.rounds, 50);
    assert_eq!(short.state.phase_plan(), long.state.phase_plan());
    assert_eq!(short.state.phase_plan().unwrap().rounds_left, 50);

    let mut open = deepest();
    open.round_start(1, &[-1, -1], &[0, 1], 1);
    let outlook = open.run().outlook();
    assert!(
        i64::from(outlook.iterations) <= ROUND_CAP,
        "{}",
        outlook.iterations
    );
    assert_eq!(outlook.rounds, 50);

    assert_eq!(long.state.visits(), 0);
    long.state.phase_plan();
    assert_eq!(long.state.visits(), 0);
}

#[test]
fn phase_plan_matches_the_round_the_tuner_would_run_next() {
    let mut j = clean_cycle(&[-10, -10], -12);
    let planned = j.state.phase_plan().unwrap();
    assert_eq!(planned.rounds_left, 2);
    let mut rounds = 0;
    for _ in 0..4000 {
        if j.run().phases.confirm_start().is_some() {
            break;
        }
        if let Action::Decide(decision) = j.next() {
            if matches!(&decision.payload, DecisionPayload::DeepeningRound(round)
                if round.event == togi_journal::RoundEvent::Start)
            {
                rounds += 1;
            }
        }
        j.step(&mut |_| togi_journal::Outcome::Pass);
    }
    assert_eq!(rounds, planned.rounds_left);
}

#[test]
fn phase_plan_views_follow_folded_profiles_and_confirmation() {
    let empty = State::new();
    assert_eq!(empty.profile(), None);
    assert_eq!(empty.profile_seq(), None);
    assert_eq!(empty.bios_profile(), None);
    assert_eq!(empty.clean_cycles(), 0);
    assert!(!empty.first_result());

    let mut j = journal(&[solo_core(-10, -13)]);
    assert_eq!(j.state.profile(), None);
    assert_eq!(j.state.profile_seq(), None);
    assert!(!j.state.first_result());
    assert_eq!(j.state.bios_profile().unwrap().confirmed, None);
    let changed = j.profile_change(&[-10]);
    assert_eq!(j.state.profile(), Some(j.profile(&[-10])));
    assert_eq!(j.state.profile_seq(), Some(changed));
    j.cycle_start(1, &[Regime::R1]);
    let first = j.cycle_end(1, true, true);
    assert!(j.state.first_result());
    assert_eq!(j.state.clean_cycles(), 0);
    let bios = j.state.bios_profile().unwrap();
    assert_eq!(bios.shown, j.profile(&[-10]));
    assert_eq!(bios.confirmed.unwrap().seq, first);
    j.cycle_start(2, &[Regime::R1]);
    j.cycle_end(2, true, true);
    assert_eq!(j.state.clean_cycles(), 1);
    j.cycle_start(3, &[Regime::R1]);
    j.cycle_end(3, true, true);
    assert_eq!(j.state.clean_cycles(), 2);
}

fn roundtrip<T>(value: &T)
where
    T: serde::Serialize + serde::de::DeserializeOwned + std::fmt::Debug + PartialEq,
{
    let json = serde_json::to_string(value).unwrap();
    assert_eq!(&serde_json::from_str::<T>(&json).unwrap(), value);
}

#[test]
fn phase_plan_query_outputs_roundtrip_through_serde() {
    let mut j = journal(&[solo_core(-10, -13), solo_core(-8, -9)]);
    end_phase1(&mut j, &[-10, -8]);
    roundtrip(&j.state.phase_plan().unwrap());
    roundtrip(&j.state.bios_profile().unwrap());
    roundtrip(&j.state.profile().unwrap());
}
