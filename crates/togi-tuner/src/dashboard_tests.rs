use std::num::{NonZeroU8, NonZeroU32};

use test_case::test_case;
use togi_journal::{Activity, HuntGroup, HuntStage, HuntStart, Member, Outcome, Seq};
use togi_machine::{Condition, Regime, workloads};

use crate::chain_tests::{
    FULL_REQUESTS, PARTIAL_REQUESTS, SHORT, STARTS, chain_journal, decided, journal, one_ccd,
    pass_part, run, shallower, two_ccds,
};
use crate::dashboard::{
    CyclePart, GroupState, Lengths, MemberProbe, Quota, TrialRequirement, TurnKind,
};
use crate::hunt::plan::GroupOutcome;
use crate::hunt::support::{
    at, attribute, cancel_hunt, group_of, hunt_of, ids, limit_session, projection_cases,
    repeated_core_oracle, repeated_session, start_hunt_at,
};
use crate::testkit::{Journal, Start, config, fresh, set};
use crate::tests::{core, off};
use crate::{
    Action, CombinationId, DecisionAction, DecisionPayload, FailurePoint, Limit, Load, PartNo,
    Reached, StepNo, TrialContext, TrialHistory, TrialPlan,
};

fn chain_record(j: &Journal, step: u32, part: u32, cores: &[u8]) -> togi_journal::Payload {
    togi_journal::CheckingChain {
        cycle: j.run().checking.open().unwrap().number,
        step: NonZeroU32::new(step).unwrap(),
        ccd: togi_machine::CcdId::new(0).unwrap(),
        workload: togi_machine::pick_workload(Regime::R7, 0).id(),
        groups: Vec::new(),
        cores: set(cores),
        source_seqs: Vec::new(),
        profile: j.run().checking.profile().unwrap().offsets(),
        part: NonZeroU32::new(part).unwrap(),
        rederived: None,
    }
    .into()
}

fn step_record(j: &Journal, step: u32) -> togi_journal::Payload {
    togi_journal::CheckingStep {
        cycle: j.run().checking.open().unwrap().number,
        step: NonZeroU32::new(step).unwrap(),
        profile: j.run().checking.profile().unwrap().offsets(),
    }
    .into()
}

fn short_secs(part: &CyclePart) -> u32 {
    part.lengths.expect("the part has trials").short.secs
}

fn part(cores: &[u8], ccd: Option<u8>, partial: bool, short_s: u32, long: (u32, u32)) -> CyclePart {
    CyclePart {
        cores: set(cores),
        ccd: ccd.map(|ccd| togi_machine::CcdId::try_from(ccd).unwrap()),
        full: !partial,
        partial,
        lengths: Some(Lengths {
            short: Quota {
                count: 3,
                secs: short_s,
            },
            long: (long.0 > 0).then_some(Quota {
                count: long.0,
                secs: long.1,
            }),
        }),
        passed: 0,
        failed: 0,
        done: false,
        running: false,
    }
}

fn parts(j: &Journal, step: usize) -> Vec<CyclePart> {
    j.state.cycle_plan().steps[step].parts.clone()
}

fn decide_next(j: &mut Journal) {
    let next = decided(j.next());
    j.decide(next);
}

fn trial_after_decisions(j: &mut Journal) -> TrialPlan {
    for _ in 0..20 {
        match j.next() {
            Action::Decide(decision) => {
                j.decide(decision);
            }
            Action::RunTrial(plan) => return plan,
            other @ Action::ReadRanking => panic!("{other:?}"),
        }
    }
    panic!("no trial followed the decisions");
}

fn cycle_plan_resumes(j: &Journal) {
    assert_eq!(fresh(&j.events).cycle_plan(), j.state.cycle_plan());
    j.assert_resumes();
}

fn cycle_plan_of(j: &Journal, step: u32, cores: &[u8], secs: u32) -> TrialPlan {
    let mut plan = j.manual(
        Regime::R7,
        0,
        Load::Many(set(cores)),
        Condition::Together,
        &j.offsets()
            .offsets()
            .iter()
            .map(|(_, offset)| offset.get())
            .collect::<Vec<_>>(),
        secs,
    );
    plan.requirement.context = TrialContext::Cycle {
        cycle: j.run().checking.open().unwrap().number,
        step: StepNo::new(NonZeroU32::new(step).unwrap()),
        part: PartNo::new(NonZeroU32::MIN),
    };
    plan
}

type Case = (
    &'static str,
    i64,
    Option<i64>,
    Option<Vec<(u8, i64)>>,
    Option<Limit>,
);

#[test]
fn core_limit_holders() {
    let combination = |first: i64, second: i64| vec![(0, first), (1, second)];
    let cases: Vec<Case> = vec![
        ("room", -20, None, None, None),
        ("floor", -50, None, None, Some(Limit::Floor)),
        (
            "failure",
            -20,
            Some(-21),
            None,
            Some(Limit::Reached(Reached::FailurePoint {
                core: core(0),
                point: FailurePoint::new(off(-21)),
            })),
        ),
        (
            "combination",
            -20,
            None,
            Some(combination(-21, -10)),
            Some(Limit::Reached(Reached::Combination(CombinationId::new(
                NonZeroU32::new(7).unwrap(),
            )))),
        ),
        (
            "clear combination",
            -20,
            None,
            Some(combination(-21, -11)),
            None,
        ),
    ];
    for (name, offset, fail, members, want) in cases {
        let mut start = Start::at_limit(offset);
        if let Some(fail) = fail {
            start = start.fail(fail);
        }
        let mut j = Journal::new(&[0, 1], &[start, Start::has_room(-10)]);
        if let Some(members) = members {
            j.combination(7, &members);
        }
        assert_eq!(j.state.core_limit(core(0)), want, "{name}");
        assert_eq!(fresh(&j.events).core_limit(core(0)), want, "{name}");
        assert_eq!(j.state.core_limit(core(9)), None, "{name}");
    }
}

#[test]
fn cycle_plan_dynamic_chain_requirements() {
    let mut j = chain_journal();
    let first = part(&[0, 1, 2, 3], Some(0), false, 120, (1, 300));
    let second = part(&[4, 5, 6, 7], Some(1), false, 120, (1, 300));
    let whole = part(&[0, 1, 2, 3, 4, 5, 6, 7], None, false, 120, (1, 600));
    assert_eq!(
        parts(&j, 0),
        [first.clone(), second.clone(), whole.clone()],
        "invented a partial before its predecessor passed"
    );
    pass_part(&mut j, &[0, 1, 2, 3], &FULL_REQUESTS);
    let plan = j.state.cycle_plan();
    assert!(plan.steps[0].parts[0].done && !plan.steps[0].done);
    assert_eq!((plan.steps[0].chains_complete, plan.current), (false, 0));
    decide_next(&mut j);
    let mut partial = part(&[0, 1, 3], Some(0), true, 120, (1, 300));
    assert_eq!(parts(&j, 0)[1], partial, "measured request order ignored");
    pass_part(&mut j, &[0, 1, 3], &PARTIAL_REQUESTS);
    decide_next(&mut j);
    partial.cores = set(&[0, 3]);
    let plan = j.state.cycle_plan();
    assert_eq!(plan.steps[0].parts[2], partial, "second partial missing");
    let passed = &plan.steps[0].parts[1];
    assert_eq!((passed.done, passed.passed), (true, 4));
    pass_part(&mut j, &[0, 3], &[(0, 1.2), (3, 1.1)]);
    decide_next(&mut j);
    pass_part(&mut j, &[4, 5, 6, 7], &[]);
    decide_next(&mut j);
    let plan = j.state.cycle_plan();
    assert!(
        plan.steps[0].chains_complete && !plan.steps[0].done,
        "{plan:?}"
    );
    assert_eq!(plan.steps[0].parts.len(), 5, "{plan:?}");
    pass_part(&mut j, &[0, 1, 2, 3, 4, 5, 6, 7], &[]);
    let plan = j.state.cycle_plan();
    assert_eq!((plan.steps[0].done, plan.current), (true, 1));
    cycle_plan_resumes(&j);
}

