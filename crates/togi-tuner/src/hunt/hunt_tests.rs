use togi_journal::{
    Activity, Attribution, CorePhase, CoreTransition, Decision, HuntEnd, HuntGroup, HuntOutcome,
    HuntStage, HuntStart, Inference, Member, Outcome, Seq, TunerDecision,
};
use togi_machine::{Condition, Regime, workloads};

use super::plan::GroupOutcome;
use super::support::{ended, passed_cycle};
use crate::later_tests::{pass_cycles, start_cycle, strikes};
use crate::testkit::{Journal, Start, config, set};
use crate::tests::{core, off};
use crate::{Action, DecisionPayload, Load, TrialContext, TrialPlan};

const SHORT: u32 = 120;

fn ids(count: usize) -> Vec<u8> {
    (0..count).map(|id| u8::try_from(id).unwrap()).collect()
}

fn limit_session(count: usize) -> Journal {
    let ids = ids(count);
    let starts = vec![Start::at_limit(-30).fail(-31); count];
    let mut j = Journal::new(&ids, &starts);
    j.profile_change(&vec![-30; count]);
    j.ranking(&ids);
    j
}

fn current(j: &Journal) -> Vec<i64> {
    j.offsets().iter().map(|(_, offset)| offset.get()).collect()
}

fn idle_config(j: &mut Journal, secs: u32) {
    if j.run().config.checking_idle.get() != secs {
        j.load(&config(&format!(
            r#"{{"durations":{{"checking_idle_s":{secs}}}}}"#
        )));
    }
}

fn together_plan(j: &mut Journal, loaded: &[u8], profile: &[i64], duration: u32) -> TrialPlan {
    if loaded.len() == current(j).len() {
        idle_config(j, duration);
        return j.manual(
            Regime::R6,
            0,
            Load::Many(set(loaded)),
            Condition::Together,
            profile,
            duration,
        );
    }
    assert_eq!(
        loaded.len(),
        1,
        "the cycle schedules one core or all of them"
    );
    j.manual(
        Regime::R3,
        0,
        Load::Single(core(loaded[0])),
        Condition::Together,
        profile,
        j.run().config.checking_trial.get(),
    )
}

fn attribute(j: &mut Journal) -> Seq {
    let decision = j.next_decision();
    assert!(
        matches!(&decision.payload, DecisionPayload::Failure(f) if f.attribution == Attribution::Unattributed),
        "{decision:?}"
    );
    j.decide(decision)
}

fn fail_together(j: &mut Journal, loaded: &[u8], profile: &[i64], duration: u32) -> Seq {
    let plan = together_plan(j, loaded, profile, duration);
    j.trial(&plan, Outcome::Failure);
    attribute(j)
}

fn fail_all_together(j: &mut Journal, duration: u32) -> Seq {
    let all = ids(current(j).len());
    let profile = current(j);
    fail_together(j, &all, &profile, duration)
}

fn hunt_of(count: usize, duration: u32) -> Journal {
    let mut j = limit_session(count);
    fail_all_together(&mut j, duration);
    j.start_hunt();
    j
}

fn group_of(action: &Action) -> Option<&HuntGroup> {
    match action {
        Action::Decide(decision) => match &decision.payload {
            DecisionPayload::HuntGroup(group) => Some(group),
            _ => None,
        },
        _ => None,
    }
}

fn end_of(action: &Action) -> Option<&HuntEnd> {
    match action {
        Action::Decide(decision) => match &decision.payload {
            DecisionPayload::HuntEnd(end) => Some(end),
            _ => None,
        },
        _ => None,
    }
}

fn at(plan: &TrialPlan, id: u8) -> i64 {
    plan.profile.get(core(id)).unwrap().get()
}

fn core_ids(cores: togi_machine::CoreSet) -> Vec<u8> {
    cores.iter().map(togi_machine::CoreId::get).collect()
}

fn run_group(j: &mut Journal, fail: bool) {
    let plan = j.next_trial();
    assert_eq!(plan.condition, Condition::Parked, "{plan:?}");
    let outcome = if fail {
        Outcome::Failure
    } else {
        Outcome::Pass
    };
    j.trial(&plan, outcome);
    if fail {
        attribute(j);
    }
}

fn pass_all(_: &TrialPlan) -> Outcome {
    Outcome::Pass
}

fn start_regime_hunt(
    j: &mut Journal,
    hunt: u32,
    regime: Regime,
    loaded: &[u8],
    failing: &[i64],
) -> Seq {
    let count = failing.len();
    let start = HuntStart {
        hunt: std::num::NonZeroU32::new(hunt).unwrap(),
        failure: Seq::new(1).unwrap(),
        trial: None,
        regime,
        workload: workloads(regime)[0].id(),
        cores: set(loaded),
        duration_s: SHORT,
        failing: j.profile(failing).offsets(),
        parked: j.profile(&vec![0; count]).offsets(),
        parked_seq: None,
        candidates: set(&ids(count)),
        trials: 5,
        trial_s: SHORT,
        miss: 0.0,
        rate: 0.0,
        ranking: ids(count).into_iter().map(core).collect(),
        reason: None,
    };
    j.append(start.into(), &[])
}

fn start_hunt_at(j: &mut Journal, hunt: u32, loaded: &[u8], failing: &[i64]) -> Seq {
    start_regime_hunt(j, hunt, Regime::R6, loaded, failing)
}

fn cancel_hunt(j: &mut Journal, hunt: u32) {
    let end = HuntEnd {
        hunt: std::num::NonZeroU32::new(hunt).unwrap(),
        result: HuntOutcome::Cancelled,
        cores: set(&[]),
        members: Vec::new(),
        groups: 0,
        reason: "test".to_owned(),
    };
    j.append(end.into(), &[]);
}

fn ran_group(
    j: &mut Journal,
    hunt: u32,
    loaded: &[u8],
    failing: &[i64],
    stage: HuntStage,
    outcome: Outcome,
    trials: usize,
) {
    start_hunt_at(j, hunt, loaded, failing);
    let planned = reach(j, stage);
    j.decide(planned);
    for _ in 0..trials {
        run_group(j, outcome == Outcome::Failure);
    }
    cancel_hunt(j, hunt);
}

fn reach(j: &mut Journal, stage: HuntStage) -> crate::DecisionAction {
    let index = u32::from(stage == HuntStage::Complement);
    for _ in 0..200 {
        let action = j.next();
        if group_of(&action).is_some_and(|group| group.stage == stage && group.index == index) {
            let Action::Decide(decision) = action else {
                unreachable!();
            };
            return decision;
        }
        j.step(&mut pass_all);
    }
    panic!("the hunt never planned a {stage:?} group");
}

fn reuse_session() -> Journal {
    let mut j = Journal::new(&ids(3), &[Start::has_room(-30); 3]);
    j.profile_change(&[-30; 3]);
    j.ranking(&ids(3));
    j
}

const REUSE_LOADED: [u8; 2] = [1, 2];

fn reset_core_zero(j: &mut Journal) {
    j.reset(0);
    let transition = j.next_decision();
    assert!(matches!(&transition.payload, DecisionPayload::CorePhase(_)));
    j.decide(transition);
}

fn group_in(decision: &crate::DecisionAction) -> &HuntGroup {
    match &decision.payload {
        DecisionPayload::HuntGroup(group) => group,
        other => panic!("expected a hunt group, got {other:?}"),
    }
}

#[test]
fn hunt_parked_offsets_raises_the_passed_full_cycle_profile() {
    struct Case {
        name: &'static str,
        passed: [i64; 4],
        failing: [i64; 4],
        parked: [i64; 4],
        from_cycle: bool,
        candidates: &'static [u8],
    }
    let cases = [
        Case {
            name: "shallower everywhere",
            passed: [-10, -10, -10, -10],
            failing: [-12, -10, -10, -10],
            parked: [-10, -10, -10, -10],
            from_cycle: true,
            candidates: &[0],
        },
        Case {
            name: "a yielded core takes its failing offset",
            passed: [-10, -10, -10, -10],
            failing: [-12, -8, -10, -10],
            parked: [-10, -8, -10, -10],
            from_cycle: true,
            candidates: &[0],
        },
        Case {
            name: "deeper everywhere falls back to all-zero",
            passed: [-12, -12, -12, -12],
            failing: [-10, -10, -10, -10],
            parked: [0, 0, 0, 0],
            from_cycle: false,
            candidates: &[0, 1, 2, 3],
        },
    ];
    for case in cases {
        let starts: Vec<Start> = case.failing.iter().map(|&o| Start::has_room(o)).collect();
        let mut j = Journal::new(&ids(4), &starts);
        j.ranking(&ids(4));
        let cycle = passed_cycle(&mut j, &case.passed, &case.failing);
        fail_together(&mut j, &[0, 1, 2, 3], &case.failing, SHORT);
        let (start, cause) = j.hunt_start_payload();
        assert_eq!(
            start.parked,
            j.profile(&case.parked).offsets(),
            "{}",
            case.name
        );
        assert_eq!(
            start.parked_seq,
            case.from_cycle.then_some(cycle),
            "{}",
            case.name
        );
        assert_eq!(core_ids(start.candidates), case.candidates, "{}", case.name);
        assert_eq!(
            cause.len(),
            1 + usize::from(case.from_cycle),
            "{}",
            case.name
        );
        j.assert_resumes();
    }
}

#[test]
fn a_hunt_start_reads_a_ranking_newer_than_the_last_round_or_hunt_plan() {
    let mut j = limit_session(4);
    let round = j.round_start(1, &[-30; 4], &[0], 1);
    assert_eq!(j.run().last_plan_seq, Some(round));
    j.round_end(1, round);
    fail_together(&mut j, &[0], &[-30; 4], SHORT);
    assert_eq!(j.run().hunt_start_action(), Some(Action::ReadRanking));
    j.ranking(&ids(4));
    let (start, _) = j.start_hunt();
    assert_eq!(j.run().last_plan_seq, Some(start));
    assert!(j.run().ranking_stale());
    j.assert_resumes();
}

#[test]
fn phase_2_owes_no_round_while_a_hunt_is_open() {
    let mut j = Journal::new(&[0, 1], &[Start::solo(-10, -11), Start::solo(-10, -11)]);
    j.profile_change(&[-10, -10]);
    j.cycle_start(1, &[Regime::R1]);
    j.cycle_end(1, true, true);
    j.ranking(&ids(2));
    assert!(j.run().round_due());
    fail_together(&mut j, &[0, 1], &[-10, -10], SHORT);
    let cycle = j.run().checking.last_cycle().unwrap().get();
    j.cycle_end(cycle, false, false);
    j.start_hunt();
    assert!(j.run().hunt_open());
    assert!(!j.run().round_due());
    j.assert_resumes();
}

#[test]
fn a_reset_attributes_the_pending_failure_then_cancels_the_hunt_then_closes_the_cycle() {
    let mut j = limit_session(4);
    fail_all_together(&mut j, SHORT);
    j.start_hunt();
    let (group, cause) = j.group_plan();
    j.decide(crate::DecisionAction::new(group, cause));
    let Action::RunTrial(plan) = j.next() else {
        panic!("expected the group trial");
    };
    let (intent, trial) = j.intent(&plan);
    let mut end = Journal::end_payload(&trial, Outcome::Failure, Some(togi_machine::Signal::Crash));
    end.duration_s = 5;
    j.append(end.into(), &[intent]);
    let cycle = j.run().checking.open().expect("the cycle is open").number;
    let reset = j.reset(1);
    let mut kinds = Vec::new();
    loop {
        let decision = j.next_decision();
        let kind = match &decision.payload {
            DecisionPayload::Failure(_) => "attribute",
            DecisionPayload::HuntEnd(end) => {
                assert_eq!(end.result, HuntOutcome::Cancelled);
                assert_eq!(decision.cause, [reset]);
                "cancel hunt"
            }
            DecisionPayload::CheckingCycle(closed) => {
                assert_eq!(closed.cycle, cycle);
                assert!(!closed.passed);
                assert_eq!(decision.cause, [reset]);
                "close cycle"
            }
            DecisionPayload::CorePhase(phase) => {
                assert_eq!((phase.core, phase.to), (core(1), CorePhase::Search));
                assert_eq!(decision.cause, [reset]);
                "search"
            }
            other => panic!("unexpected {other:?}"),
        };
        kinds.push(kind);
        j.decide(decision);
        if kind == "search" {
            break;
        }
    }
    assert_eq!(kinds, ["attribute", "cancel hunt", "close cycle", "search"]);
    assert!(j.run().hunts.active.is_none());
    j.assert_resumes();
}

