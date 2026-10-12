use proptest::prelude::*;
use togi_journal::{CorePhase, Decision, Event, Outcome};
use togi_machine::{Condition, Regime};

use crate::forecast::{AllPass, Forecast, Premise};
use crate::projection::Projection;
use crate::prop_tests::seeded;
use crate::testkit::{Journal, Start, fresh};
use crate::tests::{core, off};
use crate::{
    Action, DecisionAction, DecisionPayload, Load, NotReady, State, TrialContext, TrialPlan,
};

fn outcome(byte: u8) -> Outcome {
    match byte {
        0..=199 => Outcome::Pass,
        200..=239 => Outcome::Failure,
        _ => Outcome::Inconclusive,
    }
}

fn drive(journal: &mut Journal, script: &[(u8, u8)], ids: &[u8]) {
    for &(operation, byte) in script {
        if operation % 11 == 0 {
            journal.reset(ids[usize::from(operation / 11) % ids.len()]);
            continue;
        }
        let Ok(action) = journal.state.next() else {
            break;
        };
        match action {
            Action::Decide(decision) => {
                let stopped = matches!(decision.payload, DecisionPayload::DeadEnd(_));
                journal.decide(decision);
                if stopped {
                    break;
                }
            }
            Action::RunTrial(plan) => {
                journal.trial(&plan, outcome(byte));
            }
            Action::ReadRanking => {
                journal.ranking(ids);
            }
        }
    }
}

fn journals(start: i64, script: &[(u8, u8)]) -> Vec<Journal> {
    let ids = [0, 4, 9];
    let mut search = Journal::new(&ids, &[Start::search(start); 3]);
    drive(&mut search, script, &ids);

    let ids = [4, 9];
    let mut checking = Journal::new(&ids, &[Start::has_room(-20); 2]);
    let change = checking.next_decision();
    assert!(matches!(change.payload, DecisionPayload::ProfileChange(_)));
    checking.decide(change);
    checking.idle_failure(checking.offsets().offsets(), Condition::Together);
    assert_eq!(checking.state.next(), Ok(Action::ReadRanking));
    drive(&mut checking, script, &ids);
    vec![search, checking]
}

fn action_kind(answer: &Result<Action, NotReady>) -> u8 {
    match answer {
        Ok(Action::Decide(_)) => 1,
        Ok(Action::RunTrial(_)) => 2,
        Ok(Action::ReadRanking) => 4,
        Err(_) => 0,
    }
}

fn every_prefix(journal: &Journal, mut check: impl FnMut(&State, &[Event])) -> u8 {
    let mut state = State::new();
    let mut kinds = 0;
    for length in 0..=journal.events.len() {
        check(&state, &journal.events[..length]);
        kinds |= action_kind(&state.next());
        if let Some(event) = journal.events.get(length) {
            state.fold(event).unwrap();
        }
    }
    assert_eq!(state, journal.state);
    kinds
}

proptest! {
    #![proptest_config(seeded(48))]

    #[test]
    fn next_is_pure(
        start in -49_i64..=-5,
        script in proptest::collection::vec((any::<u8>(), any::<u8>()), 1..65),
    ) {
        let mut kinds = 0;
        for journal in journals(start, &script) {
            kinds |= every_prefix(&journal, |state, _| {
                let before = state.clone();
                let _ = state.next();
                let _ = state.next();
                assert_eq!(state, &before);
            });
        }
        prop_assert_eq!(kinds, 7);
    }

    #[test]
    fn repeated_next_same_answer(
        start in -49_i64..=-5,
        script in proptest::collection::vec((any::<u8>(), any::<u8>()), 1..65),
    ) {
        let mut kinds = 0;
        for journal in journals(start, &script) {
            kinds |= every_prefix(&journal, |state, _| {
                let first = state.next();
                let _ = state.drain();
                let _ = state.project();
                assert_eq!(state.next(), first);
                assert_eq!(state.next(), first);
            });
        }
        prop_assert_eq!(kinds, 7);
    }

    #[test]
    fn fresh_fold_equals_incremental_every_prefix(
        start in -49_i64..=-5,
        script in proptest::collection::vec((any::<u8>(), any::<u8>()), 1..65),
    ) {
        let mut kinds = 0;
        for journal in journals(start, &script) {
            kinds |= every_prefix(&journal, |incremental, prefix| {
                let resumed = fresh(prefix);
                assert_eq!(&resumed, incremental);
                assert_eq!(resumed.next(), incremental.next());
                assert_eq!(resumed.project(), incremental.project());
            });
        }
        prop_assert_eq!(kinds, 7);
    }
}

