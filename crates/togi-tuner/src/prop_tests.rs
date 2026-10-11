use proptest::prelude::*;
use togi_journal::{CorePhase, Outcome};
use togi_machine::Condition;

use crate::testkit::{Journal, Start};
use crate::{Action, DecisionAction, DecisionPayload, Load, TrialPlan};

fn left_search(action: &Action) -> bool {
    matches!(action, Action::Decide(DecisionAction {
        payload: DecisionPayload::CorePhase(phase), ..
    }) if phase.to != CorePhase::Search)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn search_converges(threshold in -49_i64..=0, start in -50_i64..=0) {
        let mut j = Journal::search_at(&[start]);
        let seen = j.run_until(
            1000,
            |plan: &TrialPlan| {
                let Load::Single(core) = plan.load else { unreachable!() };
                if plan.profile.get(core).get() >= threshold { Outcome::Pass } else { Outcome::Failure }
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
    #![proptest_config(ProptestConfig::with_cases(48))]

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
            let first = j.state.decide();
            prop_assert_eq!(&first, &j.state.decide());
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
    let mut j = Journal::has_room(&[-10, -10]);
    let plan = j.manual(
        togi_machine::Regime::R1,
        0,
        Load::Single(j.core_id(0)),
        Condition::Together,
        &[-10, -10],
        120,
    );
    for _ in 0..passes {
        j.trial(&plan, Outcome::Pass);
    }
    j.state.visits();
    j.trial(&plan, Outcome::Pass);
    let folding = j.state.visits();
    let _ = j.state.decide();
    (folding, j.state.visits())
}

#[test]
fn fold_and_decide_visit_the_distinct_evidence_not_the_passes() {
    let small = visits_per_event(100);
    let large = visits_per_event(10_000);
    assert!(small.0 > 0, "the fold of a trial end consults the ledger");
    assert_eq!(small, large);
}