#[test]
fn hunt_parked_offsets_selection_and_reset() {
    let mut j = limit_session(4);
    fail_together(&mut j, &[0], &[-30; 4], SHORT);
    let (_, start) = j.start_hunt();
    assert_eq!(start.parked_seq, None);
    assert_eq!(start.parked, j.profile(&[0, 0, 0, 0]).offsets());
    let reset = j.reset(1);
    let Action::Decide(decision) = j.next() else {
        panic!("a reset did not cancel the hunt");
    };
    let DecisionPayload::HuntEnd(end) = &decision.payload else {
        panic!("a reset did not cancel the hunt: {decision:?}");
    };
    assert_eq!(end.result, HuntOutcome::Cancelled);
    assert_eq!(decision.cause, [reset]);
    j.decide(decision);
    assert!(j.run().hunts.active.is_none());
    assert_eq!(j.run().hunt_start_action(), Some(Action::ReadRanking));
    j.ranking(&ids(4));
    let Some(Action::Decide(again)) = j.run().hunt_start_action() else {
        panic!("the cancelled source was not requeued");
    };
    let DecisionPayload::HuntStart(restart) = again.payload else {
        panic!("the cancelled source was not requeued");
    };
    assert_eq!(restart.failure, start.failure);
    assert_eq!(restart.regime, Regime::R3);
    assert_eq!(restart.workload, start.workload);
    assert_eq!(core_ids(restart.cores), [0]);
    j.assert_resumes();

    let mut loaded = limit_session(4);
    fail_together(&mut loaded, &[0], &[-30; 4], SHORT);
    loaded.start_hunt();
    loaded.reset(0);
    let end = loaded.next_decision();
    assert!(
        matches!(&end.payload, DecisionPayload::HuntEnd(e) if e.result == HuntOutcome::Cancelled)
    );
    loaded.decide(end);
    loaded.ranking(&ids(4));
    assert_eq!(loaded.run().hunt_start_action(), None);
    loaded.assert_resumes();
}

#[test]
fn all_zero_hunt_parked_offsets_omits_zero_cause() {
    let mut j = limit_session(4);
    let failure = fail_all_together(&mut j, SHORT);
    let (seq, start) = j.start_hunt();
    assert_eq!(start.parked_seq, None);
    let event = j.events.iter().find(|event| event.seq == seq).unwrap();
    assert_eq!(event.cause, [failure]);
    j.assert_resumes();
}

#[test]
fn hunt_parked_offsets_skips_exact_passed_full_cycle_profile() {
    let mut j = Journal::new(&ids(2), &[Start::has_room(-30); 2]);
    j.ranking(&ids(2));
    j.profile_change(&[-20, -20]);
    j.cycle_start(1, &[]);
    let old = j.cycle_end(1, true, true);
    j.profile_change(&[-30, -30]);
    j.cycle_start(2, &[]);
    j.cycle_end(2, true, true);
    let failure = fail_together(&mut j, &[0, 1], &[-30, -30], SHORT);
    let (start, cause) = j.hunt_start_payload();
    assert_eq!(start.failure, failure);
    assert_eq!(start.parked_seq, Some(old));
    assert_eq!(start.parked, j.profile(&[-20, -20]).offsets());
    assert_eq!(cause, [failure, old]);
    j.assert_resumes();
}

#[test]
fn hunt_skips_previously_reached_failure() {
    let mut j = hunt_of(2, SHORT);
    let profile = j.profile(&[-30, -30]).offsets();
    let failure = j.idle_failure(profile, Condition::Together);
    j.run_hunt(100, pass_all);
    let skipped = loop {
        let action = j.next();
        if let Action::Decide(decision) = &action {
            if let DecisionPayload::HuntSkipped(skipped) = &decision.payload {
                break (skipped.clone(), decision.cause.clone());
            }
        }
        j.step(&mut pass_all);
    };
    assert_eq!(skipped.0.failure, failure);
    assert!(
        skipped.0.reason.contains("combination C1"),
        "{:?}",
        skipped.0
    );
    assert_eq!(skipped.1, [failure]);
    j.decide(j.next_decision());
    assert!(j.run().hunts.active.is_none());
    assert_eq!(j.run().hunt_start_action(), None);
    j.assert_resumes();
}

#[test]
fn hunt_loaded_idle_split() {
    let mut j = Journal::new(&ids(4), &[Start::has_room(-30); 4]);
    j.profile_change(&[-30; 4]);
    j.ranking(&ids(4));
    fail_together(&mut j, &[0], &[-30; 4], SHORT);
    j.start_hunt();
    let mut round: Vec<HuntGroup> = Vec::new();
    while round.len() < 2 {
        let action = j.step(&mut pass_all);
        if let Some(group) = group_of(&action) {
            round.push(group.clone());
        }
    }
    let parts: Vec<Vec<u8>> = round.iter().map(|g| core_ids(g.cores)).collect();
    assert_eq!(parts, [vec![0], vec![1, 2, 3]]);
    for (index, group) in round.iter().enumerate() {
        assert_eq!(group.granularity, 2);
        assert_eq!(group.stage, HuntStage::Part);
        assert_eq!(group.index, u32::try_from(index).unwrap());
        assert_eq!(group.set, set(&[0, 1, 2, 3]));
    }
    j.assert_resumes();
}

#[test]
fn next_hunt_reuses_established_part_groups() {
    let mut j = hunt_of(4, SHORT);
    let fails = |plan: &TrialPlan| at(plan, 0) <= -28 && at(plan, 1) <= -28;
    let mut first: Vec<HuntGroup> = Vec::new();
    let mut second: Vec<HuntGroup> = Vec::new();
    let mut hunts = 1;
    let source = j.run().hunts.active.as_ref().unwrap().start.failure;
    for _ in 0..400 {
        let action = j.next();
        match &action {
            Action::Decide(decision) => {
                match &decision.payload {
                    DecisionPayload::HuntStart(_) => hunts += 1,
                    DecisionPayload::HuntGroup(group) if hunts == 1 => first.push(group.clone()),
                    DecisionPayload::HuntGroup(group) => second.push(group.clone()),
                    _ => {}
                }
                j.decide(decision.clone());
            }
            Action::RunTrial(plan) => {
                if let TrialContext::Rerun { failure } = plan.context {
                    assert_eq!(failure, source, "the commitment owes its source a rerun");
                    j.trial(plan, Outcome::Failure);
                    continue;
                }
                if hunts == 2 {
                    break;
                }
                let outcome = if fails(plan) {
                    Outcome::Failure
                } else {
                    Outcome::Pass
                };
                j.trial(plan, outcome);
            }
            Action::ReadRanking => {
                j.ranking(&ids(4));
            }
        }
    }
    assert_eq!(hunts, 2, "a second hunt follows the failed rerun");
    let ran = |group: &HuntGroup| group.inferred.is_none() && !group.skipped;
    let idle = first
        .iter()
        .find(|g| core_ids(g.cores) == [2, 3] && ran(g))
        .expect("the first hunt ran the part of cores 2 and 3");
    let again = second
        .iter()
        .find(|g| core_ids(g.cores) == [2, 3])
        .expect("the second hunt reaches the part of cores 2 and 3");
    assert_eq!(again.inferred, Some(Inference::Pass));
    assert_eq!(again.profile, idle.profile);
    j.assert_resumes();
}

#[test]
fn hunt_group_reuse_after_reset() {
    for stage in [HuntStage::Part, HuntStage::Complement] {
        for (name, pass, after_reset, inferred) in [
            ("old failure", false, false, None),
            ("old pass", true, false, None),
            ("new failure", false, true, Some(Inference::Failure)),
            ("new pass", true, true, Some(Inference::Pass)),
        ] {
            let mut j = reuse_session();
            let (outcome, trials) = if pass {
                (Outcome::Pass, 5)
            } else {
                (Outcome::Failure, 1)
            };
            if after_reset {
                reset_core_zero(&mut j);
            }
            ran_group(&mut j, 1, &REUSE_LOADED, &[-30; 3], stage, outcome, trials);
            if !after_reset {
                reset_core_zero(&mut j);
            }
            start_hunt_at(&mut j, 2, &REUSE_LOADED, &[-30; 3]);
            let decision = reach(&mut j, stage);
            let group = group_in(&decision);
            assert_eq!(group.inferred, inferred, "{stage:?} {name}");
            assert!(!group.skipped, "{stage:?} {name}");
            j.assert_resumes();
        }
    }
}

#[test]
fn running_hunt_group_reuses_earlier_passes() {
    for stage in [HuntStage::Part, HuntStage::Complement] {
        let mut j = reuse_session();
        ran_group(&mut j, 1, &REUSE_LOADED, &[-30; 3], stage, Outcome::Pass, 4);
        start_hunt_at(&mut j, 2, &REUSE_LOADED, &[-30; 3]);
        let decision = reach(&mut j, stage);
        assert_eq!(group_in(&decision).inferred, None, "{stage:?}");
        j.decide(decision);
        let view = j.run().hunt_groups();
        let running = view.last().unwrap();
        assert_eq!((running.passed, running.outcome), (4, None), "{stage:?}");
        assert!(matches!(j.next(), Action::RunTrial(_)), "{stage:?}");

        run_group(&mut j, false);
        let view = j.run().hunt_groups();
        let settled = view.last().unwrap();
        assert_eq!(
            (settled.passed, settled.outcome),
            (5, Some(GroupOutcome::Passed)),
            "{stage:?}"
        );
        assert!(matches!(j.next(), Action::Decide(_)), "{stage:?}");
        j.assert_resumes();
    }
}

#[test]
fn hunt_group_does_not_reuse_shallower_or_incomparable_passes() {
    for (name, failing) in [
        ("shallower", [-30, -29, -30]),
        ("incomparable", [-30, -31, -29]),
    ] {
        let mut j = reuse_session();
        ran_group(
            &mut j,
            1,
            &REUSE_LOADED,
            &failing,
            HuntStage::Part,
            Outcome::Pass,
            5,
        );
        start_hunt_at(&mut j, 2, &REUSE_LOADED, &[-30; 3]);
        let decision = reach(&mut j, HuntStage::Part);
        assert_eq!(group_in(&decision).inferred, None, "{name}");
        j.decide(decision);
        let view = j.run().hunt_groups();
        let group = view.last().unwrap();
        assert_eq!((group.passed, group.outcome), (0, None), "{name}");
        assert!(matches!(j.next(), Action::RunTrial(_)), "{name}");
        j.assert_resumes();
    }
}

#[test]
fn hunt_skips_reached_group() {
    let mut j = hunt_of(4, SHORT);
    j.combination(1, &[(2, -30), (3, -30)]);
    let action = j.next();
    let group = group_of(&action).expect("a group on the newly marked profile");
    assert!(group.skipped, "{group:?}");
    assert_eq!(core_ids(group.cores), [2, 3]);
    let number = group.group;
    let Action::Decide(decision) = action else {
        unreachable!();
    };
    j.decide(decision);
    if let Action::RunTrial(plan) = j.next() {
        assert!(
            !matches!(plan.context, TrialContext::Hunt { group, .. } if group == number),
            "ran the skipped group {plan:?}"
        );
    }
    j.assert_resumes();
}