#[test]
fn cycle_plan_partial_failure_needs_passes() {
    let mut j = chain_journal();
    pass_part(&mut j, &[0, 1, 2, 3], &FULL_REQUESTS);
    decide_next(&mut j);
    let plan = j.next_trial();
    j.trial(&plan, Outcome::Pass);
    let plan = j.next_trial();
    let named = plan.load.cores().first().unwrap().get();
    run(&mut j, &plan, Outcome::Failure, &[], Some(named));
    let got = parts(&j, 0)[1].clone();
    assert_eq!((got.passed, got.failed, got.done), (0, 1, false));
    let failure = j.next_decision();
    assert!(matches!(failure.payload, DecisionPayload::Failure(_)));
    j.decide(failure);
    let action = j.run().cycle_action().unwrap();
    let Action::RunTrial(again) = action else {
        panic!("{action:?}");
    };
    j.intent(&again);
    assert_eq!(
        j.state.requirement(),
        Some(TrialRequirement {
            passed: 0,
            failed: 1,
            trial: 1,
            needed: 3,
        })
    );
    assert!(parts(&j, 0)[1].running, "partial trial not shown running");
    j.assert_resumes();
}

#[test]
fn cycle_plan_partial_profile_changes() {
    for started in [false, true] {
        let mut j = chain_journal();
        pass_part(&mut j, &[0, 1, 2, 3], &FULL_REQUESTS);
        decide_next(&mut j);
        if started {
            let plan = j.next_trial();
            j.trial(&plan, Outcome::Inconclusive);
        }
        shallower(&mut j, 0, 10);
        let plan = j.state.cycle_plan();
        assert_eq!(plan.steps[0].parts.len(), 3, "stale partial projected");
        decide_next(&mut j);
        assert_eq!(parts(&j, 0)[1].cores, set(&[1, 2, 3]), "started {started}");
        cycle_plan_resumes(&j);
    }
}

