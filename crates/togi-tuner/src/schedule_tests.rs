use togi_journal::{CorePhase, CoreTransition, Decision, Outcome, Payload, TrialId, TunerDecision};
use togi_machine::{Condition, Regime, workloads};

use crate::tests::{core, off};
use crate::testkit::{Journal, Start};
use crate::{
    Action, DecisionAction, DecisionPayload, Load, TrialContext, WorkloadChoice,
};

pub(crate) fn transition(id: u8, to: CorePhase, offset: i64) -> CoreTransition {
    CoreTransition {
        core: core(id),
        from: None,
        to,
        offset: off(offset),
        pass: None,
        failure_point: None,
        check_solo_limit: false,
        workloads: Vec::new(),
        cleared_combination: Vec::new(),
        reason: "test".to_owned(),
    }
}

pub(crate) fn decision(action: &DecisionAction) -> &TunerDecision {
    match &action.payload {
        DecisionPayload::TunerDecision(decision) => decision,
        other => panic!("expected a tuner.decision, got {other:?}"),
    }
}

#[test]
fn search_and_solo_limit_check() {
    let mut j = Journal::search_at(&[-49]);
    let n = usize::from(j.state.run().unwrap().config.needed.get());
    let first = j.next_trial();
    assert_eq!(first.regime, Regime::R1);
    assert_eq!(first.duration.get(), 90);
    assert_eq!(first.condition, Condition::Alone);
    let r1 = j.trial(&first, Outcome::Pass);
    let second = j.next_trial();
    assert_eq!(second.regime, Regime::R2);
    let r2 = j.trial(&second, Outcome::Pass);
    let step = j.next_decision();
    assert_eq!(step.cause, [r1, r2]);
    let d = decision(&step);
    assert_eq!((d.decision, d.to_offset.get()), (Decision::StepDeeper, -50));
    j.decide(step);
    for regime in [Regime::R1, Regime::R2] {
        let plan = j.next_trial();
        assert_eq!(plan.regime, regime);
        j.trial(&plan, Outcome::Pass);
    }
    let check = j.next_decision();
    let d = decision(&check);
    assert_eq!(d.decision, Decision::CheckSoloLimit);
    assert_eq!(d.to_offset.get(), -50);
    assert_eq!(d.workloads.len(), 2);
    j.decide(check);
    for regime in [Regime::R1, Regime::R2] {
        for _ in 0..n {
            let plan = j.next_trial();
            assert_eq!(plan.regime, regime);
            assert!(matches!(plan.workload, WorkloadChoice::Frozen(_)));
            assert!(matches!(plan.context, TrialContext::SoloLimit { .. }));
            j.trial(&plan, Outcome::Pass);
        }
    }
    let exit = j.next_decision();
    let DecisionPayload::CorePhase(phase) = &exit.payload else {
        panic!("expected core.phase, got {exit:?}");
    };
    assert_eq!(phase.to, togi_journal::CorePhase::HasRoom);
    assert_eq!(phase.offset.get(), -49);
    assert_eq!(phase.pass.map(|pass| pass.get()), Some(-50));
    j.decide(exit);
    let Action::Decide(DecisionAction {
        payload: DecisionPayload::ProfileChange(change),
        ..
    }) = j.next()
    else {
        panic!("expected profile.change");
    };
    assert_eq!(change.to.iter().map(|(_, o)| o.get()).collect::<Vec<_>>(), [-49]);
    j.assert_resumes();
}

#[test]
fn search_failure_and_retry() {
    let mut j = Journal::search_at(&[-10]);
    let first = j.next_trial();
    j.trial(&first, Outcome::Inconclusive);
    let retry = j.next_trial();
    assert!(retry.retry);
    assert_eq!(retry.regime, Regime::R1);
    assert_eq!(retry.duration, first.duration);
    j.trial(&retry, Outcome::Failure);
    let failure = j.next_decision();
    let DecisionPayload::Failure(attributed) = &failure.payload else {
        panic!("expected a failure, got {failure:?}");
    };
    assert_eq!(attributed.attribution, togi_journal::Attribution::Attributed);
    assert_eq!(attributed.core, Some(core(0)));
    j.decide(failure);
    let back = j.next_decision();
    let d = decision(&back);
    assert_eq!(d.decision, Decision::Backoff);
    assert_eq!(d.to_offset.get(), -5);
    assert_eq!(d.failure_point.map(|point| point.get()), Some(-10));
    let text = Payload::from(back.payload).message();
    assert!(text.contains("failed"), "{text}");
    j.assert_resumes();
}