fn repeated_session() -> Journal {
    let count = 16;
    let mut j = Journal::new(&ids(count), &vec![Start::at_limit(-10).fail(-11); count]);
    j.profile_change(&vec![-10; count]);
    j.ranking(&ids(count));
    j.load(&config(
        r#"{"checking":{"cycle":["R6"]},"durations":{"checking_idle_s":120}}"#,
    ));
    j
}

fn repeated_core_oracle(plan: &TrialPlan) -> Outcome {
    if at(plan, 3) < -8 || (at(plan, 3) < 0 && at(plan, 4) < -9) {
        Outcome::Failure
    } else {
        Outcome::Pass
    }
}

fn active_plan(j: &Journal) -> &super::plan::Plan {
    &j.run().hunts.active.as_ref().unwrap().plan
}

#[test]
fn repeated_parked_core_probe_returns_to_binary_parts_after_pass() {
    use super::plan::GroupStage;
    let mut j = repeated_session();
    let mut prior: Vec<Seq> = Vec::new();
    let mut direct_end: Option<Seq> = None;
    let mut direct_failure: Option<Seq> = None;
    let mut owed: Option<Seq> = None;
    let mut backoffs = 0;
    for _ in 0..1000 {
        if let Some(failure) = owed.take() {
            assert_owed_rerun(&mut j, failure, Regime::R6, &ids(16));
        }
        let action = j.next();
        if let Action::Decide(decision) = &action {
            if let DecisionPayload::HuntGroup(group) = &decision.payload {
                if group.hunt.get() == 3 && group.group.get() == 1 {
                    assert_eq!(backoffs, 2, "two backoffs follow the direct ends");
                    assert_eq!(core_ids(group.cores), [3], "{group:?}");
                    let hunt = j.run().hunts.active.as_ref().unwrap().seq;
                    let mut cited = vec![hunt];
                    cited.extend(&prior);
                    assert_eq!(decision.cause, cited);
                    let singleton = |j: &Journal| {
                        let next = active_plan(j).next_group().unwrap();
                        assert_eq!(next.stage, GroupStage::Repeated);
                        assert_eq!(core_ids(next.cores), [3]);
                    };
                    singleton(&j);
                    j.decide(decision.clone());
                    singleton(&j);
                    for _ in 0..5 {
                        let plan = j.next_trial();
                        j.trial(&plan, Outcome::Pass);
                    }
                    let normal = j.next();
                    let group = group_of(&normal).expect("the binary parts resume");
                    assert_eq!(group.granularity, 2);
                    assert_eq!(group.index, 0);
                    assert_eq!(group.inferred, None);
                    assert_eq!(group.profile.get(core(3)), Some(off(-8)));
                    assert_eq!(group.profile.get(core(4)), Some(off(-10)));
                    assert_eq!(j.resumed().next(), normal);
                    let Action::Decide(normal) = normal else {
                        unreachable!();
                    };
                    j.decide(normal);
                    assert_eq!(active_plan(&j).parts().len(), 2);
                    j.assert_resumes();
                    return;
                }
            }
            let seq = j.decide(decision.clone());
            match &decision.payload {
                DecisionPayload::Failure(failure)
                    if failure.condition == Some(Condition::Parked)
                        && failure.attribution == Attribution::Attributed =>
                {
                    direct_failure = Some(seq);
                    if failure.core == Some(core(3)) {
                        prior.push(seq);
                    }
                }
                DecisionPayload::HuntEnd(end) if end.result == HuntOutcome::Direct => {
                    direct_end = Some(seq);
                }
                DecisionPayload::TunerDecision(moved)
                    if moved.activity == Activity::Hunt && direct_end.is_some() =>
                {
                    assert_eq!(decision.cause, [direct_end.take().unwrap()]);
                    assert_eq!(moved.core, core(3));
                    assert_eq!(moved.to_offset.get(), moved.from_offset.get() + 1);
                    if backoffs == 0 {
                        owed = direct_failure;
                    }
                    backoffs += 1;
                }
                _ => {}
            }
            continue;
        }
        match action {
            Action::RunTrial(plan) => {
                let outcome = repeated_core_oracle(&plan);
                j.trial(&plan, outcome);
            }
            Action::ReadRanking => {
                j.ranking(&ids(16));
            }
            Action::Decide(_) => unreachable!(),
        }
    }
    panic!("the corroborated singleton probe was never scheduled");
}

struct Parked {
    workload: usize,
    duration: u32,
    loaded: Vec<u8>,
    condition: Condition,
    profile: Vec<i64>,
    named: Option<u8>,
}

fn carry_parked_failure(j: &mut Journal, shape: &Parked) -> Seq {
    let seq = j.next_seq();
    let payload = togi_journal::TrialCarried {
        source: togi_journal::FactSource {
            session: "20230101T000000Z".parse().unwrap(),
            seq,
            build: crate::testkit::build(),
            trial: None,
            time: jiff::Timestamp::from_second(1_600_000_000).unwrap(),
            boot: togi_machine::BootId::from("old".to_owned()),
            backend: None,
        },
        class: togi_journal::TrialClass {
            regime: Regime::R6,
            workload: workloads(Regime::R6)
                .get(shape.workload)
                .map(togi_machine::Workload::id),
            cores: set(&shape.loaded),
            duration_s: shape.duration,
        },
        condition: shape.condition,
        activity: None,
        rerun: false,
        profile: j.profile(&shape.profile).offsets(),
        outcome: Outcome::Failure,
        signal: Some(togi_machine::Signal::Crash),
        duration_s: shape.duration,
        core: shape.named.map(core),
        stalled_core: None,
        voltage_requests_v: togi_machine::PerCore::default(),
        top_requesters: togi_machine::CoreSet::default(),
        ccd_mhz: std::collections::BTreeMap::new(),
    };
    j.append(payload.into(), &[])
}

#[test]
fn repeated_parked_core_probe_requires_matching_adjacent_failures_since_reset() {
    let base = || Parked {
        workload: 0,
        duration: SHORT,
        loaded: vec![0, 1, 2, 3],
        condition: Condition::Parked,
        profile: vec![0, -9, 0, 0],
        named: Some(1),
    };
    let cases: Vec<(&str, bool, bool, Parked)> = vec![
        ("matching adjacent failures", true, false, base()),
        (
            "different workload",
            false,
            false,
            Parked {
                workload: 1,
                ..base()
            },
        ),
        (
            "different duration",
            false,
            false,
            Parked {
                duration: 600,
                ..base()
            },
        ),
        (
            "different loaded cores",
            false,
            false,
            Parked {
                loaded: vec![0, 1],
                ..base()
            },
        ),
        (
            "together failure",
            false,
            false,
            Parked {
                condition: Condition::Together,
                ..base()
            },
        ),
        (
            "unattributed failure",
            false,
            false,
            Parked {
                named: None,
                ..base()
            },
        ),
        (
            "different culprit",
            false,
            false,
            Parked {
                profile: vec![-9, 0, 0, 0],
                named: Some(0),
                ..base()
            },
        ),
        (
            "nonadjacent offsets",
            false,
            false,
            Parked {
                profile: vec![0, -8, 0, 0],
                ..base()
            },
        ),
        ("reset between failures", false, true, base()),
    ];
    for (name, probe, reset, second) in cases {
        let starts = [-20, -8, -20, -20].map(Start::has_room);
        let mut j = Journal::new(&ids(4), &starts);
        j.profile_change(&[-20, -8, -20, -20]);
        j.ranking(&ids(4));
        let older = carry_parked_failure(
            &mut j,
            &Parked {
                profile: vec![0, -10, 0, 0],
                ..base()
            },
        );
        if reset {
            j.reset(1);
            let transition = j.next_decision();
            j.decide(transition);
        }
        let newer = carry_parked_failure(&mut j, &second);
        j.core_phase(CoreTransition {
            core: core(1),
            from: None,
            to: CorePhase::AtLimit,
            offset: off(-8),
            pass: None,
            failure_point: Some(off(-9)),
            check_solo_limit: false,
            workloads: Vec::new(),
            cleared_combination: Vec::new(),
            reason: "test".to_owned(),
        });
        let failure = fail_together(&mut j, &[0, 1, 2, 3], &[-20, -8, -20, -20], SHORT);
        let (hunt, start) = j.start_hunt();
        assert_eq!(start.failure, failure, "{name}");
        let (group, cause) = j.group_plan();
        let expected: &[u8] = if probe { &[1] } else { &[0, 1] };
        assert_eq!(core_ids(group.cores), expected, "{name}");
        if probe {
            assert_eq!(cause, [hunt, older, newer], "{name}");
        }
        j.assert_resumes();
    }
}

fn is_hunt_end(action: &Action, result: HuntOutcome) -> bool {
    end_of(action).is_some_and(|end| end.result == result)
}

fn expect_decision(action: Action) -> crate::DecisionAction {
    match action {
        Action::Decide(decision) => decision,
        other => panic!("expected a decision, got {other:?}"),
    }
}

fn backoff_of(decision: &crate::DecisionAction) -> &TunerDecision {
    match &decision.payload {
        DecisionPayload::TunerDecision(backoff) => backoff,
        other => panic!("expected a backoff, got {other:?}"),
    }
}

fn members_of(pairs: &[(u8, i64)]) -> Vec<Member> {
    pairs
        .iter()
        .map(|&(id, offset)| Member {
            core: core(id),
            offset: off(offset),
        })
        .collect()
}

fn reaches_combination(j: &Journal) -> bool {
    j.run()
        .constraints
        .reaches(&j.run().offsets())
        .unwrap()
        .is_some()
}

fn assert_owed_rerun(j: &mut Journal, failure: Seq, regime: Regime, loaded: &[u8]) {
    let action = loop {
        let action = j.next();
        match &action {
            Action::Decide(decision)
                if matches!(
                    decision.payload,
                    DecisionPayload::CorePhase(_) | DecisionPayload::ProfileChange(_)
                ) =>
            {
                j.decide(decision.clone());
            }
            _ => break action,
        }
    };
    assert_eq!(j.resumed().next(), action);
    let Action::RunTrial(rerun) = action else {
        panic!("the commitment owes a rerun, the tuner asked for {action:?}");
    };
    assert_eq!(rerun.context, TrialContext::Rerun { failure });
    assert_eq!(rerun.condition, Condition::Together);
    assert_eq!(rerun.activity, Activity::Checking);
    assert_eq!(rerun.regime, regime);
    assert_eq!(rerun.load, Load::Many(set(loaded)));
    assert_eq!(rerun.cause, [failure]);
    assert_eq!(rerun.requirement.kind, crate::RequirementKind::Rerun);
    assert_eq!(
        rerun.requirement.window,
        crate::Window::LiveOrCarried {
            since: Some(failure)
        }
    );
    assert_eq!(rerun.duration, j.run().config.short_trial);
}