fn repeated_evidence(passes: usize) -> (Journal, TrialPlan) {
    let mut journal = Journal::new(&[0], &[Start::search(-10).checking()]);
    let plan = journal.next_trial();
    assert_eq!(plan.regime, Regime::R1);
    assert_eq!(plan.load, Load::Single(core(0)));
    assert_eq!(plan.requirement.needed.get(), 5);
    for _ in 0..passes {
        journal.trial(&plan, Outcome::Pass);
    }
    assert_eq!(
        journal
            .events
            .iter()
            .filter(|event| matches!(event.payload, togi_journal::Payload::TrialEnd(_)))
            .count(),
        passes,
    );
    journal.state.visits();
    (journal, plan)
}

fn assert_second_regime(plan: &TrialPlan) {
    assert_eq!(plan.regime, Regime::R2);
    assert_eq!(plan.load, Load::Single(core(0)));
    assert_eq!(plan.profile.get(core(0)), Some(off(-10)));
    assert_eq!(plan.condition, Condition::Alone);
    assert_eq!(
        plan.context,
        TrialContext::SoloLimit {
            core: core(0),
            regime: Regime::R2
        }
    );
    assert_eq!(plan.requirement.needed.get(), 5);
}

fn fold_cost(passes: usize) -> u64 {
    let (mut journal, plan) = repeated_evidence(passes);
    let before = journal.events.len();
    journal.trial(&plan, Outcome::Pass);
    let visits = journal.state.visits();
    assert_eq!(journal.events.len(), before + 2);
    let history = journal.state.trial_history().unwrap();
    assert_eq!((history.passed, history.trial, history.needed), (5, 5, 5));
    assert_second_regime(&journal.next_trial());
    visits
}

#[test]
fn fold_visits_bounded_by_distinct_evidence() {
    let small = fold_cost(100);
    let large = fold_cost(10_000);
    assert!(small > 0, "fold must consult pre-end evidence");
    assert_eq!(large, small);
    assert!(
        small <= 6,
        "one pass cell and at most five witnesses: {small}"
    );
}

fn next_cost(passes: usize) -> u64 {
    let (journal, _) = repeated_evidence(passes);
    let before = journal.state.clone();
    let length = journal.events.len();
    let answer = journal.state.next().unwrap();
    let visits = journal.state.visits();
    let Action::RunTrial(plan) = &answer else {
        panic!("expected unmet R2 candidate requirement, got {answer:?}");
    };
    assert_second_regime(plan);
    assert_eq!(journal.state.next(), Ok(answer));
    assert_eq!(journal.state, before);
    assert_eq!(journal.events.len(), length);
    visits
}

#[test]
fn next_visits_bounded() {
    let small = next_cost(100);
    let large = next_cost(10_000);
    assert!(small > 0, "next must consult candidate evidence");
    assert_eq!(large, small);
    assert!(
        small <= 6,
        "one pass cell and at most five witnesses: {small}"
    );
}

fn project_cost(passes: usize) -> u64 {
    let (journal, _) = repeated_evidence(passes);
    let before = journal.state.clone();
    let length = journal.events.len();
    let projection = journal.state.project();
    let visits = journal.state.visits();
    assert_eq!(projection.cores.len(), 1);
    let core_view = &projection.cores[0];
    assert_eq!(core_view.core, core(0));
    assert_eq!(core_view.phase, Some(CorePhase::Search));
    assert_eq!(core_view.offset, off(-10));
    assert_eq!(core_view.failure_point, None);
    assert_eq!(core_view.solo_limit, None);
    assert_eq!(projection.search_turns.len(), 1);
    let turn = &projection.search_turns[0];
    assert_eq!(turn.core, core(0));
    assert!(turn.confirm);
    assert!(!turn.running);
    assert_eq!(
        (turn.step, turn.light, turn.heavy, turn.needed),
        (0, u32::try_from(passes).unwrap(), 0, 5)
    );
    assert_eq!(turn.regimes, [Regime::R1, Regime::R2]);
    assert_eq!(turn.workload.regime(), Regime::R2);
    assert_eq!(projection.checking.profile, None);
    assert!(!projection.cycle_plan.open);
    assert_eq!(projection.requirement, None);
    assert_eq!(projection.trial_history.unwrap().passed, 5);
    assert_eq!(journal.state.project(), projection);
    assert_eq!(journal.state, before);
    assert_eq!(journal.events.len(), length);
    visits
}

#[test]
fn project_visits_bounded() {
    let small = project_cost(100);
    let large = project_cost(10_000);
    assert!(small > 0, "projection must consult candidate evidence");
    assert_eq!(large, small);
    assert!(
        small <= 2,
        "only the R1 and R2 candidate classes are consulted: {small}"
    );
}

