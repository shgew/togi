use togi_journal::{CorePhase, Decision, Outcome, TunerDecision};
use togi_machine::Regime;

use crate::schedule_tests::transition;
use crate::testkit::{Journal, Start};
use crate::tests::{core, off};
use crate::{Action, DecisionAction, DecisionPayload};

#[test]
fn search_exit_starts_one_count_shallower_than_the_solo_limit() {
    let cases: [(&str, i64, Option<i64>, i64, &str); 4] = [
        (
            "interior",
            -20,
            Some(-21),
            -19,
            "one count shallower than the solo limit -20 (margin)",
        ),
        ("floor", -50, None, -49, "checking starts at -49"),
        ("zero", 0, None, 0, "checking starts at 0"),
        (
            "failure point two counts deeper",
            -20,
            Some(-22),
            -19,
            "margin",
        ),
    ];
    for (name, solo, fail, start, reason) in cases {
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
                .solo_limit
                .map(togi_machine::TargetOffset::get),
            Some(solo)
        );
    }
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
    assert_eq!(state.solo_limit, Some(off(-20)));
    j.core_phase(crate::schedule_tests::transition(0, CorePhase::Search, 0));
    assert_eq!(j.run().core(core(0)).solo_limit, None);
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
    j.assert_resumes();
    j.reset(0);
    let reset = j.next_decision();
    j.decide(reset);
    assert_eq!(j.run().phases.clean_cycles(), 0);
    assert_eq!(j.run().phases.phase1_end(), None);
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
    assert_eq!(error, Err(crate::FoldError::NoCheckingProfile));
    assert_eq!(j.state.decide(), None);
}

#[test]
fn phase2_moves() {
    struct Case {
        name: &'static str,
        starts: Vec<Start>,
        combination: Option<Vec<(u8, i64)>>,
        moved: Vec<u8>,
        want: Vec<i64>,
    }
    let from_search = |offset: i64, pass: i64, fail: i64| Start {
        from: Some(CorePhase::Search),
        ..Start::has_room(offset).pass(pass).fail(fail)
    };
    let cases = vec![
        Case {
            name: "one count each toward the solo limit",
            starts: vec![Start::solo(-10, -13), Start::solo(-8, -9)],
            combination: None,
            moved: vec![0, 1],
            want: vec![-11, -9],
        },
        Case {
            name: "a core at its solo limit stays",
            starts: vec![Start::solo(-10, -10), Start::solo(-8, -9)],
            combination: None,
            moved: vec![1],
            want: vec![-10, -9],
        },
        Case {
            name: "a failure point at the next count blocks",
            starts: vec![from_search(-10, -12, -11), Start::solo(-8, -9)],
            combination: None,
            moved: vec![1],
            want: vec![-10, -9],
        },
        Case {
            name: "a combination blocks only the second member",
            starts: vec![Start::solo(-10, -12), Start::solo(-10, -12)],
            combination: Some(vec![(0, -11), (1, -11)]),
            moved: vec![0],
            want: vec![-11, -10],
        },
        Case {
            name: "a core with no recorded solo limit is no candidate",
            starts: vec![Start::has_room(-10).pass(-12)],
            combination: None,
            moved: vec![],
            want: vec![-10],
        },
        Case {
            name: "a multi-count gap moves one count toward its failure point plus one",
            starts: vec![from_search(-5, -12, -13)],
            combination: None,
            moved: vec![0],
            want: vec![-6],
        },
        Case {
            name: "solo limit zero is no candidate",
            starts: vec![Start::solo(0, 0)],
            combination: None,
            moved: vec![],
            want: vec![0],
        },
    ];
    for case in cases {
        let ids: Vec<u8> = (0..case.starts.len())
            .map(|id| u8::try_from(id).unwrap())
            .collect();
        let mut j = Journal::new(&ids, &case.starts);
        if let Some(members) = &case.combination {
            j.combination(1, members);
        }
        let run = j.run();
        let moves = run.phase2_moves(&run.offsets(), run.phases.passed());
        assert_eq!(moves.profile, j.profile(&case.want), "{}", case.name);
        let moved: Vec<_> = case.moved.iter().map(|&id| core(id)).collect();
        assert_eq!(moves.moved, moved, "{}", case.name);
        assert!(moves.carried.is_empty(), "{}", case.name);
    }
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