#[test]
fn hunt_pair_and_commitment_resume() {
    let mut j = hunt_of(4, SHORT);
    let source = j.run().hunts.active.as_ref().unwrap().start.failure;
    let fails = |plan: &TrialPlan| {
        at(plan, 0) <= -30 && at(plan, 1) <= -20 && at(plan, 2) == 0 && at(plan, 3) == 0
    };
    let mut probes: Vec<Member> = Vec::new();
    let mut end: Option<HuntEnd> = None;
    for _ in 0..200 {
        let action = j.next();
        assert_eq!(j.resumed().next(), action, "hunt prefix replay");
        match &action {
            Action::Decide(decision) => {
                match &decision.payload {
                    DecisionPayload::HuntGroup(group) => {
                        if group.group.get() == 1 {
                            assert_eq!(
                                core_ids(group.cores),
                                [2, 3],
                                "the recent core's part runs first"
                            );
                        }
                        probes.extend(group.probe);
                    }
                    DecisionPayload::HuntEnd(ended) => end = Some(ended.clone()),
                    _ => {}
                }
                j.decide(decision.clone());
                if end.is_some() {
                    break;
                }
            }
            Action::RunTrial(plan) => {
                let outcome = if fails(plan) {
                    Outcome::Failure
                } else {
                    Outcome::Pass
                };
                j.trial(plan, outcome);
                if outcome == Outcome::Failure {
                    attribute(&mut j);
                }
            }
            Action::ReadRanking => panic!("unexpected {action:?}"),
        }
    }
    let end = end.expect("the hunt ended");
    ended(&end, HuntOutcome::Combination);
    assert_eq!(core_ids(end.cores), [0, 1]);
    assert_eq!(end.members, members_of(&[(0, -30), (1, -20)]));
    let mut expected = members_of(&[(0, -29)]);
    expected.extend(members_of(&[
        (1, -29),
        (1, -28),
        (1, -26),
        (1, -22),
        (1, -14),
        (1, -18),
        (1, -20),
        (1, -19),
    ]));
    assert_eq!(probes, expected);

    let record = j.next();
    assert_eq!(j.resumed().next(), record, "commitment resume");
    let record = expect_decision(record);
    let DecisionPayload::Combination(combination) = &record.payload else {
        panic!("expected the combination, got {record:?}");
    };
    assert_eq!(combination.members, members_of(&[(0, -30), (1, -20)]));
    j.decide(record);

    let back = j.next();
    assert_eq!(j.resumed().next(), back, "commitment resume");
    let back = expect_decision(back);
    let moved = backoff_of(&back);
    assert_eq!(moved.decision, Decision::Backoff);
    assert_eq!(moved.activity, Activity::Hunt);
    assert_eq!((moved.core, moved.to_offset), (core(0), off(-29)));
    j.decide(back);
    assert!(
        j.run().hunts.active.is_none(),
        "the hunt reopened after its commitment"
    );
    assert!(!reaches_combination(&j));
    assert_owed_rerun(&mut j, source, Regime::R6, &[0, 1, 2, 3]);
    j.assert_resumes();
}

#[test]
fn combination_backoff_moves_to_a_tested_probe() {
    let mut j = hunt_of(4, SHORT);
    let fails = |p0: i64, p1: i64| (p0 <= -27 && p1 <= -27) || (p0 == -26 && p1 == -30);
    let mut probes: std::collections::BTreeMap<Seq, i64> = std::collections::BTreeMap::new();
    loop {
        let action = j.next();
        match &action {
            Action::Decide(decision) => {
                let seq = j.decide(decision.clone());
                if let DecisionPayload::HuntGroup(group) = &decision.payload {
                    if let Some(probe) = group.probe {
                        probes.insert(seq, probe.offset.get());
                    }
                }
                if is_hunt_end(&action, HuntOutcome::Combination) {
                    break;
                }
            }
            Action::RunTrial(plan) => {
                let failed = fails(at(plan, 0), at(plan, 1));
                j.trial(
                    plan,
                    if failed {
                        Outcome::Failure
                    } else {
                        Outcome::Pass
                    },
                );
                if failed {
                    attribute(&mut j);
                }
            }
            Action::ReadRanking => panic!("unexpected {action:?}"),
        }
    }
    let record = j.next_decision();
    let DecisionPayload::Combination(combination) = &record.payload else {
        panic!("expected the combination, got {record:?}");
    };
    assert_eq!(combination.members, members_of(&[(0, -26), (1, -30)]));
    let recorded = j.decide(record);
    let back = j.next_decision();
    let moved = backoff_of(&back);
    assert_eq!(moved.decision, Decision::Backoff);
    assert_eq!((moved.core, moved.to_offset), (core(0), off(-25)));
    assert_eq!(back.cause.len(), 2, "{:?}", back.cause);
    assert_eq!(back.cause[0], recorded);
    assert_eq!(probes.get(&back.cause[1]), Some(&-25));
    j.decide(back);
    let offsets = current(&j);
    assert!(
        !fails(offsets[0], offsets[1]),
        "together {offsets:?} still fails"
    );
    j.assert_resumes();
}

#[test]
fn hunt_fallback_and_full_check() {
    struct Case {
        name: &'static str,
        duration: u32,
        full_fails: bool,
        result: HuntOutcome,
        reason: &'static [&'static str],
    }
    let cases = [
        Case {
            name: "fallback",
            duration: 120,
            full_fails: false,
            result: HuntOutcome::Fallback,
            reason: &["unresolved", "combination"],
        },
        Case {
            name: "full failure",
            duration: 600,
            full_fails: true,
            result: HuntOutcome::Combination,
            reason: &[
                "member probes",
                "core 00 -30",
                "core 01 -30",
                "a count shallower",
            ],
        },
    ];
    for case in cases {
        let mut j = hunt_of(2, case.duration);
        let mut full = 0;
        let end = loop {
            let action = j.next();
            if group_of(&action).is_some_and(|g| g.stage == HuntStage::Full) {
                full += 1;
            }
            if let Some(end) = end_of(&action) {
                break end.clone();
            }
            j.step(&mut |plan| {
                if case.full_fails && at(plan, 0) == -30 && at(plan, 1) == -30 {
                    Outcome::Failure
                } else {
                    Outcome::Pass
                }
            });
        };
        assert_eq!(end.result, case.result, "{}: {}", case.name, end.reason);
        for token in case.reason {
            assert!(
                end.reason.contains(token),
                "{}: {:?} lacks {token}",
                case.name,
                end.reason
            );
        }
        if case.full_fails {
            assert_eq!(full, 1, "{}", case.name);
        }
        j.assert_resumes();
    }
}

fn limited(offsets: [i64; 2]) -> Journal {
    let starts = offsets.map(|offset| Start::at_limit(offset).fail(offset - 1).pass(offset));
    let mut j = Journal::new(&[0, 1], &starts);
    j.profile_change(&offsets);
    j
}

fn manual_end(j: &mut Journal, result: HuntOutcome, cores: &[u8]) {
    let end = HuntEnd {
        hunt: std::num::NonZeroU32::MIN,
        result,
        cores: set(cores),
        members: Vec::new(),
        groups: 2,
        reason: "test".to_owned(),
    };
    j.append(end.into(), &[]);
}

#[test]
fn hunt_combination_already_broken() {
    let mut j = limited([-29, -30]);
    start_hunt_at(&mut j, 1, &[0, 1], &[-30, -30]);
    manual_end(&mut j, HuntOutcome::Combination, &[0, 1]);
    let record = j.next_decision();
    let DecisionPayload::Combination(combination) = &record.payload else {
        panic!("expected the combination, got {record:?}");
    };
    for token in ["no backoff", "core 00", "already shallower"] {
        assert!(
            combination.reason.contains(token),
            "{:?}",
            combination.reason
        );
    }
    j.decide(record);
    assert!(j.run().hunts.active.is_none());
    j.assert_resumes();
}

#[test]
fn hunt_culprit_discards_contradicted_pass() {
    let mut j = limited([-29, -30]);
    start_hunt_at(&mut j, 1, &[0, 1], &[-29, -30]);
    manual_end(&mut j, HuntOutcome::Culprit, &[0]);
    let commitment = j.next_decision();
    let moved = backoff_of(&commitment);
    assert_eq!(moved.activity, Activity::Hunt);
    assert_eq!(moved.core, core(0));
    assert_eq!(moved.pass, None);
    assert_eq!(moved.failure_point, Some(off(-29)));
    for token in ["-29", "discarded", "contradicts"] {
        assert!(moved.reason.contains(token), "{:?}", moved.reason);
    }
    j.assert_resumes();
}

#[test]
fn sixteen_core_culprit_and_pair() {
    for (name, members, result) in [
        ("single core", vec![13u8], HuntOutcome::Direct),
        ("combination pair", vec![3, 11], HuntOutcome::Combination),
    ] {
        let mut j = hunt_of(16, SHORT);
        let mut oracle = |plan: &TrialPlan| {
            if members.iter().all(|&member| at(plan, member) == -30) {
                Outcome::Failure
            } else {
                Outcome::Pass
            }
        };
        let mut groups = 0;
        let mut end = None;
        for _ in 0..2500 {
            let action = j.step(&mut oracle);
            if let Some(group) = group_of(&action) {
                groups = groups.max(group.group.get());
            }
            if let Some(found) = end_of(&action) {
                end = Some(found.clone());
                break;
            }
        }
        let end = end.unwrap_or_else(|| panic!("{name}: the hunt did not terminate"));
        assert_eq!(end.result, result, "{name}: {end:?}");
        assert_eq!(core_ids(end.cores), members, "{name}");
        assert!(
            groups <= 194,
            "{name}: {groups} groups exceed the progress bound"
        );
        j.assert_resumes();
    }
}

fn restore_has_room(j: &mut Journal, id: u8, offset: i64) {
    j.core_phase(CoreTransition {
        core: core(id),
        from: Some(CorePhase::Search),
        to: CorePhase::HasRoom,
        offset: off(offset),
        pass: None,
        failure_point: None,
        check_solo_limit: false,
        workloads: Vec::new(),
        cleared_combination: Vec::new(),
        reason: "test".to_owned(),
    });
}

#[test]
fn hunt_duration_prior_respects_evidence_and_short_failures() {
    struct Case {
        name: &'static str,
        count: usize,
        workload: usize,
        profile: [i64; 2],
        short_failed: bool,
        reset: bool,
        want: u32,
    }
    let base = Case {
        name: "",
        count: 5,
        workload: 0,
        profile: [-32, -32],
        short_failed: false,
        reset: false,
        want: 600,
    };
    let cases = [
        Case {
            name: "valid deeper passes",
            ..base
        },
        Case {
            name: "short failure in another workload",
            short_failed: true,
            want: 120,
            ..base
        },
        Case {
            name: "insufficient trials",
            count: 4,
            want: 120,
            ..base
        },
        Case {
            name: "different workload",
            workload: 1,
            want: 120,
            ..base
        },
        Case {
            name: "incomparable profile",
            profile: [-32, -29],
            want: 120,
            ..base
        },
        Case {
            name: "reset evidence boundary",
            reset: true,
            want: 120,
            ..base
        },
    ];
    for case in cases {
        let mut j = Journal::new(&[0, 1], &[Start::has_room(-30); 2]);
        j.profile_change(&[-30, -30]);
        j.ranking(&[0, 1]);
        if case.short_failed {
            j.carry(
                "20230101T000000Z",
                (Regime::R6, 1, &[0, 1], SHORT),
                Condition::Parked,
                &[-40, 0],
                Outcome::Failure,
            );
        }
        let passes: Vec<Seq> = (0..case.count)
            .map(|_| {
                j.carry(
                    "20230101T000000Z",
                    (Regime::R6, case.workload, &[0, 1], SHORT),
                    Condition::Parked,
                    &case.profile,
                    Outcome::Pass,
                )
            })
            .collect();
        if case.reset {
            j.reset(0);
            let transition = j.next_decision();
            j.decide(transition);
            restore_has_room(&mut j, 0, -30);
        }
        fail_together(&mut j, &[0, 1], &[-30, -30], 600);
        let (hunt, _) = j.start_hunt();
        let (group, cause) = j.group_plan();
        assert_eq!(group.duration_s, case.want, "{}", case.name);
        if case.want == 600 {
            let mut cited = vec![hunt];
            cited.extend(&passes);
            assert_eq!(cause, cited, "{}", case.name);
        }
        let Action::Decide(planned) = j.next() else {
            unreachable!();
        };
        j.decide(planned);
        let next = j.next();
        assert_eq!(j.resumed().next(), next, "{}", case.name);
        if group.inferred.is_none() {
            let Action::RunTrial(plan) = &next else {
                panic!("{}: expected the group's trial, got {next:?}", case.name);
            };
            assert_eq!(plan.duration.get(), case.want, "{}", case.name);
        }
        j.assert_resumes();
    }
}