#[test]
fn cycle_plan_repeated_partial_requirements() {
    let mut j = journal(
        two_ccds(),
        &STARTS,
        Some(r#"{"checking":{"cycle":["R7","R7","R7","R7"]}}"#),
    );
    for step in [1, 4] {
        if step != 1 {
            j.append(step_record(&j, step), &[]);
        }
        j.append(chain_record(&j, step, 1, &[1, 2, 3]), &[]);
    }
    let plan = cycle_plan_of(&j, 4, &[1, 2, 3], SHORT);
    for _ in 0..2 {
        j.trial(&plan, Outcome::Pass);
    }
    j.intent(&plan);
    assert_eq!(
        j.state.requirement(),
        Some(TrialRequirement {
            passed: 2,
            failed: 0,
            trial: 3,
            needed: 6,
        }),
        "explicit step answered an earlier occurrence"
    );
    let view = j.state.cycle_plan();
    let got = &view.steps[3].parts[1];
    assert!(
        got.counts() == (6, 2) && got.passed == 2 && !got.done && got.running,
        "{got:?}"
    );
    assert!(
        !view.steps[0].parts[1].running,
        "earlier occurrence running"
    );
    cycle_plan_resumes(&j);
}

#[test]
fn cycle_plan_partial_evidence_windows() {
    for carried in [false, true] {
        let mut j = chain_journal();
        pass_part(&mut j, &[0, 1, 2, 3], &[]);
        decide_next(&mut j);
        let plan = j.next_trial();
        assert_eq!(plan.load, Load::Many(set(&[1, 2, 3])));
        let profile: Vec<i64> = j.offsets().offsets().iter().map(|(_, o)| o.get()).collect();
        for _ in 0..3 {
            if carried {
                j.carry(
                    "20230101T000000Z",
                    (Regime::R7, 0, &[1, 2, 3], SHORT),
                    Condition::Together,
                    &profile,
                    Outcome::Pass,
                );
            } else {
                j.trial(&plan, Outcome::Pass);
            }
        }
        let want = if carried { 0 } else { 3 };
        let got = parts(&j, 0)[1].clone();
        assert_eq!((got.passed, got.done), (want, false));
        let long = cycle_plan_of(&j, 1, &[1, 2, 3], 300);
        j.intent(&long);
        assert_eq!(
            j.state.requirement(),
            Some(TrialRequirement {
                passed: 0,
                failed: 0,
                trial: 1,
                needed: 1,
            }),
            "short partial evidence leaked into the long class (carried {carried})"
        );
        cycle_plan_resumes(&j);
    }
}

#[test]
fn cycle_plan_credits_each_quota_only_up_to_its_count() {
    let mut j = chain_journal();
    pass_part(&mut j, &[0, 1, 2, 3], &[]);
    decide_next(&mut j);
    let plan = j.next_trial();
    for _ in 0..5 {
        j.trial(&plan, Outcome::Pass);
    }
    let long = cycle_plan_of(&j, 1, &[1, 2, 3], 300);
    let (intent, trial) = j.intent(&long);
    let got = parts(&j, 0)[1].clone();
    assert!(
        got.counts() == (3, 1) && got.passed == 3 && !got.done && got.running,
        "five short passes credit three: {got:?}"
    );
    j.end(intent, &trial, Outcome::Pass);
    let got = parts(&j, 0)[1].clone();
    assert_eq!((got.passed, got.done), (4, true));
    cycle_plan_resumes(&j);
}

#[test]
fn cycle_plan_one_ccd_merges_coincident_durations() {
    let mut j = journal(
        one_ccd(),
        &STARTS,
        Some(r#"{"checking":{"cycle":["R7"]},"durations":{"short_trial_s":1200}}"#),
    );
    let all = [0, 1, 2, 3, 4, 5, 6, 7];
    let view = parts(&j, 0);
    assert!(
        view.len() == 1 && view[0].full && view[0].counts() == (4, 0),
        "{view:?}"
    );
    pass_part_merged(&mut j, &all);
    decide_next(&mut j);
    let got = parts(&j, 0)[1].clone();
    assert!(
        got.partial && !got.full && got.counts() == (4, 0) && short_secs(&got) == 1200,
        "{got:?}"
    );
    let plan = j.next_trial();
    j.intent(&plan);
    assert_eq!(
        j.state.requirement(),
        Some(TrialRequirement {
            passed: 0,
            failed: 0,
            trial: 1,
            needed: 4,
        })
    );
}

fn pass_part_merged(j: &mut Journal, cores: &[u8]) {
    for _ in 0..4 {
        let plan = j.next_trial();
        assert_eq!(plan.load, Load::Many(set(cores)), "{plan:?}");
        j.trial(&plan, Outcome::Pass);
    }
}

#[test]
fn requirement_counts_only_fresh_search_trials() {
    let mut j = Journal::search_at(&[-20]);
    let first = j.next_trial();
    j.trial(&first, Outcome::Pass);
    let second = j.next_trial();
    j.trial(&second, Outcome::Pass);
    decide_next(&mut j);
    let later = j.next_trial();
    assert_ne!(
        later.profile, first.profile,
        "the search stayed on its step"
    );
    j.intent(&later);
    assert_eq!(
        j.state.requirement(),
        Some(TrialRequirement {
            passed: 0,
            failed: 0,
            trial: 1,
            needed: 1,
        }),
        "a search step's trial counted an earlier step's pass"
    );
}

#[test]
fn requirement_counts_alone_deepening_checks_against_their_round() {
    let mut j = Journal::has_room(&[-20, -20]);
    j.round_start(2, &[-20, -21], &[1], 3);
    for passed in 0..2_u32 {
        let plan = trial_after_decisions(&mut j);
        assert!(
            plan.condition == Condition::Alone
                && matches!(plan.requirement.context, TrialContext::Round { round } if round.get() == 2),
            "{plan:?}"
        );
        let (intent, trial) = j.intent(&plan);
        assert_eq!(
            j.state.requirement(),
            Some(TrialRequirement {
                passed,
                failed: 0,
                trial: passed + 1,
                needed: 3,
            }),
            "an alone deepening check counted as a search step"
        );
        j.end(intent, &trial, Outcome::Pass);
    }
}

#[test]
fn cycle_plan_runs_no_part_during_a_rerun() {
    let mut j = chain_journal();
    pass_part(&mut j, &[0, 1, 2, 3], &FULL_REQUESTS);
    decide_next(&mut j);
    pass_part(&mut j, &[0, 1, 3], &PARTIAL_REQUESTS);
    decide_next(&mut j);
    let plan = j.next_trial();
    assert_eq!(plan.load, Load::Many(set(&[0, 3])));
    run(&mut j, &plan, Outcome::Failure, &[], Some(3));
    let attribution = j.next_decision();
    assert!(matches!(attribution.payload, DecisionPayload::Failure(_)));
    j.decide(attribution);
    let backoff = j.next_decision();
    let DecisionPayload::TunerDecision(moved) = &backoff.payload else {
        panic!("expected a backoff, got {backoff:?}");
    };
    assert_eq!((moved.core, moved.to_offset.get()), (core(3), -39));
    assert_eq!(moved.decision, togi_journal::Decision::Backoff);
    j.decide(backoff);
    // Without a failure clock there is no V*: one count leaves no room before the failure point.
    let limit = j.next_decision();
    let DecisionPayload::CorePhase(transition) = &limit.payload else {
        panic!("expected the failure-point limit, got {limit:?}");
    };
    assert_eq!(
        (transition.core, transition.from, transition.to),
        (
            core(3),
            Some(togi_journal::CorePhase::HasRoom),
            togi_journal::CorePhase::AtLimit,
        )
    );
    j.decide(limit);
    let change = j.next_decision();
    assert!(matches!(change.payload, DecisionPayload::ProfileChange(_)));
    j.decide(change);
    assert!(!j.run().reruns.is_empty());
    let Action::RunTrial(rerun) = j.next() else {
        panic!("the failure did not rerun");
    };
    assert!(
        matches!(rerun.requirement.context, TrialContext::Rerun { .. }),
        "{rerun:?}"
    );
    j.intent(&rerun);
    let view = j.state.cycle_plan();
    assert!(view.paused, "{view:?}");
    assert!(
        view.steps
            .iter()
            .all(|step| step.parts.iter().all(|part| !part.running)),
        "the rerun ran as a part of the paused cycle: {view:?}"
    );
}

#[test]
fn cycle_plan_runs_only_the_current_steps_part() {
    let mut j = Journal::has_room(&[-20, -20]);
    j.ranking(&[0, 1]);
    j.load(&config(r#"{"checking":{"cycle":["R6","R6","R6","R6"]}}"#));
    for step in 0..4 {
        let plan = loop {
            match j.next() {
                Action::Decide(decision) => {
                    j.decide(decision);
                }
                Action::RunTrial(plan) => break plan,
                other @ Action::ReadRanking => panic!("{other:?}"),
            }
        };
        let (intent, trial) = j.intent(&plan);
        let view = j.state.cycle_plan();
        for (at, one) in view.steps.iter().enumerate() {
            for each in &one.parts {
                assert_eq!(each.running, at == step, "step {step}: {view:?}");
            }
        }
        j.end(intent, &trial, Outcome::Pass);
    }
}

fn group_payload(
    hunt: u32,
    group: u32,
    stage: HuntStage,
    set_of: &[u8],
    granularity: u32,
    index: u32,
    duration_s: u32,
) -> HuntGroup {
    HuntGroup {
        hunt: NonZeroU32::new(hunt).unwrap(),
        group: NonZeroU32::new(group).unwrap(),
        cores: set(&set_of[..set_of.len().min(2)]),
        profile: togi_machine::PerCore::default(),
        set: set(set_of),
        granularity,
        stage,
        index,
        duration_s,
        escalated: false,
        full_checked: false,
        any_failed: false,
        probe: None,
        held: Vec::new(),
        inferred: None,
        skipped: false,
        reason: Some("test".to_owned()),
    }
}

type Cut<'a> = (HuntStage, &'a [u8], u32, u32, u32, u32);

#[test]
fn hunt_cut_counts_splits_across_the_hunt() {
    let all: &[u8] = &[0, 1, 2, 3];
    let sequences: [(&str, Vec<Cut<'_>>); 3] = [
        (
            "first split, its complements included, then a finer cut",
            vec![
                (HuntStage::Part, all, 2, 0, 60, 1),
                (HuntStage::Part, all, 2, 1, 60, 1),
                (HuntStage::Complement, all, 2, 0, 60, 1),
                (HuntStage::Part, &[0, 1], 2, 0, 60, 2),
                (HuntStage::Full, all, 0, 0, 60, 0),
            ],
        ),
        (
            "a split recurring after other cuts is a new cut",
            vec![
                (HuntStage::Part, all, 4, 0, 60, 1),
                (HuntStage::Part, all, 2, 0, 60, 2),
                (HuntStage::Part, all, 2, 1, 60, 2),
                (HuntStage::Part, all, 4, 0, 60, 3),
                (HuntStage::Part, all, 4, 1, 60, 3),
            ],
        ),
        (
            "the same set and granularity at another trial length is a new cut",
            vec![
                (HuntStage::Part, all, 2, 0, 60, 1),
                (HuntStage::Part, all, 2, 1, 60, 1),
                (HuntStage::Complement, all, 2, 0, 60, 1),
                (HuntStage::Part, all, 2, 0, 120, 2),
                (HuntStage::Part, all, 2, 1, 120, 2),
            ],
        ),
    ];
    for (name, groups) in sequences {
        let mut j = limit_session(4);
        start_hunt_at(&mut j, 1, &[0, 1, 2, 3], &[-30; 4]);
        for (at, (stage, set_of, granularity, index, duration, want)) in groups.iter().enumerate() {
            let mut payload = group_payload(
                1,
                u32::try_from(at + 1).unwrap(),
                *stage,
                set_of,
                *granularity,
                *index,
                *duration,
            );
            payload.profile = j.profile(&[-30; 4]).offsets();
            j.append(payload.into(), &[]);
            let live = j.state.hunt_plan().unwrap().cut;
            let replayed = fresh(&j.events).hunt_plan().unwrap().cut;
            assert_eq!((live, replayed), (*want, *want), "{name}: group {}", at + 1);
        }
        j.assert_resumes();
    }
}

#[test]
fn hunt_plan_cut_counts_an_upcoming_distinct_set_as_a_new_cut() {
    let mut j = hunt_of(4, SHORT);
    let mut oracle = |plan: &TrialPlan| {
        if at(plan, 0) <= -30 && at(plan, 1) <= -30 {
            Outcome::Failure
        } else {
            Outcome::Pass
        }
    };
    let mut last: Option<(togi_machine::CoreSet, u32)> = None;
    let (mut want, mut same_granularity_new_set) = (0, false);
    for _ in 0..300 {
        if j.state.hunt_plan().is_none() {
            break;
        }
        let action = j.next();
        if let Some(group) = group_of(&action).filter(|group| {
            group.probe.is_none() && matches!(group.stage, HuntStage::Part | HuntStage::Complement)
        }) {
            let signature = (group.set, group.granularity);
            if last != Some(signature) {
                same_granularity_new_set |= last.is_some_and(|(_, g)| g == group.granularity);
                want += 1;
                last = Some(signature);
            }
            assert_eq!(
                j.state.hunt_plan().unwrap().cut,
                want,
                "upcoming group {group:?}"
            );
            assert_eq!(fresh(&j.events).hunt_plan().unwrap().cut, want);
        }
        if matches!(action, Action::RunTrial(_))
            && j.state.hunt_plan().is_some_and(|plan| plan.cut != 0)
        {
            assert_eq!(j.state.hunt_plan().unwrap().cut, want, "running group");
        }
        j.step(&mut oracle);
    }
    assert!(
        same_granularity_new_set && want == 2,
        "the hunt never cut a distinct set at the same granularity (cuts {want})"
    );
}

#[test]
fn hunt_plan_cut_advances_when_passing_splits_return_to_the_singleton_granularity() {
    let mut j = repeated_session();
    let mut reached = false;
    for _ in 0..1000 {
        if group_of(&j.next()).is_some_and(|group| group.hunt.get() == 3 && group.group.get() == 1)
        {
            reached = true;
            break;
        }
        j.step(&mut repeated_core_oracle);
    }
    assert!(
        reached,
        "the corroborated singleton group was never planned"
    );
    let candidates = j.state.hunt_plan().unwrap().candidates.len();
    let n = u32::try_from(candidates).unwrap();
    let (mut previous, mut want) = (n, 1);
    let check = |j: &Journal, when: &str, want: u32| {
        let live = j.state.hunt_plan().unwrap().cut;
        assert_eq!(live, want, "{when}: live cut");
        assert_eq!(
            fresh(&j.events).hunt_plan().unwrap().cut,
            want,
            "{when}: replayed cut"
        );
    };
    let mut revisited = false;
    let mut pass = |_: &TrialPlan| Outcome::Pass;
    for _ in 0..2000 {
        if j.state.hunt_plan().is_none() {
            break;
        }
        let action = j.next();
        if let Some(group) = group_of(&action) {
            if group.probe.is_none()
                && matches!(group.stage, HuntStage::Part | HuntStage::Complement)
            {
                if group.granularity != previous {
                    previous = group.granularity;
                    want += 1;
                }
                check(&j, "planned group", want);
                j.step(&mut pass);
                if group.granularity == n
                    && u32::try_from(group.set.len()).unwrap() == n
                    && want > 1
                {
                    revisited = true;
                    break;
                }
                continue;
            }
        }
        if matches!(action, Action::RunTrial(_)) {
            check(&j, "running group", want);
        }
        j.step(&mut pass);
    }
    if revisited && matches!(j.next(), Action::RunTrial(_)) {
        check(&j, "running revisited group", want);
    }
    let mut finer = 0;
    let mut g = 2;
    while g < n {
        g = (2 * g).min(n);
        finer += 1;
    }
    assert!(
        revisited && want == 2 + finer,
        "the hunt never returned to granularity {n} after finer passing splits (cut {want}, want {})",
        2 + finer
    );
}

#[test]
fn hunt_plan_running_member_is_not_done() {
    let mut j = hunt_of(4, SHORT);
    for _ in 0..300 {
        let action = j.next();
        if let Some(group) = group_of(&action) {
            let probe = group.probe;
            let Action::Decide(decision) = action else {
                unreachable!();
            };
            j.decide(decision);
            if let Some(probe) = probe {
                let plan = j.next_trial();
                j.intent(&plan);
                let found = j
                    .state
                    .hunt_plan()
                    .unwrap()
                    .probes
                    .into_iter()
                    .find(|each| each.member == probe.core)
                    .expect("running member missing");
                assert_eq!(
                    found,
                    MemberProbe {
                        member: probe.core,
                        offset: probe.offset,
                        failed_at: vec![off(-30)],
                        passed_at: Vec::new(),
                        carried: Vec::new(),
                        running: true,
                        done: false,
                    }
                );
                return;
            }
            continue;
        }
        match action {
            Action::RunTrial(plan) => {
                let fail = at(&plan, 0) <= -27 && at(&plan, 1) <= -27;
                j.trial(
                    &plan,
                    if fail {
                        Outcome::Failure
                    } else {
                        Outcome::Pass
                    },
                );
                if fail {
                    attribute(&mut j);
                }
            }
            Action::Decide(decision) => {
                assert!(
                    !matches!(decision.payload, DecisionPayload::HuntEnd(_)),
                    "hunt stopped before probes: {decision:?}"
                );
                j.decide(decision);
            }
            other @ Action::ReadRanking => panic!("{other:?}"),
        }
    }
    panic!("hunt never probed a member");
}

pub(crate) fn probe_without_checking_profile() -> Journal {
    let mut j = Journal::new(&[0, 1], &[Start::has_room(-20), Start::has_room(-10)]);
    assert_eq!(j.run().checking.profile(), None);
    let start = HuntStart {
        hunt: NonZeroU32::MIN,
        failure: Seq::new(1).unwrap(),
        trial: None,
        regime: Regime::R6,
        workload: workloads(Regime::R6)[0].id(),
        cores: set(&[0, 1]),
        duration_s: SHORT,
        failing: j.profile(&[-20, -10]).offsets(),
        parked: j.profile(&[-10, -5]).offsets(),
        parked_seq: None,
        candidates: set(&[0, 1]),
        trials: 5,
        trial_s: SHORT,
        miss: 0.0,
        rate: 0.0,
        ranking: vec![core(0), core(1)],
        reason: None,
    };
    j.append(start.into(), &[]);
    let probe = Member {
        core: core(0),
        offset: off(-15),
    };
    let held = Member {
        core: core(1),
        offset: off(-8),
    };
    let mut payload = group_payload(1, 1, HuntStage::Probe, &[0, 1], 2, 0, SHORT);
    payload.cores = set(&[0, 1]);
    payload.profile = j.profile(&[-15, -8]).offsets();
    payload.probe = Some(probe);
    payload.held = vec![held];
    j.append(payload.into(), &[]);
    let mut plan = j.manual(
        Regime::R6,
        0,
        Load::Many(set(&[0, 1])),
        Condition::Parked,
        &[-15, -8],
        SHORT,
    );
    plan.requirement.context = TrialContext::Hunt {
        hunt: NonZeroU32::MIN,
        group: NonZeroU32::MIN,
    };
    plan.activity = Activity::Hunt;
    j.intent(&plan);
    j
}

#[test]
fn hunt_probe_without_recorded_checking_profile() {
    let j = probe_without_checking_profile();
    let probe = Member {
        core: core(0),
        offset: off(-15),
    };
    let held = Member {
        core: core(1),
        offset: off(-8),
    };
    let view = j.state.hunt_plan().unwrap();
    assert_eq!(view.probes.len(), 2, "{view:?}");
    assert!(
        view.probes[0].running && view.probes[0].offset == off(-15),
        "{view:?}"
    );
    assert_eq!(view.groups[0].probe, Some(probe));
    assert_eq!(view.groups[0].held, vec![held]);
    j.assert_resumes();
}

#[test]
fn active_hunt_projection() {
    for case in projection_cases() {
        let (mut j, _) = case.journal();
        let outcome = match case.outcome {
            None => GroupState::Running,
            Some(GroupOutcome::Passed) => GroupState::Passed,
            Some(GroupOutcome::Failed) => GroupState::Failed,
            Some(GroupOutcome::Skipped) => GroupState::Skipped,
        };
        let plan = j
            .state
            .hunt_plan()
            .unwrap_or_else(|| panic!("{}", case.name));
        assert_eq!(plan.number, 2, "{}", case.name);
        assert_eq!(plan.candidates, set(&ids(4)), "{}", case.name);
        assert_eq!(plan.groups.len(), 1, "{}", case.name);
        let view = &plan.groups[0];
        assert_eq!(view.number, 1, "{}", case.name);
        assert_eq!(view.cores, set(&[0, 1]), "{}", case.name);
        assert_eq!(view.stage, case.stage, "{}", case.name);
        assert_eq!(view.probe, case.probe, "{}", case.name);
        assert_eq!(view.held, case.held, "{}", case.name);
        assert_eq!(view.needed, 5, "{}", case.name);
        assert_eq!(view.passed, case.passes, "{}", case.name);
        assert_eq!(view.outcome, outcome, "{}", case.name);
        assert!(!view.carried, "{}", case.name);
        j.assert_resumes();
        cancel_hunt(&mut j, 2);
        assert_eq!(j.state.hunt_plan(), None, "{}", case.name);
        j.assert_resumes();
    }
}

fn reach_group(j: &mut Journal) -> DecisionAction {
    for _ in 0..200 {
        let action = j.next();
        if group_of(&action).is_some_and(|group| group.stage == HuntStage::Part && group.index == 0)
        {
            let Action::Decide(decision) = action else {
                unreachable!();
            };
            return decision;
        }
        j.step(&mut |_| Outcome::Pass);
    }
    panic!("the hunt never planned a part group");
}

#[test]
fn carried_evidence_consumers() {
    let mut j = Journal::new(&[0], &[Start::at_limit(-20)]);
    j.profile_change(&[-20]);
    let secs = j.run().config.checking_trial.get();
    let needed = j.run().config.needed.get();
    for _ in 0..needed {
        j.carry(
            "20261002T004254Z",
            (Regime::R1, 0, &[0], secs),
            Condition::Together,
            &[-20],
            Outcome::Pass,
        );
    }
    j.cycle_start(1, &[Regime::R1]);
    let projection = j.state.project();
    let view = projection.cycle_plan;
    assert!(
        view.steps[0]
            .parts
            .iter()
            .all(|part| part.passed == 0 && part.failed == 0 && !part.done)
            && view.current == 0
            && view.open,
        "the cycle credited carried passes: {view:?}"
    );
    assert_eq!(projection.requirement, None);
    assert_eq!(projection.trial_history, None);
    let mut guard = 0;
    loop {
        match j.next() {
            Action::RunTrial(plan) => {
                assert_eq!(plan.regime, Regime::R1);
                j.trial(&plan, Outcome::Pass);
            }
            Action::Decide(decision) => {
                if matches!(&decision.payload, DecisionPayload::CheckingCycle(cycle) if cycle.passed)
                {
                    break;
                }
                j.decide(decision);
            }
            other @ Action::ReadRanking => panic!("{other:?}"),
        }
        guard += 1;
        assert!(guard < 50, "the live passes never finished the cycle");
    }
    let view = j.state.project().cycle_plan;
    assert_eq!((view.steps[0].done, view.current), (true, 1));
    j.assert_resumes();
}

#[test]
fn stored_requirement_survives_crash_and_changed_config() {
    let mut j = Journal::new(&[0], &[Start::search(-20).checking()]);
    let needed = u32::from(j.run().config.needed.get());
    for _ in 0..needed - 1 {
        let plan = j.next_trial();
        j.trial(&plan, Outcome::Pass);
    }
    let plan = j.next_trial();
    let (intent, trial) = j.intent(&plan);
    let want = TrialRequirement {
        passed: needed - 1,
        failed: 0,
        trial: needed,
        needed,
    };
    assert_eq!(j.state.requirement(), Some(want));
    let search = j.run().config.search_trial.get() + 1;
    j.load(&config(&format!(
        r#"{{"evidence":{{"miss":0.001}},"durations":{{"search_trial_s":{search}}}}}"#
    )));
    assert_eq!(
        j.state.requirement(),
        Some(want),
        "config reload reinterpreted the running requirement"
    );
    j.end(intent, &trial, Outcome::Failure);
    let history = TrialHistory {
        passed: needed - 1,
        trial: needed,
        needed,
        step: None,
    };
    assert_eq!(j.state.trial_history(), Some(history));
    assert_eq!(fresh(&j.events).trial_history(), Some(history));
    assert_eq!(j.state.requirement(), None);
    j.reset(0);
    assert_eq!(
        j.state.trial_history(),
        Some(history),
        "reset erased historical counts"
    );
}

#[test]
fn dashboard_views_visits_bounded() {
    fn visits(passes: usize) -> u64 {
        let mut j = chain_journal();
        let plan = cycle_plan_of(&j, 1, &[0, 1, 2, 3], SHORT);
        for _ in 0..passes {
            j.trial(&plan, Outcome::Pass);
        }
        j.state.visits();
        let _ = j.state.cycle_plan();
        let _ = j.state.search_turns();
        let _ = j.state.requirement();
        let _ = j.state.trial_history();
        let _ = j.state.hunt_plan();
        let _ = j.state.core_limit(core(0));
        j.state.visits()
    }
    fn confirmation_visits(passes: usize) -> u64 {
        let mut j = Journal::new(&[0], &[Start::search(-20).checking()]);
        let plan = j.next_trial();
        for _ in 0..passes {
            j.trial(&plan, Outcome::Pass);
        }
        j.intent(&plan);
        j.state.visits();
        let _ = j.state.search_turns();
        let _ = j.state.requirement().unwrap();
        j.state.visits()
    }
    fn hunt_visits(passes: usize) -> u64 {
        let mut j = limit_session(4);
        start_hunt_at(&mut j, 1, &[0, 1, 2, 3], &[-30; 4]);
        let decision = reach_group(&mut j);
        j.decide(decision);
        let plan = j.next_trial();
        for _ in 0..passes {
            j.trial(&plan, Outcome::Pass);
        }
        j.intent(&plan);
        j.state.visits();
        let _ = j.state.hunt_plan().unwrap();
        let _ = j.state.requirement().unwrap();
        j.state.visits()
    }
    let small = visits(10);
    let large = visits(1000);
    assert_eq!(small, large, "views visited more cells as passes grew");
    assert_eq!(confirmation_visits(10), confirmation_visits(1000));
    assert_eq!(hunt_visits(10), hunt_visits(1000));
}

#[test]
fn search_turns_order_and_confirmation() {
    let mut j = Journal::search_at(&[-20, -20, -20]);
    let turns = j.state.search_turns();
    assert_eq!(
        turns.iter().map(|turn| turn.core).collect::<Vec<_>>(),
        [core(0), core(1), core(2)]
    );
    assert!(turns.iter().all(|turn| turn.kind
        == TurnKind::Search {
            step: NonZeroU8::new(5).unwrap()
        }));
    let plan = j.next_trial();
    j.intent(&plan);
    let running: Vec<_> = j
        .state
        .search_turns()
        .into_iter()
        .filter(|t| t.running)
        .collect();
    assert_eq!(running.len(), 1, "{running:?}");
    assert_eq!(running[0].core, core(0));
    j.assert_resumes();
}

#[test]
fn views_are_empty_without_a_session_or_after_poisoning() {
    fn empty(state: &crate::State) {
        assert!(state.cycle_plan().steps.is_empty());
        assert!(state.search_turns().is_empty());
        assert_eq!(state.requirement(), None);
        assert_eq!(state.trial_history(), None);
        assert_eq!(state.hunt_plan(), None);
        assert_eq!(state.core_limit(core(0)), None);
        assert_eq!(state.forecast(), crate::Forecast::default());
    }
    empty(&crate::State::new());
    let mut j = Journal::search_at(&[-20]);
    let plan = j.next_trial();
    j.trial(&plan, Outcome::Pass);
    let stale = j.events.last().unwrap().clone();
    assert!(matches!(
        j.state.fold(&stale),
        Err(crate::FoldError::OutOfOrder { .. })
    ));
    assert!(j.state.next().is_err());
    empty(&j.state);
}

#[test]
fn dashboard_views_serialize_roundtrip() {
    fn roundtrip<T>(value: &T)
    where
        T: std::fmt::Debug + PartialEq + serde::Serialize + serde::de::DeserializeOwned,
    {
        let encoded = serde_json::to_string(value).unwrap();
        let decoded: T = serde_json::from_str(&encoded).unwrap();
        assert_eq!(*value, decoded);
    }
    let cycle = chain_journal();
    roundtrip(&cycle.state.cycle_plan());
    let mut search = Journal::new(&[0], &[Start::search(-20).checking()]);
    let plan = search.next_trial();
    let (intent, trial) = search.intent(&plan);
    roundtrip(&search.state.search_turns());
    roundtrip(&search.state.requirement().unwrap());
    search.end(intent, &trial, Outcome::Pass);
    roundtrip(&search.state.trial_history().unwrap());
    let hunt = hunt_of(4, SHORT);
    roundtrip(&hunt.state.hunt_plan().unwrap());
    roundtrip(&Limit::Reached(Reached::FailurePoint {
        core: core(0),
        point: FailurePoint::new(off(-21)),
    }));
}

#[test]
fn dashboard_views_are_read_only() {
    let journals = vec![
        chain_journal(),
        Journal::new(&[0], &[Start::search(-20).checking()]),
        hunt_of(4, SHORT),
    ];
    for j in journals {
        let before = j.state.clone();
        for _ in 0..2 {
            let _ = j.state.cycle_plan();
            let _ = j.state.search_turns();
            let _ = j.state.requirement();
            let _ = j.state.trial_history();
            let _ = j.state.hunt_plan();
            let _ = j.state.core_limit(core(0));
        }
        assert_eq!(before, j.state);
    }
}

#[test]
fn stored_requirement_separates_class_count_and_part() {
    let mut j = chain_journal();
    pass_part(&mut j, &[0, 1, 2, 3], &FULL_REQUESTS);
    decide_next(&mut j);
    let plan = j.next_trial();
    assert!(matches!(
        plan.requirement.context,
        TrialContext::Cycle { step, part, .. } if step.get().get() == 1 && part.get().get() == 2
    ));
    let (intent, trial) = j.intent(&plan);
    assert_eq!(
        j.state.project().requirement,
        Some(TrialRequirement {
            passed: 0,
            failed: 0,
            trial: 1,
            needed: 3,
        })
    );
    j.end(intent, &trial, Outcome::Inconclusive);
    let projection = j.state.project();
    assert_eq!(
        projection.trial_history,
        Some(TrialHistory {
            passed: 0,
            trial: 1,
            needed: 3,
            step: Some(crate::StepNo::new(std::num::NonZeroU32::MIN)),
        })
    );
    let part = &projection.cycle_plan.steps[0].parts[1];
    let (short, long) = part.counts();
    assert_eq!(short + long, 4, "{part:?}");
    assert_eq!(part.passed + part.failed + 1, 1, "{part:?}");
    assert!(!part.done && !part.running, "{part:?}");
    assert_eq!(fresh(&j.events).project(), projection);
}

#[test]
fn project_assembles_the_active_hunt() {
    let mut j = hunt_of(4, SHORT);
    let opened = j.state.project();
    let plan = opened.hunt_plan.as_ref().expect("an active hunt");
    assert_eq!((plan.number, plan.regime), (1, Regime::R6));
    assert_eq!(plan.candidates, set(&[0, 1, 2, 3]));
    let failed_trial = plan.trial.clone();
    assert!(failed_trial.is_some(), "the hunt names the failure's trial");
    assert!(opened.cycle_plan.paused);
    assert_eq!(opened.cores.len(), 4);
    assert_eq!(opened.deepening, None);
    assert_eq!(opened.requirement, None);
    let decision = reach_group(&mut j);
    j.decide(decision);
    let group_plan = j.next_trial();
    let (intent, trial) = j.intent(&group_plan);
    let running = j.state.project();
    let hunt = running.hunt_plan.as_ref().unwrap();
    assert_eq!(hunt.trial, failed_trial);
    assert!(!hunt.groups.is_empty());
    assert_eq!(
        running.requirement,
        Some(TrialRequirement {
            passed: 0,
            failed: 0,
            trial: 1,
            needed: u32::from(group_plan.requirement.needed.get()),
        })
    );
    j.end(intent, &trial, Outcome::Pass);
    let ended = j.state.project();
    assert_eq!(ended.requirement, None);
    let history = ended.trial_history.unwrap();
    assert_eq!(
        (history.passed, history.trial, history.needed),
        (0, 1, u32::from(group_plan.requirement.needed.get()))
    );
    assert_eq!(ended.hunt_plan.as_ref().unwrap().trial, failed_trial);
    assert_eq!(fresh(&j.events).project(), ended);
    cancel_hunt(&mut j, 1);
    let closed = j.state.project();
    assert_eq!(closed.hunt_plan, None);
    assert!(!closed.cycle_plan.paused);
}

#[test]
fn crash_recovery_keeps_the_running_part() {
    let ids = ids(8);
    let starts: Vec<Start> = STARTS
        .iter()
        .map(|&offset| Start::has_room(offset))
        .collect();
    let mut j = Journal::on(&ids, two_ccds(), &starts);
    j.ranking(&ids);
    decide_next(&mut j);
    j.cycle_start(1, &[Regime::R1]);
    let plan = trial_after_decisions(&mut j);
    assert_eq!(plan.regime, Regime::R1);
    let (intent, trial) = j.intent(&plan);
    let running = |j: &Journal| {
        let projection = j.state.project();
        assert_eq!(fresh(&j.events).project(), projection);
        projection.cycle_plan.steps[0]
            .parts
            .iter()
            .filter(|part| part.running)
            .count()
    };
    assert_eq!(running(&j), 1, "the trial's part is not running");
    j.append(
        togi_journal::CrashDetected {
            previous_boot: togi_machine::BootId::from("b".to_owned()),
            in_flight: Some(intent),
            stray: false,
            condition: None,
            reset_reason: None,
            reset_reason_raw: Vec::new(),
            inconclusive: false,
            pstore: None,
        }
        .into(),
        &[],
    );
    assert_eq!(
        running(&j),
        1,
        "crash.detected dropped the running part before recovery closed the trial"
    );
    let end = Journal::end_payload(&trial, Outcome::Failure, Some(togi_machine::Signal::Crash));
    j.append(end.into(), &[intent]);
    assert_eq!(running(&j), 0, "the part still runs after its trial ended");
}

#[test]
fn config_reload_keeps_the_running_parts_stored_requirement() {
    let ids = ids(8);
    let starts: Vec<Start> = STARTS
        .iter()
        .map(|&offset| Start::has_room(offset))
        .collect();
    let mut j = Journal::on(&ids, two_ccds(), &starts);
    j.ranking(&ids);
    decide_next(&mut j);
    j.cycle_start(1, &[Regime::R1]);
    let plan = trial_after_decisions(&mut j);
    let (intent, trial) = j.intent(&plan);
    let running = |j: &Journal| -> Option<(usize, CyclePart)> {
        let projection = j.state.project();
        assert_eq!(fresh(&j.events).project(), projection);
        let parts = &projection.cycle_plan.steps[0].parts;
        parts
            .iter()
            .position(|part| part.running)
            .map(|at| (at, parts[at].clone()))
    };
    let (at, stored) = running(&j).expect("the trial's part is not running");
    let secs = j.run().config.checking_trial.get();
    assert!(
        short_secs(&stored) == secs && stored.counts() == (1, 0) && stored.passed == 0,
        "{stored:?}"
    );
    let changed = secs + 1;
    j.load(&config(&format!(
        r#"{{"durations":{{"checking_trial_s":{changed}}}}}"#
    )));
    assert_eq!(
        running(&j),
        Some((at, stored.clone())),
        "config reload reinterpreted the running part"
    );
    j.append(
        togi_journal::CrashDetected {
            previous_boot: togi_machine::BootId::from("b".to_owned()),
            in_flight: Some(intent),
            stray: false,
            condition: None,
            reset_reason: None,
            reset_reason_raw: Vec::new(),
            inconclusive: false,
            pstore: None,
        }
        .into(),
        &[],
    );
    assert_eq!(
        running(&j),
        Some((at, stored)),
        "crash.detected reinterpreted the running part"
    );
    let end = Journal::end_payload(&trial, Outcome::Failure, Some(togi_machine::Signal::Crash));
    j.append(end.into(), &[intent]);
    assert_eq!(
        running(&j),
        None,
        "the part still runs after its trial ended"
    );
    let rebuilt = &parts(&j, 0)[at];
    assert!(
        short_secs(rebuilt) == changed && rebuilt.passed == 0,
        "the ended part kept the old requirement: {rebuilt:?}"
    );
}

#[test]
fn search_turns_keep_an_unfinished_second_regime_first_then_rotate_past_the_cursor() {
    let mut j = Journal::search_at(&[-20, -20, -20]);
    let turns = |j: &Journal| -> Vec<(u8, Vec<Regime>, bool)> {
        j.state
            .search_turns()
            .iter()
            .map(|turn| (turn.core.get(), turn.regimes.clone(), turn.running))
            .collect()
    };
    let both = vec![Regime::R1, Regime::R2];
    let first = j.next_trial();
    assert_eq!(
        (first.regime, first.load),
        (Regime::R1, Load::Single(core(0)))
    );
    let (intent, trial) = j.intent(&first);
    assert_eq!(
        turns(&j),
        [
            (1, both.clone(), false),
            (2, both.clone(), false),
            (0, both.clone(), true)
        ]
    );
    j.end(intent, &trial, Outcome::Pass);
    assert_eq!(
        turns(&j),
        [
            (0, vec![Regime::R2], false),
            (1, both.clone(), false),
            (2, both.clone(), false)
        ]
    );
    j.assert_resumes();
    let second = j.next_trial();
    assert_eq!(
        (second.regime, second.load),
        (Regime::R2, Load::Single(core(0)))
    );
    let (intent, trial) = j.intent(&second);
    assert_eq!(turns(&j)[0], (0, vec![Regime::R2], true));
    j.end(intent, &trial, Outcome::Pass);
    let rotated = [
        (1, both.clone(), false),
        (2, both.clone(), false),
        (0, both.clone(), false),
    ];
    assert_eq!(turns(&j), rotated);
    j.assert_resumes();
    decide_next(&mut j);
    assert_eq!(turns(&j), rotated);
    j.assert_resumes();
}

#[test_case(0, false, &[0, 8, 1, 9], 0, Regime::R1; "initial alternating order")]
#[test_case(1, false, &[0, 8, 1, 9], 0, Regime::R2; "current R2 priority")]
#[test_case(2, false, &[8, 1, 9, 0], 8, Regime::R1; "completed turn rotates")]
#[test_case(0, true, &[0, 8, 9], 0, Regime::R1; "skip checking core")]
#[test_case(1, true, &[0, 8, 9], 0, Regime::R2; "R2 before other CCD")]
#[test_case(2, true, &[8, 9, 0], 8, Regime::R1; "rotation skips checking core")]
fn scheduler_and_projection_share_search_order(
    completed: u8,
    skip_one: bool,
    expected: &[u8],
    next_core: u8,
    next_regime: Regime,
) {
    let mut starts = [Start::search(-20); 4];
    if skip_one {
        starts[1] = Start::has_room(-20);
    }
    let mut j = Journal::new(&[0, 1, 8, 9], &starts);
    for _ in 0..completed {
        let plan = j.next_trial();
        j.trial(&plan, Outcome::Pass);
    }
    if completed == 2 {
        decide_next(&mut j);
    }
    let turns = j.state.search_turns();
    assert_eq!(
        turns.iter().map(|turn| turn.core.get()).collect::<Vec<_>>(),
        expected
    );
    let plan = j.next_trial();
    assert_eq!(
        (plan.load, plan.regime),
        (Load::Single(core(next_core)), next_regime)
    );
    assert_eq!(turns[0].core, core(next_core));
    assert_eq!(
        turns[0].regimes,
        if next_regime == Regime::R2 {
            vec![Regime::R2]
        } else {
            vec![Regime::R1, Regime::R2]
        }
    );
    j.assert_resumes();
}

#[test]
fn sparse_search_keeps_holes_and_the_unfinished_second_regime() {
    let mut j = Journal::new(&[2, 15], &[Start::search(-20); 2]);
    let first = j.next_trial();
    assert_eq!(
        (first.load, first.regime),
        (Load::Single(core(2)), Regime::R1)
    );
    j.trial(&first, Outcome::Pass);
    let second = j.next_trial();
    assert_eq!(
        (second.load, second.regime),
        (Load::Single(core(2)), Regime::R2)
    );
    assert_eq!(
        j.state
            .search_turns()
            .iter()
            .map(|turn| turn.core)
            .collect::<Vec<_>>(),
        [core(2), core(15)]
    );
    j.assert_resumes();
}

#[test]
fn absent_lengths_and_confirmation_turns_keep_their_flat_state_shape() {
    let short_only = part(&[0, 1], Some(0), true, 600, (0, 0));
    let json = serde_json::to_value(&short_only).unwrap();
    assert_eq!(
        (
            &json["short"],
            &json["short_s"],
            &json["long"],
            &json["long_s"]
        ),
        (&3.into(), &600.into(), &0.into(), &0.into())
    );
    assert_eq!(
        serde_json::from_value::<CyclePart>(json).unwrap(),
        short_only
    );
    let none = CyclePart {
        lengths: None,
        ..short_only
    };
    let json = serde_json::to_value(&none).unwrap();
    assert_eq!((&json["short"], &json["short_s"]), (&0.into(), &0.into()));
    assert_eq!(serde_json::from_value::<CyclePart>(json).unwrap(), none);

    let (json, turn) = flat_turn(TurnKind::Confirm);
    assert_eq!(
        (json["confirm"].as_bool(), json["step"].as_u64()),
        (Some(true), Some(0))
    );
    assert_eq!(
        serde_json::from_value::<crate::SearchTurn>(json.clone()).unwrap(),
        turn
    );
    let mut stepped = json;
    stepped["step"] = 5.into();
    assert!(serde_json::from_value::<crate::SearchTurn>(stepped).is_err());
}

#[test]
fn search_turns_keep_their_flat_step_and_refuse_step_zero() {
    let (json, turn) = flat_turn(TurnKind::Search {
        step: NonZeroU8::MIN,
    });
    assert_eq!(
        (json["confirm"].as_bool(), json["step"].as_u64()),
        (Some(false), Some(1))
    );
    assert_eq!(
        serde_json::from_value::<crate::SearchTurn>(json.clone()).unwrap(),
        turn
    );
    let mut stepless = json;
    stepless["step"] = 0.into();
    assert!(serde_json::from_value::<crate::SearchTurn>(stepless).is_err());
}

fn flat_turn(kind: TurnKind) -> (serde_json::Value, crate::SearchTurn) {
    let j = Journal::search_at(&[-20, -20, -20]);
    let turn = crate::SearchTurn {
        kind,
        ..j.state.search_turns()[0].clone()
    };
    (serde_json::to_value(&turn).unwrap(), turn)
}

fn rerun(long: Option<Quota>) -> crate::RerunPlan {
    crate::RerunPlan {
        regime: Regime::R2,
        cores: set(&[0]),
        lengths: Lengths {
            short: Quota {
                count: 3,
                secs: 600,
            },
            long,
        },
    }
}

fn flat_rerun(plan: &crate::RerunPlan) -> serde_json::Value {
    let json = serde_json::to_value(plan).unwrap();
    let keys: Vec<&str> = json
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        ["cores", "long", "long_s", "regime", "short", "short_s"],
        "{json}"
    );
    assert_eq!(
        &serde_json::from_value::<crate::RerunPlan>(json.clone()).unwrap(),
        plan
    );
    json
}

#[test]
fn a_rerun_without_a_long_trial_keeps_its_flat_state_shape() {
    let json = flat_rerun(&rerun(None));
    assert_eq!(
        (
            &json["short"],
            &json["short_s"],
            &json["long"],
            &json["long_s"]
        ),
        (&3.into(), &600.into(), &0.into(), &0.into())
    );
}

#[test]
fn a_rerun_with_a_long_trial_keeps_its_flat_state_shape() {
    let json = flat_rerun(&rerun(Some(Quota {
        count: 1,
        secs: 1200,
    })));
    assert_eq!(
        (
            &json["short"],
            &json["short_s"],
            &json["long"],
            &json["long_s"]
        ),
        (&3.into(), &600.into(), &1.into(), &1200.into())
    );
}

#[test]
fn a_rerun_without_a_short_trial_is_refused() {
    let mut json = serde_json::to_value(rerun(None)).unwrap();
    json["short"] = 0.into();
    json["short_s"] = 0.into();
    assert!(serde_json::from_value::<crate::RerunPlan>(json).is_err());
}