fn forecast_cost(passes: usize) -> (u64, u64) {
    let (mut journal, _) = repeated_evidence(passes);
    let plan = journal.next_trial();
    assert_second_regime(&plan);
    journal.intent(&plan);
    journal.state.visits();
    let before = journal.state.clone();
    let length = journal.events.len();
    crate::evidence::take_copied();
    let forecast = journal.state.forecast();
    let copied = crate::evidence::take_copied();
    let visits = journal.state.visits();
    assert_eq!(forecast.between, None);
    assert_eq!(
        forecast
            .branches
            .iter()
            .map(|branch| branch.premise)
            .collect::<Vec<_>>(),
        [
            Premise::Pass,
            Premise::AllPass,
            Premise::Named(core(0)),
            Premise::Inconclusive
        ]
    );
    let pass = &forecast.branches[0];
    assert_eq!(pass.passes, 1);
    assert_eq!(pass.all_pass, None);
    assert_eq!(pass.uncertainty, None);
    assert!(pass.decisions.is_empty());
    assert_second_regime(pass.next.as_ref().unwrap());
    let all_pass = &forecast.branches[1];
    assert_eq!(all_pass.passes, 5);
    assert_eq!(all_pass.all_pass, Some(AllPass::Complete));
    assert_eq!(all_pass.uncertainty, None);
    assert!(all_pass.decisions.iter().any(|decision| matches!(decision, DecisionPayload::CorePhase(change) if change.core == core(0) && change.to != CorePhase::Search && change.offset == off(-9) && change.pass == Some(off(-10)))));
    assert_eq!(
        all_pass.next.as_ref().unwrap().profile.get(core(0)),
        Some(off(-9))
    );
    let named = &forecast.branches[2];
    assert_eq!(named.passes, 0);
    assert_eq!(named.uncertainty, None);
    assert!(named.decisions.iter().any(|decision| matches!(decision, DecisionPayload::Failure(failure) if failure.core == Some(core(0)) && failure.offset == Some(off(-10)))));
    assert!(named.decisions.iter().any(|decision| matches!(decision, DecisionPayload::TunerDecision(change) if change.core == core(0) && change.decision == Decision::Backoff && change.to_offset == off(-5))));
    let inconclusive = &forecast.branches[3];
    assert_eq!(inconclusive.passes, 0);
    assert_eq!(inconclusive.uncertainty, None);
    assert!(inconclusive.decisions.is_empty());
    let retry = inconclusive.next.as_ref().unwrap();
    assert_second_regime(retry);
    assert!(retry.retry);
    assert_eq!(retry.requirement, plan.requirement);
    assert_eq!(journal.state, before);
    assert_eq!(journal.events.len(), length);
    assert_eq!(journal.state.forecast(), forecast);
    (visits, copied)
}

#[test]
fn forecast_clone_not_replay() {
    let (small, small_copied) = forecast_cost(100);
    let (large, large_copied) = forecast_cost(10_000);
    assert!(
        small > 0,
        "counter must observe evidence consulted by synthetic branch folds"
    );
    assert_eq!(large, small);
    assert!(
        small <= 384,
        "four premises, five synthetic passes, and bounded cell/witness visits: {small}"
    );
    assert_eq!(
        large_copied, small_copied,
        "the branches copied seqs in proportion to the passes behind them"
    );
    assert!(small_copied < 100, "{small_copied}");
}

fn forecast_own_cell_cost(passes: usize) -> (u64, u64) {
    let (mut journal, plan) = repeated_evidence(passes);
    journal.intent(&plan);
    journal.state.visits();
    let before = journal.state.clone();
    crate::evidence::take_copied();
    let forecast = journal.state.forecast();
    let copied = crate::evidence::take_copied();
    let visits = journal.state.visits();
    let pass = &forecast.branches[0];
    assert_eq!(pass.premise, Premise::Pass);
    assert_eq!(pass.passes, 1);
    assert_eq!(journal.state, before);
    (visits, copied)
}

#[test]
fn forecast_appends_to_the_open_trials_cell_without_copying_its_history() {
    let (small, small_copied) = forecast_own_cell_cost(100);
    let (large, large_copied) = forecast_own_cell_cost(10_000);
    assert_eq!(large, small);
    assert_eq!(
        large_copied, small_copied,
        "a synthetic pass copied the passes its cell already held"
    );
    assert_eq!(
        small_copied, 1,
        "only the Pass premise's own pass, cloned once for its resume"
    );
}