#[test]
fn hunt_duration_prior_is_vetoed_by_an_earlier_idle_failure() {
    let session = "20230101T000000Z";
    for (name, idle, want) in [
        ("no idle failure", None, 600),
        ("live idle failure", Some(false), 120),
        ("carried idle failure", Some(true), 120),
    ] {
        let mut j = Journal::new(&[0, 1], &[Start::has_room(-30); 2]);
        j.profile_change(&[-30, -30]);
        j.ranking(&[0, 1]);
        let passes: Vec<Seq> = (0..5)
            .map(|_| {
                j.carry(
                    session,
                    (Regime::R6, 0, &[0, 1], SHORT),
                    Condition::Parked,
                    &[-32, -32],
                    Outcome::Pass,
                )
            })
            .collect();
        match idle {
            Some(false) => {
                let deep = j.profile(&[-40, -40]).offsets();
                j.idle_failure(deep, Condition::Together);
            }
            Some(true) => {
                j.carry_idle(session, &[-40, -40]);
            }
            None => {}
        }
        let failure = j.next_seq();
        let start = HuntStart {
            hunt: std::num::NonZeroU32::MIN,
            failure,
            trial: Some(togi_journal::TrialId::from("0001".to_owned())),
            regime: Regime::R6,
            workload: workloads(Regime::R6)[0].id(),
            cores: set(&[0, 1]),
            duration_s: 600,
            failing: j.profile(&[-30, -30]).offsets(),
            parked: j.profile(&[0, 0]).offsets(),
            parked_seq: None,
            candidates: set(&[0, 1]),
            trials: 5,
            trial_s: SHORT,
            miss: 0.0,
            rate: 0.0,
            ranking: vec![core(0), core(1)],
            reason: None,
        };
        let hunt = j.append(start.into(), &[]);
        let (group, cause) = j.group_plan();
        assert_eq!(group.duration_s, want, "{name}");
        if want == 600 {
            let mut cited = vec![hunt];
            cited.extend(&passes);
            assert_eq!(cause, cited, "{name}");
        }
        j.assert_resumes();
    }
}

#[test]
fn hunt_duration_escalation_only_for_longer_failure() {
    let part = |duration| (HuntStage::Part, duration, false);
    for (name, duration, expected) in [
        ("shorter", 90, vec![part(120), part(120)]),
        ("equal", 120, vec![part(120), part(120)]),
        (
            "longer",
            600,
            vec![
                part(120),
                part(120),
                (HuntStage::Full, 120, false),
                (HuntStage::Part, 600, true),
                (HuntStage::Part, 600, true),
            ],
        ),
    ] {
        let mut j = hunt_of(2, duration);
        let mut groups = Vec::new();
        let mut trials = 0;
        loop {
            let action = j.next();
            match &action {
                Action::Decide(decision) => match &decision.payload {
                    DecisionPayload::HuntGroup(group) => {
                        groups.push((group.stage, group.duration_s, group.escalated));
                    }
                    DecisionPayload::HuntEnd(end) => {
                        assert_eq!(end.result, HuntOutcome::Fallback, "{name}");
                        assert_eq!(j.resumed().next(), action, "{name}");
                        break;
                    }
                    _ => {}
                },
                Action::RunTrial(plan) => {
                    let latest = j.run().hunts.active.as_ref().unwrap().latest().unwrap();
                    assert_eq!(plan.duration.get(), latest.payload.duration_s, "{name}");
                    trials += 1;
                }
                Action::ReadRanking => panic!("{name}: unexpected {action:?}"),
            }
            j.step(&mut pass_all);
        }
        assert_eq!(groups, expected, "{name}");
        assert_eq!(trials, expected.len() * 5, "{name}");
        j.assert_resumes();
    }
}

#[test]
fn hunt_escalation_and_inferred_group() {
    let mut j = hunt_of(2, 600);
    let first = j.next_decision();
    let profile = group_in(&first).profile;
    for _ in 0..5 {
        j.carry(
            "20230101T000000Z",
            (Regime::R6, 0, &[0, 1], SHORT),
            Condition::Parked,
            &profile.iter().map(|(_, o)| o.get()).collect::<Vec<_>>(),
            Outcome::Pass,
        );
    }
    let inferred = j.next_decision();
    assert_eq!(group_in(&inferred).inferred, Some(Inference::Pass));
    j.decide(inferred);
    let mut escalated = false;
    for _ in 0..200 {
        let action = j.next();
        if let Some(group) = group_of(&action) {
            if group.escalated {
                escalated = true;
                assert_eq!(group.duration_s, 600);
            }
        }
        if is_hunt_end(&action, HuntOutcome::Fallback) {
            assert!(escalated, "the full pass never escalated");
            j.assert_resumes();
            return;
        }
        j.step(&mut pass_all);
    }
    panic!("the escalated hunt did not finish");
}

#[test]
fn active_hunt_projection() {
    struct Case {
        name: &'static str,
        stage: HuntStage,
        prior: usize,
        live: Vec<Outcome>,
        inferred: Option<Inference>,
        skipped: bool,
        reset: bool,
        escalated: bool,
        probe: Option<Member>,
        held: Vec<Member>,
        passes: u32,
        outcome: Option<GroupOutcome>,
    }
    let base = || Case {
        name: "",
        stage: HuntStage::Part,
        prior: 0,
        live: Vec::new(),
        inferred: None,
        skipped: false,
        reset: false,
        escalated: false,
        probe: None,
        held: Vec::new(),
        passes: 0,
        outcome: None,
    };
    let cases = vec![
        Case {
            name: "running reuses earlier valid passes",
            prior: 2,
            live: vec![Outcome::Pass],
            passes: 3,
            ..base()
        },
        Case {
            name: "passing complement accumulates trials",
            stage: HuntStage::Complement,
            prior: 4,
            live: vec![Outcome::Pass],
            passes: 5,
            outcome: Some(GroupOutcome::Passed),
            ..base()
        },
        Case {
            name: "failure invalidates earlier passes",
            prior: 2,
            live: vec![Outcome::Failure],
            outcome: Some(GroupOutcome::Failed),
            ..base()
        },
        Case {
            name: "inferred pass",
            prior: 5,
            inferred: Some(Inference::Pass),
            passes: 5,
            outcome: Some(GroupOutcome::Passed),
            ..base()
        },
        Case {
            name: "inferred failure",
            stage: HuntStage::Complement,
            inferred: Some(Inference::Failure),
            outcome: Some(GroupOutcome::Failed),
            ..base()
        },
        Case {
            name: "skipped",
            skipped: true,
            outcome: Some(GroupOutcome::Skipped),
            ..base()
        },
        Case {
            name: "reset excludes reused evidence",
            prior: 4,
            reset: true,
            live: vec![Outcome::Pass],
            passes: 1,
            ..base()
        },
        Case {
            name: "escalated full uses only new trials",
            stage: HuntStage::Full,
            prior: 4,
            live: vec![Outcome::Pass],
            escalated: true,
            passes: 1,
            ..base()
        },
        Case {
            name: "member probe preserves held members",
            stage: HuntStage::Probe,
            prior: 4,
            live: vec![Outcome::Pass],
            probe: Some(Member {
                core: core(0),
                offset: off(-30),
            }),
            held: members_of(&[(1, -30)]),
            passes: 1,
            ..base()
        },
    ];
    for case in cases {
        let mut j = Journal::new(&ids(4), &[Start::has_room(-30); 4]);
        j.profile_change(&[-30; 4]);
        j.ranking(&ids(4));
        if case.prior > 0 {
            ran_group(
                &mut j,
                1,
                &ids(4),
                &[-30; 4],
                HuntStage::Part,
                Outcome::Pass,
                case.prior,
            );
        }
        if case.reset {
            j.reset(0);
            let transition = j.next_decision();
            j.decide(transition);
            restore_has_room(&mut j, 0, -30);
        }
        let begin = start_hunt_at(&mut j, 2, &ids(4), &[-30; 4]);
        let group = HuntGroup {
            hunt: std::num::NonZeroU32::new(2).unwrap(),
            group: std::num::NonZeroU32::MIN,
            cores: set(&[0, 1]),
            profile: j.profile(&[-30, -30, 0, 0]).offsets(),
            set: set(&ids(4)),
            granularity: 2,
            stage: case.stage,
            index: 0,
            duration_s: SHORT,
            escalated: case.escalated,
            full_checked: false,
            any_failed: false,
            probe: case.probe,
            held: case.held.clone(),
            inferred: case.inferred,
            skipped: case.skipped,
            reason: Some("test".to_owned()),
        };
        let seq = j.append(group.into(), &[begin]);
        for outcome in &case.live {
            run_group(&mut j, *outcome == Outcome::Failure);
        }
        let view = j.run().hunt_groups();
        assert_eq!(view.len(), 1, "{}", case.name);
        let group = &view[0];
        assert_eq!(group.number, 1, "{}", case.name);
        assert_eq!(group.seq, seq, "{}", case.name);
        assert_eq!(core_ids(group.cores), [0, 1], "{}", case.name);
        assert_eq!(group.stage, case.stage, "{}", case.name);
        assert_eq!(group.probe, case.probe, "{}", case.name);
        assert_eq!(group.held, case.held, "{}", case.name);
        assert_eq!(group.needed, 5, "{}", case.name);
        assert_eq!(group.passed, case.passes, "{}", case.name);
        assert_eq!(group.outcome, case.outcome, "{}", case.name);
        assert!(!group.carried, "{}", case.name);
        j.assert_resumes();
        cancel_hunt(&mut j, 2);
        assert!(j.run().hunt_groups().is_empty(), "{}", case.name);
        j.assert_resumes();
    }
}

#[test]
fn drain_commits_resolved_hunt_without_starting_groups() {
    let mut j = limit_session(2);
    carry_parked_failure(
        &mut j,
        &Parked {
            workload: 0,
            duration: SHORT,
            loaded: vec![0, 1],
            condition: Condition::Parked,
            profile: vec![0, -30],
            named: Some(1),
        },
    );
    let source = fail_together(&mut j, &[0, 1], &[-30, -30], 600);
    j.start_hunt();
    let mut end = None;
    for _ in 0..20 {
        let action = j.run().hunt_next().expect("the open hunt has work");
        assert_eq!(j.next(), action);
        let Action::Decide(decision) = action else {
            panic!("the resolving hunt started a group trial: {action:?}");
        };
        let closing = j.run().hunt_closing();
        if let DecisionPayload::HuntEnd(found) = &decision.payload {
            assert_eq!(closing.as_ref(), Some(&decision));
            end = Some(found.clone());
            j.decide(decision);
            break;
        }
        assert_eq!(closing, None, "a group plan is not a closing decision");
        j.decide(decision);
    }
    let end = end.expect("the known failure resolved the hunt without a trial");
    ended(&end, HuntOutcome::Culprit);
    assert_eq!(core_ids(end.cores), [1]);
    let commitment = j.run().hunt_closing().expect("the culprit is committed");
    assert_eq!(j.next(), Action::Decide(commitment.clone()));
    let moved = backoff_of(&commitment);
    assert_eq!(moved.decision, Decision::Backoff);
    assert_eq!((moved.core, moved.to_offset), (core(1), off(-29)));
    j.decide(commitment);
    assert_eq!(j.run().hunt_closing(), None);
    assert_eq!(j.run().hunt_next(), None);
    assert_owed_rerun(&mut j, source, Regime::R6, &[0, 1]);
    j.assert_resumes();
}