#[test]
fn decision_supersedes_alone_retry() {
    let mut j = Journal::search_at(&[-10]);
    let first = j.next_trial();
    j.trial(&first, Outcome::Inconclusive);
    j.core_phase(transition(0, CorePhase::Search, -5));
    let plan = j.next_trial();
    assert!(!plan.retry);
    assert_eq!(plan.profile.get(core(0)).get(), -5);
}

#[test]
fn search_skips_cores_with_room() {
    let j = Journal::new(&[0, 1], &[Start::at_limit(-50), Start::search(-10)]);
    let plan = j.next_trial();
    assert_eq!(plan.load, Load::Single(core(1)));
    assert_eq!(plan.regime, Regime::R1);
    assert_eq!(plan.condition, Condition::Alone);
}

fn checking_phase(id: u8, offset: i64, pair: [usize; 2]) -> CoreTransition {
    CoreTransition {
        check_solo_limit: true,
        workloads: vec![
            workloads(Regime::R1)[pair[0]].id(),
            workloads(Regime::R2)[pair[1]].id(),
        ],
        ..transition(id, CorePhase::Search, offset)
    }
}

#[test]
fn seeded_candidate_solo_limit_freezes_workloads() {
    let mut j = Journal::search_at(&[-20]);
    j.core_phase(checking_phase(0, -20, [1, 2]));
    let n = usize::from(j.state.run().unwrap().config.needed.get());
    for (regime, frozen) in [(Regime::R1, 1), (Regime::R2, 2)] {
        for _ in 0..n {
            let plan = j.next_trial();
            assert_eq!(plan.regime, regime);
            assert_eq!(
                plan.workload,
                WorkloadChoice::Frozen(workloads(regime)[frozen].id())
            );
            j.trial(&plan, Outcome::Pass);
        }
    }
    let exit = j.next_decision();
    let DecisionPayload::CorePhase(phase) = &exit.payload else {
        panic!("expected core.phase, got {exit:?}");
    };
    assert_eq!(phase.to, CorePhase::HasRoom);
    assert_eq!(phase.pass.map(|pass| pass.get()), Some(-20));
}

#[test]
fn seeded_candidate_solo_limit_advances_next_workload_pair() {
    let mut j = Journal::search_at(&[-20]);
    j.core_phase(checking_phase(0, -20, [0, 0]));
    let mut failed = false;
    let seen = j.run_until(
        100,
        |_| {
            if failed {
                Outcome::Pass
            } else {
                failed = true;
                Outcome::Failure
            }
        },
        |action| {
            matches!(action, Action::Decide(DecisionAction {
                payload: DecisionPayload::TunerDecision(d), ..
            }) if d.decision == Decision::CheckSoloLimit)
        },
    );
    let Some(Action::Decide(last)) = seen.last() else {
        panic!("the search never scheduled the next candidate solo limit");
    };
    assert_eq!(
        decision(last).workloads,
        [workloads(Regime::R1)[1].id(), workloads(Regime::R2)[1].id()]
    );
}

#[test]
fn trial_plan_intent_rotation() {
    for regime in [Regime::R1, Regime::R7] {
        let mut j = Journal::has_room(&[-10]);
        let load = if regime == Regime::R1 {
            Load::Single(core(0))
        } else {
            Load::Many(crate::testkit::set(&[0]))
        };
        let steps = [
            (Outcome::Pass, None, 0),
            (Outcome::Failure, None, 1),
            (Outcome::Inconclusive, None, 2),
            (Outcome::Pass, Some(1), 1),
            (Outcome::Pass, None, 0),
        ];
        for (outcome, frozen, want) in steps {
            let mut plan = j.manual(regime, 0, load, Condition::Together, &[-10], 120);
            plan.workload = frozen.map_or(WorkloadChoice::Rotate, |index| {
                WorkloadChoice::Frozen(workloads(regime)[index].id())
            });
            let intent = plan.intent(TrialId::from("probe".to_owned()), j.rotation(&plan));
            assert_eq!(intent.workload, workloads(regime)[want].id(), "{regime} {outcome:?}");
            j.trial(&plan, outcome);
            if outcome == Outcome::Failure {
                let attribution = j.next_decision();
                j.decide(attribution);
            }
        }
    }
}