fn forecast_after_retained_clone_cost(passes: usize) -> (u64, u64) {
    let (mut journal, plan) = repeated_evidence(1);
    let retained = journal.state.clone();
    for _ in 0..passes {
        journal.trial(&plan, Outcome::Pass);
    }
    journal.intent(&plan);
    journal.state.visits();
    crate::evidence::take_copied();
    let forecast = journal.state.forecast();
    let copied = crate::evidence::take_copied();
    let visits = journal.state.visits();
    assert_eq!(forecast.branches[0].premise, Premise::Pass);
    assert_ne!(journal.state, retained);
    (visits, copied)
}

#[test]
fn forecast_cost_does_not_grow_with_passes_since_a_retained_clone() {
    let (small, small_copied) = forecast_after_retained_clone_cost(100);
    let (large, large_copied) = forecast_after_retained_clone_cost(10_000);
    assert_eq!(large, small);
    let clones_per_forecast = 5;
    let bound = clones_per_forecast * 2 * crate::evidence::TAIL_CAP as u64;
    assert!(
        small_copied <= bound && large_copied <= bound,
        "forecasting copied the passes recorded since another clone was retained: \
         {small_copied} after 100, {large_copied} after 10,000, at most {bound} allowed"
    );
}

#[test]
fn a_cloned_state_shares_its_evidence_while_either_side_appends() {
    let (mut journal, _) = repeated_evidence(10_000);
    let second = journal.next_trial();
    assert_second_regime(&second);
    journal.trial(&second, Outcome::Pass);
    let snapshot = journal.state.clone();
    let shared = |journal: &Journal| {
        journal
            .run()
            .evidence
            .shared_with(&snapshot.run().unwrap().evidence)
    };
    let (all, total) = shared(&journal);
    assert!(total >= 2, "{total}");
    assert_eq!(all, total, "a clone copied seqs it only reads");
    crate::evidence::take_copied();
    journal.trial(&second, Outcome::Pass);
    let copied = crate::evidence::take_copied();
    assert_eq!(
        (shared(&journal), copied),
        ((total, total), 0),
        "appending a pass must keep sharing the history and copy none of it"
    );
    assert_ne!(journal.state, snapshot, "the append reached the clone");
}

#[test]
fn signatures_are_shared_borrows_without_a_journal() {
    let _: fn(&State) -> Result<Action, NotReady> = State::next;
    let _: fn(&State) -> Option<DecisionAction> = State::drain;
    let _: fn(&State) -> Projection = State::project;
    let _: fn(&State) -> Forecast = State::forecast;
}

#[test]
fn replay_folds_a_whole_journal_and_stops_at_the_first_event_it_cannot_follow() {
    let mut j = Journal::has_room(&[-20, -20]);
    for _ in 0..3 {
        let decision = j.next_decision();
        j.decide(decision);
    }
    let replayed = crate::replay(&j.events).unwrap();
    assert_eq!(replayed, j.state);
    assert_eq!(replayed.next(), j.state.next());
    assert_eq!(crate::replay([]).unwrap(), State::new());
    let again = j.events.iter().chain(j.events.first());
    assert!(matches!(
        crate::replay(again),
        Err(crate::FoldError::OutOfOrder { .. })
    ));
    assert!(matches!(
        crate::replay(&j.events[1..]),
        Err(crate::FoldError::NotStarted)
    ));
}

#[test]
fn a_cloned_state_shares_the_r7_witnesses_until_one_side_writes_a_segment() {
    let mut journal = Journal::r7_session();
    let report = crate::testkit::R7Report::default().requests(&[(0, 1.1), (1, 1.0)]);
    for _ in 0..500 {
        journal.r7_trial(&[0, 1], &[-30; 4], Outcome::Pass, &report);
    }
    let snapshot = journal.state.clone();
    let shared = |journal: &Journal| {
        journal
            .run()
            .r7
            .targets
            .shared_with(&snapshot.run().unwrap().r7.targets)
    };
    let (all, total) = shared(&journal);
    assert!(total > 0);
    assert_eq!(all, total, "a clone copied witnesses it only reads");
    let witnesses = |state: &State| state.run().unwrap().r7.targets.witnesses();
    assert_eq!(witnesses(&snapshot), 1000);
    journal.r7_trial(&[0, 1], &[-30; 4], Outcome::Pass, &report);
    assert_eq!(shared(&journal), (0, total));
    assert_eq!(witnesses(&snapshot), 1000, "a write reached the clone");
    assert_eq!(witnesses(&journal.state), 1002);
}

#[test]
fn a_cloned_state_shares_its_failure_witnesses() {
    let (mut journal, _) = repeated_evidence(100);
    let second = journal.next_trial();
    assert_second_regime(&second);
    journal.trial(&second, Outcome::Failure);
    let clone = journal.state.clone();
    let (shared, total) = journal
        .run()
        .evidence
        .shared_with(&clone.run().unwrap().evidence);
    assert!(total >= 3, "{total}");
    assert_eq!(shared, total);
}