#[test]
fn drain_records_fallback_combination_before_backoff() {
    let mut j = hunt_of(2, SHORT);
    let end = j.run_hunt(100, pass_all);
    ended(&end, HuntOutcome::Fallback);
    let record = j.run().hunt_closing().expect("the fallback is recorded");
    assert_eq!(j.next(), Action::Decide(record.clone()));
    assert_eq!(j.resumed().run().hunt_closing(), Some(record.clone()));
    let DecisionPayload::Combination(combination) = &record.payload else {
        panic!("expected the combination, got {record:?}");
    };
    assert!(combination.fallback);
    assert_eq!(combination.members, members_of(&[(0, -30), (1, -30)]));
    j.decide(record);
    let back = j.run().hunt_closing().expect("the combination is broken");
    assert_eq!(j.next(), Action::Decide(back.clone()));
    let moved = backoff_of(&back);
    assert_eq!(moved.decision, Decision::Backoff);
    assert_eq!(moved.to_offset, off(-29));
    j.decide(back);
    assert!(!reaches_combination(&j));
    assert_eq!(j.run().hunt_closing(), None);
    assert_eq!(j.run().hunt_next(), None);
    j.assert_resumes();
}

fn restore_offsets(j: &mut Journal, was: &[i64]) {
    let now = current(j);
    for (id, (&before, &after)) in (0u8..).zip(was.iter().zip(&now)) {
        if before != after {
            restore_has_room(j, id, before);
        }
    }
}

fn escalate_r7(j: &mut Journal) -> Seq {
    use crate::r7::escalation::ESCALATE_AFTER;
    let report = crate::testkit::R7Report::default().top(&[0]);
    let before = current(j);
    for _ in 0..ESCALATE_AFTER {
        j.r7_failed(&[0, 1], &before, &report);
        loop {
            let decision = j.next_decision();
            let backoff = matches!(
                &decision.payload,
                DecisionPayload::TunerDecision(moved) if moved.decision == Decision::Backoff
            );
            j.decide(decision);
            if backoff {
                break;
            }
        }
        restore_offsets(j, &before);
        j.profile_change(&before);
    }
    j.r7_failed(&[0, 1], &before, &report)
}

#[test]
fn located_hunt_resumes_during_locate() {
    let mut j = Journal::r7_session();
    escalate_r7(&mut j);
    let start = loop {
        let decision = j.next_decision();
        if matches!(decision.payload, DecisionPayload::HuntStart(_)) {
            break decision;
        }
        j.decide(decision);
    };
    j.decide(start);
    let locate = j.next_decision();
    assert_eq!(group_in(&locate).stage, HuntStage::Locate);
    j.decide(locate);
    for _ in 0..2 {
        let plan = j.next_trial();
        j.trial(&plan, Outcome::Pass);
    }
    let want = j.next();
    let Action::RunTrial(plan) = &want else {
        panic!("locate did not continue: {want:?}");
    };
    assert!(matches!(plan.context, TrialContext::Hunt { group, .. } if group.get() == 1));
    assert_eq!(
        plan.profile
            .iter()
            .map(|(_, o)| o.get())
            .collect::<Vec<_>>(),
        [-30, -30, 0, 0]
    );
    assert_eq!(j.resumed().next(), want);
    let view = j.run().hunt_groups();
    assert_eq!((view[0].passed, view[0].outcome), (2, None));
    j.assert_resumes();
}

fn named_failure(j: &mut Journal, plan: &TrialPlan, named: u8) -> Seq {
    let (intent, trial) = j.intent(plan);
    let mut end = Journal::end_payload(&trial, Outcome::Failure, Some(togi_machine::Signal::Crash));
    end.core = Some(core(named));
    j.append(end.into(), &[intent])
}

#[test]
fn parked_failure_naming_a_core_at_zero_reruns_all_zero() {
    for rerun_fails in [false, true] {
        let mut j = hunt_of(4, SHORT);
        let planned = j.next_decision();
        let group = group_in(&planned).clone();
        j.decide(planned);
        let parked = ids(4)
            .into_iter()
            .find(|&id| !group.cores.contains(core(id)))
            .unwrap();
        let plan = j.next_trial();
        named_failure(&mut j, &plan, parked);
        let attribution = j.next_decision();
        let DecisionPayload::Failure(record) = &attribution.payload else {
            panic!("expected the attribution, got {attribution:?}");
        };
        assert_eq!(record.core, Some(core(parked)));
        assert_eq!(record.offset, Some(off(0)));
        let failure = j.decide(attribution);
        let rerun = j.next_trial();
        assert_eq!(rerun.context, TrialContext::ZeroRerun { failure });
        assert_eq!(rerun.condition, Condition::Parked);
        assert_eq!(rerun.activity, Activity::Hunt);
        assert_eq!(rerun.duration.get(), SHORT);
        assert_eq!(rerun.cause, [failure]);
        assert_eq!(rerun.requirement.kind, crate::RequirementKind::ZeroRerun);
        assert_eq!(rerun.requirement.needed.get(), 1);
        assert!(rerun.profile.is_zero());
        assert_eq!(rerun.load, Load::Many(set(&ids(4))));
        if !rerun_fails {
            j.trial(&rerun, Outcome::Pass);
            let next = j.next();
            let planned = group_of(&next).expect("the failure goes to the group's members");
            assert!(!planned.cores.is_empty());
            assert!(planned.cores.iter().all(|c| group.cores.contains(c)));
            j.assert_resumes();
            continue;
        }
        let rerun_end = j.trial(&rerun, Outcome::Failure);
        let after = j.next_decision();
        j.decide(after);
        let action = j.next();
        let decision = expect_decision(action);
        let DecisionPayload::DeadEnd(dead) = &decision.payload else {
            panic!("expected the dead end, got {decision:?}");
        };
        assert_eq!(
            dead.condition,
            togi_journal::DeadEndCondition::FailureAtZero
        );
        assert_eq!(decision.cause, [failure, rerun_end]);
        assert_eq!(
            dead.detail,
            format!(
                "core {parked:02} failed at CO 0; the rerun with every core at CO 0 failed too (#{rerun_end}), so the instability is not caused by Curve Optimizer"
            )
        );
        j.assert_resumes();
    }
}

#[test]
fn together_failure_naming_a_core_at_zero_is_hunted_after_a_passed_rerun() {
    for gate_open in [false, true] {
        let mut j = Journal::new(&[0, 1], &[Start::has_room(0), Start::has_room(-10)]);
        j.profile_change(&[0, -10]);
        j.ranking(&[0, 1]);
        if gate_open {
            pass_cycles(&mut j, 3);
            start_cycle(&mut j);
            assert!(
                j.run().later_gate().is_some(),
                "the later-failure gate is closed"
            );
        }
        let plan = together_plan(&mut j, &[0, 1], &[0, -10], SHORT);
        named_failure(&mut j, &plan, 0);
        let attribution = j.next_decision();
        let failure = j.decide(attribution);
        let rerun = j.next_trial();
        assert_eq!(rerun.context, TrialContext::ZeroRerun { failure });
        j.trial(&rerun, Outcome::Pass);
        let action = j.next();
        let decision = expect_decision(action.clone());
        let DecisionPayload::HuntStart(start) = &decision.payload else {
            panic!("the failure was not hunted among the nonzero cores: {decision:?}");
        };
        assert_eq!(start.failure, failure);
        assert_eq!(core_ids(start.candidates), [1]);
        assert_eq!(strikes(&j), 0, "a failure at CO 0 was held");
        assert_eq!(j.resumed().next(), action);
        j.decide(decision);
        let end = expect_decision(j.next());
        let DecisionPayload::HuntEnd(end) = &end.payload else {
            panic!("expected the hunt end, got {end:?}");
        };
        assert_eq!(end.result, HuntOutcome::Culprit);
        assert_eq!(core_ids(end.cores), [1]);
        j.assert_resumes();
    }
}

