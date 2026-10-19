use togi_journal::{CorePhase, Decision, Outcome, TunerDecision};
use togi_machine::Regime;

use crate::schedule_tests::transition;
use crate::testkit::{Journal, Start};
use crate::tests::{core, off};
use crate::{Action, DecisionAction, DecisionPayload};

#[test_case::test_case("interior", -20, Some(-21), -19, "one count shallower than the solo limit -20 (margin)"; "interior")]
#[test_case::test_case("floor", -50, None, -49, "checking starts at -49"; "floor")]
#[test_case::test_case("zero", 0, None, 0, "checking starts at 0"; "zero")]
#[test_case::test_case("failure point two counts deeper", -20, Some(-22), -19, "margin"; "failure point two counts deeper")]
fn search_exit_starts_one_count_shallower_than_the_solo_limit(
    name: &str,
    solo: i64,
    fail: Option<i64>,
    start: i64,
    reason: &str,
) {
    let mut begin = Start::search(solo).checking();
    begin.fail = fail;
    let mut j = Journal::new(&[0], &[begin]);
    let seen = j.run_until(
        200,
        |_| Outcome::Pass,
        |action| {
            matches!(action, Action::Decide(DecisionAction {
                    payload: DecisionPayload::CorePhase(phase), ..
                }) if phase.to != CorePhase::Search)
        },
    );
    let Some(Action::Decide(DecisionAction {
        payload: DecisionPayload::CorePhase(exit),
        ..
    })) = seen.last()
    else {
        panic!("{name}: the core never left search");
    };
    assert_eq!(exit.offset.get(), start, "{name}");
    assert_eq!(
        exit.pass.map(togi_machine::TargetOffset::get),
        Some(solo),
        "{name}"
    );
    assert_eq!(exit.to, CorePhase::HasRoom, "{name}");
    assert!(exit.reason.contains(reason), "{name}: {}", exit.reason);
    assert_eq!(
        j.run()
            .core(core(0))
            .phase
            .solo_limit()
            .map(togi_machine::TargetOffset::get),
        Some(solo)
    );
}

#[test]
fn solo_limit_survives_a_failure_discarding_pass() {
    let mut j = Journal::new(&[0], &[Start::solo(-19, -20)]);
    j.decision(TunerDecision {
        core: core(0),
        activity: togi_journal::Activity::Checking,
        decision: Decision::Backoff,
        from_offset: off(-19),
        to_offset: off(-18),
        pass: None,
        failure_point: Some(off(-19)),
        workloads: Vec::new(),
        reason: "test".to_owned(),
    });
    let state = j.run().core(core(0));
    assert_eq!(state.pass, None);
    assert_eq!(state.phase.solo_limit(), Some(off(-20)));
    j.core_phase(crate::schedule_tests::transition(0, CorePhase::Search, 0));
    assert_eq!(j.run().core(core(0)).phase.solo_limit(), None);
}

#[test]
fn phase1_never_deepens_and_ends_at_its_first_passed_full_cycle() {
    let mut j = Journal::new(&[0, 1], &[Start::solo(-10, -11), Start::solo(-10, -11)]);
    j.profile_change(&[-10, -10]);
    j.cycle_start(1, &[Regime::R1]);
    j.cycle_end(1, true, false);
    assert_eq!(j.run().phases.phase1_end(), None);
    assert!(!j.run().round_due());
    j.cycle_start(2, &[Regime::R1]);
    let ended = j.cycle_end(2, true, true);
    assert_eq!(j.run().phases.phase1_end(), Some(ended));
    assert_eq!(
        j.run().phases.first_confirmed(),
        Some(&j.profile(&[-10, -10]))
    );
    assert!(j.run().round_due());
    j.assert_resumes();
}

#[test]
fn no_deepening_round_is_proposed_during_phase_1() {
    let mut j = Journal::new(&[0, 1], &[Start::solo(-10, -11), Start::solo(-10, -11)]);
    j.profile_change(&[-10, -10]);
    j.cycle_start(1, &[Regime::R1]);
    j.cycle_end(1, true, false);
    j.cycle_start(2, &[Regime::R1]);
    let mut trials = 0;
    for _ in 0..3 {
        let action = j.step(&mut |_| Outcome::Pass);
        match &action {
            Action::Decide(decision) => assert!(
                !matches!(decision.payload, DecisionPayload::DeepeningRound(_)),
                "phase 1 deepened: {decision:?}"
            ),
            Action::RunTrial(_) => trials += 1,
            Action::ReadRanking => {}
        }
    }
    assert!(trials > 0);
    assert_eq!(j.run().phases.phase1_end(), None);
}

