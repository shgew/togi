use proptest::prelude::*;
use togi_journal::{CorePhase, Outcome};

use crate::testkit::{Journal, Start};
use crate::{Action, DecisionAction, DecisionPayload, Load, TrialPlan};

/// The proptest configuration for this crate: `cases` cases drawn from a fixed seed, so every run
/// checks the same cases, and no failure file written into the source tree.
pub(crate) fn seeded(cases: u32) -> ProptestConfig {
    ProptestConfig {
        cases,
        rng_seed: proptest::test_runner::RngSeed::Fixed(0x7475_6e65_725f_7072),
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

#[test]
fn seeded_config_draws_the_same_cases_every_run() {
    use proptest::strategy::{Strategy, ValueTree};
    use proptest::test_runner::TestRunner;

    let draw = || {
        let mut runner = TestRunner::new(seeded(8));
        (0..8)
            .map(|_| any::<u64>().new_tree(&mut runner).unwrap().current())
            .collect::<Vec<_>>()
    };
    assert_eq!(draw(), draw());
    assert_eq!(seeded(48).cases, 48);
    assert!(seeded(48).failure_persistence.is_none());
}

fn left_search(action: &Action) -> bool {
    matches!(action, Action::Decide(DecisionAction {
        payload: DecisionPayload::CorePhase(phase), ..
    }) if phase.to != CorePhase::Search)
}

proptest! {
    #![proptest_config(seeded(256))]

    #[test]
    fn search_converges(threshold in -49_i64..=0, start in -50_i64..=0) {
        let mut j = Journal::search_at(&[start]);
        let seen = j.run_until(
            1000,
            |plan: &TrialPlan| {
                let Load::Single(core) = plan.load else { unreachable!() };
                if plan.profile.get(core).unwrap().get() >= threshold { Outcome::Pass } else { Outcome::Failure }
            },
            left_search,
        );
        let Some(Action::Decide(DecisionAction { payload: DecisionPayload::CorePhase(exit), .. })) = seen.last() else {
            return Err(TestCaseError::fail("the search did not find the solo limit"));
        };
        prop_assert_eq!(exit.pass.map(togi_machine::TargetOffset::get), Some(threshold));
        prop_assert_eq!(exit.offset.get(), (threshold + 1).min(0));
    }
}

fn outcome(byte: u8) -> Outcome {
    match byte {
        0..=199 => Outcome::Pass,
        200..=239 => Outcome::Failure,
        _ => Outcome::Inconclusive,
    }
}

proptest! {
    #![proptest_config(seeded(48))]

    #[test]
    fn a_fresh_fold_of_every_prefix_equals_the_incremental_fold(
        script in proptest::collection::vec((any::<u8>(), any::<u8>()), 1..80),
    ) {
        let mut j = Journal::new(
            &[0, 1, 2],
            &[Start::search(-10), Start::search(-30), Start::search(-5)],
        )
        .resuming_every(1);
        for (op, byte) in script {
            if op % 11 == 0 {
                j.reset(op / 11 % 3);
                continue;
            }
            let before = j.state.clone();
            let first = j.state.next().ok();
            prop_assert_eq!(&first, &j.state.next().ok());
            prop_assert_eq!(&before, &j.state);
            let mut oracle = |_: &TrialPlan| outcome(byte);
            let action = j.step(&mut oracle);
            if matches!(action, Action::Decide(DecisionAction { payload: DecisionPayload::DeadEnd(_), .. })) {
                break;
            }
        }
        j.assert_resumes();
    }
}

fn visits_per_event(passes: usize) -> (u64, u64) {
    let mut j = Journal::new(&[0], &[Start::search(-10).checking()]);
    let plan = j.next_trial();
    for _ in 0..passes {
        j.trial(&plan, Outcome::Pass);
    }
    j.state.visits();
    j.trial(&plan, Outcome::Pass);
    let folding = j.state.visits();
    let _ = j.state.next().ok();
    (folding, j.state.visits())
}

#[test]
fn fold_and_decide_visit_the_distinct_evidence_not_the_passes() {
    let small = visits_per_event(100);
    let large = visits_per_event(10_000);
    assert!(small.0 > 0, "the fold of a trial end consults the ledger");
    assert_eq!(small, large);
}