fn group_event(
    number: u32,
    hunt: u32,
    cores: &[u8],
    profile: crate::Profile,
    stage: HuntStage,
) -> HuntGroup {
    HuntGroup {
        hunt: std::num::NonZeroU32::new(hunt).unwrap(),
        group: std::num::NonZeroU32::new(number).unwrap(),
        cores: set(cores),
        profile: profile.offsets(),
        set: profile.cores(),
        granularity: 2,
        stage,
        index: 0,
        duration_s: SHORT,
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

#[test]
fn known_failure_skip_completes_running_member_probe_group() {
    let mut j = Journal::new(&[0, 1], &[Start::has_room(-10); 2]);
    j.profile_change(&[-10, -10]);
    j.ranking(&[0, 1]);
    start_hunt_at(&mut j, 1, &[0, 1], &[-9, -10]);
    let earlier = group_event(1, 1, &[0, 1], j.profile(&[-9, -10]), HuntStage::Full);
    j.append(earlier.into(), &[]);
    let plan = j.next_trial();
    let known = j.trial(&plan, Outcome::Failure);
    attribute(&mut j);
    cancel_hunt(&mut j, 1);
    fail_together(&mut j, &[0, 1], &[-10, -10], 600);
    j.ranking(&[0, 1]);
    j.start_hunt();
    let mut oracle = |plan: &TrialPlan| {
        if at(plan, 0) == -10 && at(plan, 1) == -10 {
            Outcome::Failure
        } else {
            Outcome::Pass
        }
    };
    let planned = loop {
        let action = j.next();
        if group_of(&action).is_some_and(|group| group.probe.is_some()) {
            break expect_decision(action);
        }
        j.step(&mut oracle);
    };
    let group = group_in(&planned).clone();
    assert_eq!(
        group.probe,
        Some(Member {
            core: core(0),
            offset: off(-9)
        })
    );
    assert_eq!(group.inferred, None);
    let planned_seq = j.decide(planned);

    let resolved = j.next();
    let replacement = group_of(&resolved).expect("the known failure resolves the probe group");
    assert_eq!(replacement.group, group.group);
    assert_eq!(replacement.inferred, Some(Inference::Failure));
    assert_eq!(
        expect_decision(resolved.clone()).cause,
        [planned_seq, known]
    );
    j.decide(expect_decision(resolved));
    j.assert_resumes();

    let continued = j.next();
    let probe = group_of(&continued).expect("the inferred failure advances the probes");
    assert_eq!(
        probe.probe,
        Some(Member {
            core: core(0),
            offset: off(-8)
        })
    );
    j.decide(expect_decision(continued));
    for _ in 0..100 {
        let action = j.next();
        if let Some(end) = end_of(&action) {
            ended(end, HuntOutcome::Combination);
            assert_eq!(end.members, members_of(&[(0, -9), (1, -10)]));
            assert_eq!(j.resumed().next(), action);
            return;
        }
        j.step(&mut pass_all);
    }
    panic!("the member probe hunt did not finish");
}

const CARRIED_SESSION: &str = "20261002T004254Z";

fn carry_passes(
    j: &mut Journal,
    regime: Regime,
    cores: &[u8],
    profile: &[i64],
    count: usize,
    outcome: Outcome,
) -> Vec<Seq> {
    (0..count)
        .map(|_| {
            j.carry(
                CARRIED_SESSION,
                (regime, 0, cores, SHORT),
                Condition::Together,
                profile,
                outcome,
            )
        })
        .collect()
}

#[test]
fn running_hunt_group_cites_mixed_carried_and_live_passes() {
    let mut j = Journal::new(&[0, 1], &[Start::at_limit(-10); 2]);
    j.profile_change(&[-10, -10]);
    j.ranking(&[0, 1]);
    let first = carry_passes(&mut j, Regime::R6, &[0, 1], &[-10, 0], 3, Outcome::Pass);
    let second = carry_passes(&mut j, Regime::R6, &[0, 1], &[0, -10], 3, Outcome::Pass);
    let start = start_regime_hunt(&mut j, 1, Regime::R6, &[0, 1], &[-10, -10]);
    for (index, facts) in [&first, &second].into_iter().enumerate() {
        let number = u32::try_from(index + 1).unwrap();
        let planned = j.run().hunt_next().expect("a group to plan");
        let decision = expect_decision(planned.clone());
        let group = group_in(&decision);
        assert_eq!((group.group.get(), group.inferred), (number, None));
        j.decide(expect_decision(planned));
        for _ in 0..2 {
            let Some(Action::RunTrial(plan)) = j.run().hunt_next() else {
                panic!("group {number} needs two live trials");
            };
            assert!(
                matches!(plan.context, TrialContext::Hunt { group, .. } if group.get() == number)
            );
            j.trial(&plan, Outcome::Pass);
        }
        let done = j.run().hunt_next().expect("the group completed");
        let decision = expect_decision(done.clone());
        let mut cited = vec![start];
        cited.extend(&first);
        let reason = if index == 1 {
            cited.extend(facts.iter());
            let DecisionPayload::HuntEnd(end) = &decision.payload else {
                panic!("the two-group hunt ends: {decision:?}");
            };
            assert_eq!((end.result, end.groups), (HuntOutcome::Fallback, 2));
            end.reason.clone()
        } else {
            let DecisionPayload::HuntGroup(next) = &decision.payload else {
                panic!("group 2 is planned: {decision:?}");
            };
            assert_eq!(next.group.get(), 2);
            next.reason.clone().unwrap()
        };
        assert_eq!(decision.cause, cited, "group {number}");
        assert!(reason.contains(CARRIED_SESSION), "group {number}: {reason}");
        assert_eq!(j.resumed().run().hunt_next(), Some(done), "group {number}");
    }
    j.assert_resumes();
}

#[test]
fn hunt_plan_marks_only_groups_answered_by_carried_evidence() {
    let mut j = Journal::new(&[0, 1], &[Start::at_limit(-10); 2]);
    j.profile_change(&[-10, -10]);
    j.ranking(&[0, 1]);
    carry_passes(&mut j, Regime::R7, &[0, 1], &[-10, 0], 5, Outcome::Pass);
    start_regime_hunt(&mut j, 1, Regime::R7, &[0, 1], &[-10, -10]);
    for _ in 0..20 {
        let Some(action) = j.run().hunt_next() else {
            break;
        };
        if is_hunt_end(&action, HuntOutcome::Fallback) {
            break;
        }
        match action {
            Action::RunTrial(plan) => {
                j.trial(&plan, Outcome::Pass);
            }
            Action::Decide(decision) => {
                j.decide(decision);
            }
            Action::ReadRanking => panic!("unexpected ranking"),
        }
    }
    let view = j.run().hunt_groups();
    assert_eq!(view.len(), 2, "{view:?}");
    assert_eq!(
        (view[0].outcome, view[0].carried),
        (Some(GroupOutcome::Passed), true)
    );
    assert_eq!(
        (view[1].outcome, view[1].carried),
        (Some(GroupOutcome::Passed), false)
    );
    j.assert_resumes();
}

#[test]
fn hunt_plan_keeps_a_carried_failure_answer_after_later_live_failures() {
    let mut j = Journal::new(&ids(4), &[Start::at_limit(-10); 4]);
    j.profile_change(&[-10; 4]);
    j.ranking(&ids(4));
    carry_passes(
        &mut j,
        Regime::R7,
        &[0, 1, 2, 3],
        &[-10, -10, 0, 0],
        1,
        Outcome::Failure,
    );
    start_regime_hunt(&mut j, 1, Regime::R7, &[0, 1, 2, 3], &[-10; 4]);
    let first = j.run().hunt_next().expect("the first group");
    let group = group_of(&first).expect("the first group");
    assert_eq!(group.inferred, Some(Inference::Failure));
    assert_eq!(group.profile, j.profile(&[-10, -10, 0, 0]).offsets());
    j.decide(expect_decision(first));
    for _ in 0..20 {
        let Some(action) = j.run().hunt_next() else {
            break;
        };
        match action {
            Action::RunTrial(plan) => {
                j.trial(&plan, Outcome::Failure);
                if let Action::Decide(decision) = j.next() {
                    if matches!(decision.payload, DecisionPayload::Failure(_)) {
                        j.decide(decision);
                    }
                }
                break;
            }
            Action::Decide(decision) => {
                j.decide(decision);
            }
            Action::ReadRanking => panic!("unexpected ranking"),
        }
    }
    let view = j.run().hunt_groups();
    assert!(view.len() >= 2, "{view:?}");
    assert!(
        view[0].carried,
        "a later live failure rewrote group 1: {view:?}"
    );
    j.assert_resumes();
}

#[test]
fn a_hunt_start_with_a_malformed_ranking_is_rejected_and_leaves_the_state() {
    use crate::{FoldError, RankingError};
    let mut j = limit_session(3);
    fail_all_together(&mut j, SHORT);
    let (start, cause) = j.hunt_start_payload();
    let cases: [(&str, Vec<u8>, RankingError); 3] = [
        ("duplicate", vec![0, 1, 1], RankingError::Duplicate(core(1))),
        ("missing", vec![0, 1], RankingError::Missing(core(2))),
        ("unknown", vec![0, 1, 2, 9], RankingError::Unknown(core(9))),
    ];
    for (name, order, error) in cases {
        let mut malformed = start.clone();
        malformed.ranking = order.into_iter().map(core).collect();
        let event = j.event(malformed.into(), &cause);
        let mut folded = j.state.clone();
        assert_eq!(
            folded.fold(&event),
            Err(FoldError::Ranking(error)),
            "{name}"
        );
        assert_eq!(folded.run(), j.state.run(), "{name}");
    }
    j.append(start.into(), &cause);
    assert!(j.run().hunts.active.is_some());
    j.assert_resumes();
}

#[test]
fn a_rejected_core_phase_keeps_the_hunt_a_queued_reset_would_close() {
    use crate::FoldError;
    let mut j = hunt_of(2, SHORT);
    let end = j.run_hunt(100, pass_all);
    ended(&end, HuntOutcome::Fallback);
    j.reset(0);
    let transition = CoreTransition {
        core: core(0),
        from: None,
        to: CorePhase::Search,
        offset: off(-30),
        pass: None,
        failure_point: None,
        check_solo_limit: true,
        workloads: Vec::new(),
        cleared_combination: Vec::new(),
        reason: "test".to_owned(),
    };
    let event = j.event(transition.into(), &[]);
    let mut folded = j.state.clone();
    assert_eq!(folded.fold(&event), Err(FoldError::BadCandidate(core(0))));
    assert_eq!(folded.run(), j.state.run());
    assert!(j.run().hunts.active.as_ref().unwrap().end.is_some());
}

fn known_failure_hunt(passes_after: usize, config_after_start: &str) -> (Journal, Seq, Seq) {
    let mut j = Journal::new(&[0, 1], &[Start::has_room(-10); 2]);
    j.profile_change(&[-10, -10]);
    j.ranking(&[0, 1]);
    let plan = together_plan(&mut j, &[0, 1], &[-10, -10], SHORT);
    let known = j.trial(&plan, Outcome::Failure);
    attribute(&mut j);
    for _ in 0..passes_after {
        j.trial(&plan, Outcome::Pass);
    }
    start_hunt_at(&mut j, 1, &[0, 1], &[-10, -10]);
    j.load(&config(config_after_start));
    let group = group_event(1, 1, &[0, 1], j.profile(&[-10, -10]), HuntStage::Full);
    let group_seq = j.append(group.into(), &[]);
    (j, known, group_seq)
}

#[test]
fn the_known_failure_gate_reads_the_threshold_frozen_at_the_hunt_start() {
    let lower = r#"{"evidence":{"miss":0.9,"rate":0.5}}"#;
    let higher = r#"{"evidence":{"miss":0.001,"rate":0.5}}"#;
    for (config_text, passes, replaced) in [
        (lower, 3, true),
        (higher, 7, false),
        (lower, 5, false),
        (higher, 4, true),
    ] {
        let (j, known, group_seq) = known_failure_hunt(passes, config_text);
        let hunt = j.run().hunts.active.as_ref().unwrap().needed.get();
        let configured = j.run().config.needed.get();
        assert_eq!(hunt, 5);
        assert_ne!(configured, hunt, "{config_text}");
        let action = j.next();
        if replaced {
            let decision = expect_decision(action.clone());
            let group = group_in(&decision);
            assert_eq!(
                group.inferred,
                Some(Inference::Failure),
                "{config_text} {passes}"
            );
            assert_eq!(decision.cause, [group_seq, known]);
        } else {
            assert!(
                matches!(action, Action::RunTrial(_)),
                "{config_text} {passes}: {action:?}"
            );
        }
        assert_eq!(j.resumed().next(), action);
        j.assert_resumes();
    }
}

#[test]
fn replayed_hunt_trials_clamp_to_the_nonzero_evidence_range() {
    for (trials, needed) in [(0, 1), (1, 1), (65_535, 65_535), (65_536, 65_535)] {
        let mut j = limit_session(3);
        fail_all_together(&mut j, SHORT);
        let (mut start, cause) = j.hunt_start_payload();
        start.trials = trials;
        j.append(start.into(), &cause);
        assert_eq!(j.run().hunts.active.as_ref().unwrap().needed.get(), needed);
        let group = j.next_decision();
        j.decide(group);
        assert_eq!(j.next_trial().requirement.needed.get(), needed);
        j.assert_resumes();
    }
}

#[test]
fn a_hunt_start_rejects_a_workload_from_another_regime_without_mutating_the_run() {
    let mut j = limit_session(3);
    fail_all_together(&mut j, SHORT);
    let (mut start, cause) = j.hunt_start_payload();
    start.workload = workloads(Regime::R3)[0].id();
    let expected = crate::FoldError::WorkloadRegime {
        regime: start.regime,
        workload: start.workload,
    };
    let event = j.event(start.into(), &cause);
    let mut folded = j.state.clone();
    assert_eq!(folded.fold(&event), Err(expected));
    assert_eq!(folded.run(), j.state.run());
}

#[test]
fn an_empty_hunt_load_replays_as_all_topology_cores() {
    let mut j = limit_session(3);
    fail_all_together(&mut j, SHORT);
    let (mut start, cause) = j.hunt_start_payload();
    start.cores = set(&[]);
    j.append(start.into(), &cause);
    assert_eq!(
        j.run().hunts.active.as_ref().unwrap().class.loaded(),
        set(&ids(3))
    );
    let group = j.next_decision();
    j.decide(group);
    assert_eq!(j.next_trial().load, Load::Many(set(&ids(3))));
    j.assert_resumes();
}

#[test]
fn groups_and_ends_for_another_hunt_do_not_change_the_active_hunt() {
    for active in [false, true] {
        let mut j = limit_session(3);
        if active {
            fail_all_together(&mut j, SHORT);
            j.start_hunt();
        }
        let before = j.run().hunts.clone();
        let group = group_event(1, 2, &[0], j.profile(&[-30, 0, 0]), HuntStage::Part);
        j.append(group.into(), &[]);
        let end = HuntEnd {
            hunt: std::num::NonZeroU32::new(2).unwrap(),
            result: HuntOutcome::Culprit,
            cores: set(&[0]),
            members: Vec::new(),
            groups: 1,
            reason: "test".to_owned(),
        };
        j.append(end.into(), &[]);
        assert_eq!(j.run().hunts, before, "active {active}");
        j.assert_resumes();
    }
}

#[test]
fn the_recent_constraint_core_must_be_a_candidate_for_the_repeated_probe() {
    let mut j = limit_session(4);
    fail_all_together(&mut j, SHORT);
    let (mut start, cause) = j.hunt_start_payload();
    assert_eq!(j.run().constraints.recent(), set(&[3]));
    start.candidates = set(&[0, 1, 2]);
    start.parked = j.profile(&[0, 0, 0, -30]).offsets();
    j.append(start.into(), &cause);
    assert_eq!(j.run().hunts.active.as_ref().unwrap().repeated, None);
    let (group, _) = j.group_plan();
    assert_eq!(group.cores, set(&[0, 1]));
    assert_eq!(group.granularity, 2);
    j.assert_resumes();
}

#[test]
fn an_unanswered_hunt_group_has_no_drain_commitment() {
    let mut j = hunt_of(3, SHORT);
    assert_eq!(j.run().hunt_closing(), None);
    let planned = j.next_decision();
    j.decide(planned);
    let plan = j.next_trial();
    for _ in 0..4 {
        assert_eq!(j.run().hunt_closing(), None);
        j.trial(&plan, Outcome::Pass);
    }
    assert_eq!(j.run().hunt_closing(), None);
    assert!(matches!(j.run().hunt_next(), Some(Action::RunTrial(_))));
    j.assert_resumes();
}

#[test]
fn a_direct_hunt_end_does_not_create_a_second_hunt_commitment() {
    let mut j = hunt_of(3, SHORT);
    manual_end(&mut j, HuntOutcome::Direct, &[0]);
    assert_eq!(j.run().hunt_closing(), None);
    assert_eq!(j.run().hunt_next(), None);
    j.assert_resumes();
}

#[test]
fn a_culprit_commitment_never_deepens_an_already_shallower_core() {
    let mut j = limited([-28, -30]);
    start_hunt_at(&mut j, 1, &[0, 1], &[-30, -30]);
    manual_end(&mut j, HuntOutcome::Culprit, &[0]);
    let commitment = j.run().hunt_closing().unwrap();
    let moved = backoff_of(&commitment);
    assert_eq!((moved.from_offset, moved.to_offset), (off(-28), off(-28)));
    assert_eq!(moved.failure_point, Some(off(-29)));
    assert_eq!(moved.pass, Some(off(-28)));
    j.decide(commitment);
    assert!(j.run().hunts.active.is_none());
    assert_eq!(current(&j), [-28, -30]);
    j.assert_resumes();
}

#[test]
fn a_recorded_combination_needs_no_backoff_after_the_checking_profile_breaks_it() {
    let mut j = limited([-30, -30]);
    start_hunt_at(&mut j, 1, &[0, 1], &[-30, -30]);
    manual_end(&mut j, HuntOutcome::Combination, &[0, 1]);
    j.combination(1, &[(0, -30), (1, -30)]);
    assert!(j.run().hunt_closing().is_some());
    j.profile_change(&[-29, -30]);
    assert!(j.run().hunts.active.is_some());
    assert_eq!(j.run().hunt_closing(), None);
    assert_eq!(j.run().hunt_next(), None);
    j.assert_resumes();
}

#[test]
fn a_combination_commitment_returns_no_unsafe_move_when_no_safe_profile_exists() {
    let mut j = limited([-30, -30]);
    start_hunt_at(&mut j, 1, &[0, 1], &[-30, -30]);
    manual_end(&mut j, HuntOutcome::Combination, &[0, 1]);
    j.combination(1, &[(0, -30), (1, -30)]);
    j.core_phase(CoreTransition {
        core: core(0),
        from: Some(CorePhase::AtLimit),
        to: CorePhase::AtLimit,
        offset: off(-30),
        pass: None,
        failure_point: Some(off(0)),
        check_solo_limit: false,
        workloads: Vec::new(),
        cleared_combination: Vec::new(),
        reason: "test".to_owned(),
    });
    assert_eq!(j.run().hunt_closing(), None);
    assert_eq!(j.run().hunt_next(), None);
    assert_eq!(current(&j), [-30, -30]);
    j.assert_resumes();
}

#[test]
fn an_open_hunt_blocks_another_queued_hunt_and_locate() {
    let mut j = hunt_of(3, SHORT);
    let profile = j.offsets().offsets();
    let queued = j.idle_failure(profile, Condition::Together);
    assert!(j.run().failures.queue().any(|seq| seq == queued));
    assert_eq!(j.run().hunt_start_action(), None);
    assert_eq!(j.run().locate_action(), None);
    j.assert_resumes();
}

#[test]
fn a_paired_hunt_preserves_the_second_failures_nonempty_reason() {
    let mut j = crate::later_tests::later_session(&[-10, -12]);
    crate::later_tests::start_cycle(&mut j);
    let first = crate::later_tests::idle_failure(&mut j);
    let held = j.next_decision();
    j.decide(held);
    let profile = j.offsets().offsets();
    let second = j.failure(
        togi_journal::Failure {
            signal: togi_machine::Signal::Crash,
            attribution: Attribution::Unattributed,
            core: None,
            offset: None,
            trial: None,
            regime: Some(Regime::R6),
            condition: Some(Condition::Together),
            profile: Some(profile),
            known_failure: None,
            reason: Some("observed idle crash".to_owned()),
            round: None,
        },
        &[],
    );
    let decision = crate::later_tests::drive_to_hunt_start(&mut j);
    let DecisionPayload::HuntStart(start) = &decision.payload else {
        panic!("expected the paired hunt start, got {decision:?}");
    };
    assert_eq!(decision.cause, [second, first]);
    let reason = start.reason.as_deref().unwrap();
    assert!(reason.starts_with("observed idle crash; second failure within 5 cycles"));
    assert!(reason.contains(&format!("#{first}")));
    assert!(reason.contains(&format!("#{second}")));
    j.decide(decision);
    j.assert_resumes();
}

#[test]
fn a_warned_idle_failure_still_opens_its_hunt() {
    for (name, pass_profile, warns) in [
        ("equal profile", [-10, -12], true),
        ("deeper passing profile", [-11, -13], true),
        ("shallower passing profile", [-9, -11], false),
    ] {
        let mut j = super::support::hunt_session(&[-10, -12]);
        let passes: Vec<Seq> = (0..j.run().config.needed.get())
            .map(|_| {
                j.carry(
                    CARRIED_SESSION,
                    (Regime::R6, 0, &[0, 1], SHORT),
                    Condition::Together,
                    &pass_profile,
                    Outcome::Pass,
                )
            })
            .collect();
        let profile = j.profile(&[-10, -12]).offsets();
        let failure = j.idle_failure(profile, Condition::Together);
        let mut warned = false;
        let mut started = None;
        for _ in 0..20 {
            let action = j.next_decision();
            match &action.payload {
                DecisionPayload::TunerWarning(warning) => {
                    assert_eq!(warning.passes, passes, "{name}");
                    assert_eq!(action.cause[0], failure, "{name}");
                    warned = true;
                }
                DecisionPayload::HuntStart(start) => {
                    started = Some(start.failure);
                    break;
                }
                _ => {}
            }
            j.decide(action);
        }
        assert_eq!(warned, warns, "{name}");
        assert_eq!(started, Some(failure), "{name}");
        j.assert_resumes();
    }
}

fn before_first_member_probe() -> Journal {
    let mut j = hunt_of(4, 600);
    for _ in 0..200 {
        if group_of(&j.next()).is_some_and(|group| group.probe.is_some()) {
            return j;
        }
        j.step(&mut |plan| {
            if at(plan, 0) <= -30 && at(plan, 1) <= -30 {
                Outcome::Failure
            } else {
                Outcome::Pass
            }
        });
    }
    panic!("the hunt never reached its first member probe");
}

#[test]
fn a_skipped_member_probe_ends_with_the_known_failing_members_and_explains_the_stop() {
    let mut j = before_first_member_probe();
    let (mut group, cause) = j.group_plan();
    assert_eq!(group.probe.unwrap().core, core(0));
    group.skipped = true;
    group.inferred = None;
    j.append(group.into(), &cause);
    let action = j.run().hunt_next().unwrap();
    let end = end_of(&action).unwrap();
    assert_eq!(end.result, HuntOutcome::Combination);
    assert_eq!(end.members, members_of(&[(0, -30), (1, -30)]));
    assert!(
        end.reason
            .contains("member probes stopped at a skipped group")
    );
    assert!(end.reason.contains("core 00 -30") && end.reason.contains("core 01 -30"));
    j.assert_resumes();
}

#[test]
fn an_already_broken_combination_ends_without_starting_member_probes() {
    let mut j = before_first_member_probe();
    j.profile_change(&[-29, -30, -30, -30]);
    let action = j.run().hunt_next().unwrap();
    let end = end_of(&action).unwrap();
    assert_eq!(end.result, HuntOutcome::Combination);
    assert_eq!(end.cores, set(&[0, 1]));
    assert!(end.members.is_empty());
    assert!(!end.reason.contains("member probes"));
    j.assert_resumes();
}

#[test]
fn an_already_broken_combination_shows_every_member_done_in_the_plan() {
    let mut j = before_first_member_probe();
    let ladder = |j: &Journal| -> Vec<(togi_machine::CoreId, i64, bool)> {
        j.state
            .hunt_plan()
            .unwrap()
            .probes
            .iter()
            .map(|probe| (probe.member, probe.offset.get(), probe.done))
            .collect()
    };
    assert!(ladder(&j).iter().any(|&(_, _, done)| !done));
    j.profile_change(&[-29, -30, -30, -30]);
    assert_eq!(ladder(&j), [(core(0), -30, true), (core(1), -30, true)]);
    j.assert_resumes();
}

#[test]
fn a_combination_before_any_checking_profile_is_recorded_without_the_shallower_note() {
    let starts = [-30, -30].map(|offset| Start::at_limit(offset).fail(offset - 1).pass(offset));
    let mut j = Journal::new(&[0, 1], &starts);
    assert!(j.run().checking.profile().is_none());
    start_hunt_at(&mut j, 1, &[0, 1], &[-30, -30]);
    manual_end(&mut j, HuntOutcome::Combination, &[0, 1]);
    let record = j.next_decision();
    let DecisionPayload::Combination(combination) = &record.payload else {
        panic!("expected the combination, got {record:?}");
    };
    assert_eq!(combination.reason, "hunt 1 found a combined failure");
    j.decide(record);
    j.assert_resumes();
}

#[test]
fn a_core_reset_drops_live_parked_failures_and_those_of_classes_loading_it() {
    use super::state::{Hunts, ParkedFailure};
    let secs = crate::Secs::new(std::num::NonZeroU32::new(SHORT).unwrap());
    let class = |ids: &[u8]| crate::ClassKey::new(workloads(Regime::R3)[0].id(), set(ids), secs);
    let parked = |at: u64, carried: bool| ParkedFailure {
        seq: Seq::new(at).unwrap(),
        core: core(1),
        offset: off(-10),
        carried,
    };
    let mut hunts = Hunts::default();
    hunts.note_parked_failure(class(&[0]), parked(1, true));
    hunts.note_parked_failure(class(&[0]), parked(2, false));
    hunts.note_parked_failure(class(&[2]), parked(3, false));
    hunts.note_parked_failure(class(&[1]), parked(4, true));
    let reset = Seq::new(9).unwrap();
    hunts.reset_core(reset, core(1));
    assert_eq!(hunts.reset, Some(reset));
    assert_eq!(hunts.parked_failures.len(), 1);
    let survivor = hunts.parked_failures.get(&class(&[0])).unwrap();
    assert_eq!(survivor, &{
        let mut only = super::state::Parked::default();
        only.add(parked(1, true));
        only
    });
}