#[test]
fn phases_run_to_a_conclusion_and_count_clean_cycles() {
    let mut j = Journal::new(&[0], &[Start::solo(-10, -11)]);
    j.profile_change(&[-10]);
    j.cycle_start(1, &[Regime::R1]);
    j.cycle_end(1, true, true);
    assert_eq!(j.run().phases.clean_cycles(), 0);
    let start = j.cycle_start(2, &[Regime::R1]);
    assert_eq!(j.run().phases.confirm_start(), Some(start));
    assert!(!j.run().round_due());
    j.cycle_end(2, false, true);
    assert_eq!(j.run().phases.concluded(), None);
    j.cycle_start(3, &[Regime::R1]);
    let concluded = j.cycle_end(3, true, true);
    assert_eq!(j.run().phases.concluded(), Some(concluded));
    assert_eq!(j.run().phases.clean_cycles(), 1);
    j.cycle_start(4, &[Regime::R1]);
    j.cycle_end(4, true, false);
    assert_eq!(j.run().phases.clean_cycles(), 1);
    j.cycle_start(5, &[Regime::R1]);
    j.cycle_end(5, true, true);
    assert_eq!(j.run().phases.clean_cycles(), 2);
    j.profile_change(&[-9]);
    j.cycle_start(6, &[Regime::R1]);
    j.cycle_end(6, true, true);
    assert_eq!(j.run().phases.clean_cycles(), 3);
    j.assert_resumes();
    j.reset(0);
    let reset = j.next_decision();
    j.decide(reset);
    assert_eq!(j.run().phases.clean_cycles(), 0);
    assert_eq!(j.run().phases.phase1_end(), None);
    assert_eq!(j.run().phases.concluded(), None);
}

#[test]
fn a_cycle_cannot_pass_before_a_profile_change() {
    let mut j = Journal::new(&[0], &[Start::solo(-10, -11)]);
    j.cycle_start(1, &[Regime::R1]);
    let error = j.try_append(
        togi_journal::CheckingCycle {
            cycle: std::num::NonZeroU32::MIN,
            event: togi_journal::CycleEvent::End,
            passed: true,
            full: true,
            missing: Vec::new(),
            steps: Vec::new(),
            reason: None,
        }
        .into(),
        &[],
    );
    assert!(matches!(error, Err(crate::FoldError::NoCheckingProfile)));
    assert_eq!(j.state.next().ok(), None);
}

#[test_case::test_case("one count each toward the solo limit", &[Start::solo(-10, -13), Start::solo(-8, -9)], None, &[0, 1], &[-11, -9]; "one count each toward the solo limit")]
#[test_case::test_case("a core at its solo limit stays", &[Start::solo(-10, -10), Start::solo(-8, -9)], None, &[1], &[-10, -9]; "a core at its solo limit stays")]
#[test_case::test_case("a failure point at the next count blocks", &[Start {
        from: Some(CorePhase::Search),
        ..Start::has_room(-10).pass(-12).fail(-11)
    }, Start::solo(-8, -9)], None, &[1], &[-10, -9]; "a failure point at the next count blocks")]
#[test_case::test_case("a combination blocks only the second member", &[Start::solo(-10, -12), Start::solo(-10, -12)], Some(&[(0, -11), (1, -11)]), &[0], &[-11, -10]; "a combination blocks only the second member")]
#[test_case::test_case("a core with no recorded solo limit is no candidate", &[Start::has_room(-10).pass(-12)], None, &[], &[-10]; "a core with no recorded solo limit is no candidate")]
#[test_case::test_case("a multi-count gap moves one count toward its failure point plus one", &[Start {
        from: Some(CorePhase::Search),
        ..Start::has_room(-5).pass(-12).fail(-13)
    }], None, &[0], &[-6]; "a multi-count gap moves one count toward its failure point plus one")]
#[test_case::test_case("solo limit zero is no candidate", &[Start::solo(0, 0)], None, &[], &[0]; "solo limit zero is no candidate")]
fn phase2_moves(
    name: &'static str,
    starts: &[Start],
    combination: Option<&[(u8, i64)]>,
    moved: &[u8],
    want: &[i64],
) {
    let ids: Vec<u8> = (0..starts.len())
        .map(|id| u8::try_from(id).unwrap())
        .collect();
    let mut j = Journal::new(&ids, starts);
    if let Some(members) = combination {
        j.combination(1, members);
    }
    let run = j.run();
    let moves = run
        .phase2_moves(&run.offsets(), run.phases.passed())
        .unwrap();
    assert_eq!(moves.profile, j.profile(want), "{name}");
    let moved: Vec<_> = moved.iter().map(|&id| core(id)).collect();
    assert_eq!(moves.moved, moved, "{name}");
    assert!(moves.carried.is_empty(), "{name}");
}

#[test]
fn reset_clears_combination_and_reevaluates_other_cores() {
    let mut j = Journal::new(
        &[0, 1],
        &[Start::at_limit(-30).fail(-31), Start::at_limit(-30)],
    );
    j.baseline(&[(0, -10), (1, -10)]);
    j.combination(1, &[(0, -30), (1, -31)]);
    let reset = j.reset(0);
    let action = j.next_decision();
    assert_eq!(action.cause, [reset]);
    let DecisionPayload::CorePhase(phase) = &action.payload else {
        panic!("expected core.phase, got {action:?}");
    };
    assert_eq!(phase.to, CorePhase::Search);
    assert_eq!(phase.offset.get(), -10);
    assert_eq!(phase.cleared_combination.len(), 1);
    assert!(phase.reason.contains("C1"), "{}", phase.reason);
    j.decide(action);
    let other = j.next_decision();
    let DecisionPayload::CorePhase(other) = &other.payload else {
        panic!("expected core.phase");
    };
    assert_eq!(other.core, core(1));
    assert_eq!(other.from, Some(CorePhase::AtLimit));
    assert_eq!(other.to, CorePhase::HasRoom);
    let _ = transition;
}
