use std::collections::BTreeSet;

use togi_journal::{
    Activity, Attribution, Decision, Event, Failure, HuntEnd, HuntGroup, HuntOutcome, HuntStage,
    HuntStart, Inference, Outcome, Payload, Seq, TunerDecision,
};
use togi_machine::{Condition, Regime, Signal};

use super::plan::GroupOutcome;
use super::support::hunt_session;
use crate::testkit::{Journal, Start, fresh, set};
use crate::tests::{core, off};
use crate::{Action, DecisionPayload, Load, TrialPlan};

const SESSION: &str = "20261001T000000Z";
const ALL: [u8; 4] = [0, 1, 2, 3];

type Step = (HuntStage, [i64; 4], Option<(u8, i64)>, bool);

fn value(plan: &TrialPlan, id: u8) -> i64 {
    plan.profile.get(core(id)).unwrap().get()
}

fn joint_fails(plan: &TrialPlan) -> bool {
    value(plan, 0) <= -30 && value(plan, 1) <= -27
}

fn joint_oracle(plan: &TrialPlan) -> Outcome {
    if joint_fails(plan) {
        Outcome::Failure
    } else {
        Outcome::Pass
    }
}

fn limit_session() -> Journal {
    let starts = [Start::at_limit(-30).fail(-31); 4];
    let mut j = Journal::new(&ALL, &starts);
    j.load(&crate::testkit::config(
        r#"{"durations":{"checking_idle_s":120}}"#,
    ));
    j.profile_change(&[-30; 4]);
    j.ranking(&ALL);
    j
}

fn joint_session() -> (Journal, Seq) {
    let mut j = limit_session();
    let carried = j.carry(
        SESSION,
        (Regime::R6, 0, &ALL, 120),
        Condition::Together,
        &[-30; 4],
        Outcome::Failure,
    );
    (j, carried)
}

fn skip_known(j: &mut Journal, carried: Seq) -> Seq {
    let plan = j.manual(
        Regime::R6,
        0,
        Load::Many(set(&ALL)),
        Condition::Together,
        &[-30; 4],
        120,
    );
    let Action::Decide(skip) = j.run().skip_known_failure(Action::RunTrial(plan)) else {
        panic!("the known failure did not skip the trial");
    };
    let DecisionPayload::Failure(failure) = &skip.payload else {
        panic!("expected a failure, got {skip:?}");
    };
    assert_eq!(failure.known_failure, Some(carried));
    assert_eq!(skip.cause, [carried]);
    j.decide(skip)
}

fn seq_of(j: &Journal) -> Seq {
    Seq::new(j.events.len() as u64).unwrap()
}

fn is_hunt_decision(payload: &DecisionPayload) -> bool {
    matches!(payload, DecisionPayload::TunerDecision(td) if td.activity == Activity::Hunt)
}

fn joint_hunt(mut decided: impl FnMut(&mut Journal, Seq, &DecisionPayload)) -> (Journal, Seq) {
    let (mut j, carried) = joint_session();
    skip_known(&mut j, carried);
    let mut oracle = joint_oracle;
    for _ in 0..500 {
        let action = j.step(&mut oracle);
        if let Action::Decide(decision) = action {
            let seq = seq_of(&j);
            decided(&mut j, seq, &decision.payload);
            if is_hunt_decision(&decision.payload) {
                j.assert_resumes();
                return (j, carried);
            }
        }
    }
    panic!("the hunt never backed off");
}

fn reaches(events: &[Event], cause: &[Seq]) -> BTreeSet<Seq> {
    let mut seen = BTreeSet::new();
    let mut todo = cause.to_vec();
    while let Some(seq) = todo.pop() {
        if !seen.insert(seq) {
            continue;
        }
        if let Some(event) = events.get(usize::try_from(seq.get()).unwrap() - 1) {
            todo.extend(event.cause.iter().copied());
        }
    }
    seen
}

fn hunt_failures(events: &[Event], hunt: u32) -> Vec<Seq> {
    let mut hunted = BTreeSet::new();
    let mut out = Vec::new();
    for event in events {
        match &event.payload {
            Payload::TrialIntent(intent) if intent.hunt.is_some_and(|h| h.get() == hunt) => {
                hunted.insert(intent.trial.clone());
            }
            Payload::TrialEnd(end)
                if hunted.contains(&end.trial) && end.outcome == Outcome::Failure =>
            {
                out.push(event.seq);
            }
            _ => {}
        }
    }
    out
}

struct Commitments {
    start: (Seq, HuntStart),
    end: (Seq, HuntEnd, Vec<Seq>),
    combination: (Seq, Vec<Seq>),
    backoff: (Seq, TunerDecision, Vec<Seq>),
}

fn commitments(events: &[Event]) -> Commitments {
    let mut start = None;
    let mut end = None;
    let mut combination = None;
    let mut backoff = None;
    for event in events {
        match &event.payload {
            Payload::HuntStart(payload) => start = Some((event.seq, payload.clone())),
            Payload::HuntEnd(payload) => {
                end = Some((event.seq, payload.clone(), event.cause.clone()));
            }
            Payload::Combination(_) => combination = Some((event.seq, event.cause.clone())),
            Payload::TunerDecision(td) if td.activity == Activity::Hunt => {
                backoff = Some((event.seq, td.clone(), event.cause.clone()));
            }
            _ => {}
        }
    }
    Commitments {
        start: start.expect("a hunt start"),
        end: end.expect("a hunt end"),
        combination: combination.expect("a combination"),
        backoff: backoff.expect("a hunt backoff"),
    }
}

fn assert_reaches_every_decisive_failure(
    events: &[Event],
    commitments: &Commitments,
    carried: Seq,
    failures: &[Seq],
) {
    let cited = [
        ("hunt.end", commitments.end.0, &commitments.end.2),
        (
            "combination",
            commitments.combination.0,
            &commitments.combination.1,
        ),
        ("backoff", commitments.backoff.0, &commitments.backoff.2),
    ];
    for (name, seq, cause) in cited {
        let reached = reaches(events, cause);
        assert!(
            reached.contains(&carried),
            "{name} #{seq} cause {cause:?} does not reach the carried failure #{carried}"
        );
        let unreached: Vec<Seq> = failures
            .iter()
            .copied()
            .filter(|failure| !reached.contains(failure))
            .collect();
        assert!(
            unreached.is_empty(),
            "{name} #{seq} cause {cause:?} does not reach live failures {unreached:?}"
        );
    }
}

#[test]
fn hunt_commitments_cite_decisive_live_failures() {
    let (j, carried) = joint_hunt(|_, _, _| {});
    let found = commitments(&j.events);
    assert!(
        reaches(
            &j.events,
            &j.events[usize::try_from(found.start.0.get()).unwrap() - 1].cause
        )
        .contains(&carried)
    );
    assert_eq!(
        found
            .end
            .1
            .members
            .iter()
            .map(|member| (member.core, member.offset))
            .collect::<Vec<_>>(),
        [(core(0), off(-30)), (core(1), off(-27))]
    );
    let failures = hunt_failures(&j.events, found.start.1.hunt.get());
    let mut want = vec![found.start.0];
    want.extend(&failures);
    assert_eq!(found.end.2, want);
    assert_eq!(failures.len(), 4);
    assert_reaches_every_decisive_failure(&j.events, &found, carried, &failures);
    j.assert_resumes();
}

#[test]
fn hunt_live_failure_causes_native_fixture() {
    let (mut j, carried) = joint_session();
    let known = skip_known(&mut j, carried);
    assert_eq!(
        j.events[usize::try_from(known.get()).unwrap() - 1].cause,
        [carried]
    );
    let (start_seq, start) = j.start_hunt();
    assert_eq!(start.candidates, set(&ALL));
    assert_eq!(start.failure, carried);
    assert_eq!(
        j.events[usize::try_from(start_seq.get()).unwrap() - 1].cause,
        [carried]
    );
    let n = j.run().config.needed.get();
    let mut failures = Vec::new();
    let mut groups = Vec::new();
    let expected: [Step; 9] = [
        (HuntStage::Part, [0, 0, -30, -30], None, false),
        (HuntStage::Part, [-30, -30, 0, 0], None, true),
        (HuntStage::Part, [-30, 0, 0, 0], None, false),
        (HuntStage::Part, [0, -30, 0, 0], None, false),
        (HuntStage::Probe, [-29, -30, 0, 0], Some((0, -29)), false),
        (HuntStage::Probe, [-30, -29, 0, 0], Some((1, -29)), true),
        (HuntStage::Probe, [-30, -28, 0, 0], Some((1, -28)), true),
        (HuntStage::Probe, [-30, -26, 0, 0], Some((1, -26)), false),
        (HuntStage::Probe, [-30, -27, 0, 0], Some((1, -27)), true),
    ];
    for (at, (stage, profile, probe, fails)) in expected.into_iter().enumerate() {
        let (group, _) = j.group_plan();
        assert_eq!(group.group.get() as usize, at + 1);
        assert_eq!(group.stage, stage, "group {}", at + 1);
        assert_eq!(
            group.profile,
            j.profile(&profile).offsets(),
            "group {}",
            at + 1
        );
        assert_eq!(
            group
                .probe
                .map(|member| (member.core.get(), member.offset.get())),
            probe,
            "group {}",
            at + 1
        );
        let decision = j.next_decision();
        groups.push(j.decide(decision));
        for _ in 0..if fails { 1 } else { n } {
            let plan = j.next_trial();
            if fails {
                failures.push(j.trial(&plan, Outcome::Failure));
                let attribution = j.next_decision();
                j.decide(attribution);
            } else {
                j.trial(&plan, Outcome::Pass);
            }
        }
    }
    assert_eq!(failures.len(), 4);
    let decision = j.next_decision();
    let DecisionPayload::HuntEnd(end) = decision.payload else {
        panic!("expected the hunt end, got {decision:?}");
    };
    let cause = decision.cause;
    assert_eq!(end.result, HuntOutcome::Combination);
    assert_eq!(end.groups, 9);
    let mut want = vec![start_seq];
    want.extend(&failures);
    assert_eq!(cause, want);
    let end_seq = j.append(end.into(), &cause);
    let Action::Decide(record) = j.next() else {
        panic!("expected the combination");
    };
    assert!(matches!(record.payload, DecisionPayload::Combination(_)));
    assert_eq!(record.cause, [end_seq]);
    let combination = j.decide(record);
    let Action::Decide(backoff) = j.next() else {
        panic!("expected the backoff");
    };
    let DecisionPayload::TunerDecision(td) = &backoff.payload else {
        panic!("expected the backoff, got {backoff:?}");
    };
    assert_eq!(
        (td.decision, td.activity, td.core, td.to_offset),
        (Decision::Backoff, Activity::Hunt, core(0), off(-29))
    );
    assert_eq!(backoff.cause, [combination, groups[4]]);
    let backoff_seq = j.decide(backoff.clone());
    let found = commitments(&j.events);
    assert_eq!(found.backoff.0, backoff_seq);
    assert_reaches_every_decisive_failure(&j.events, &found, carried, &failures);
    j.assert_resumes();
}

#[test]
fn hunt_causes_omit_a_failure_after_the_group_passed() {
    let mut late: Option<Seq> = None;
    let (j, _) = joint_hunt(|j, _, payload| {
        let DecisionPayload::HuntGroup(group) = payload else {
            return;
        };
        if group.group.get() != 2 || late.is_some() {
            return;
        }
        let first = &j.run().hunt_groups()[0];
        assert!(first.probe.is_none());
        assert_eq!(first.outcome, Some(GroupOutcome::Passed), "{first:?}");
        let profile = first.profile.offsets();
        late = Some(j.failure(
            Failure {
                signal: Signal::Crash,
                attribution: Attribution::Unattributed,
                core: None,
                offset: None,
                trial: None,
                regime: Some(Regime::R6),
                condition: Some(Condition::Parked),
                profile: Some(profile),
                known_failure: None,
                reason: None,
                round: None,
            },
            &[],
        ));
    });
    let late = late.expect("the hunt never recorded group 2");
    for event in &j.events[usize::try_from(late.get()).unwrap()..] {
        let hunt_event = match &event.payload {
            Payload::HuntGroup(_) | Payload::HuntEnd(_) | Payload::Combination(_) => true,
            Payload::TunerDecision(td) => td.activity == Activity::Hunt,
            _ => false,
        };
        assert!(
            !hunt_event || !event.cause.contains(&late),
            "#{} cause {:?} cites failure #{late}, recorded after group 1 passed",
            event.seq,
            event.cause
        );
    }
    j.assert_resumes();
}

#[test]
fn hunt_causes_omit_a_failure_covered_before_the_group_passed() {
    let mut covered: Option<Seq> = None;
    let mut passes: Vec<Seq> = Vec::new();
    let (j, _) = joint_hunt(|j, _, payload| {
        let DecisionPayload::HuntGroup(group) = payload else {
            return;
        };
        if group.group.get() != 1 || covered.is_some() {
            return;
        }
        let profile: Vec<i64> = ALL
            .iter()
            .map(|&id| group.profile.get(core(id)).unwrap().get())
            .collect();
        let class = (Regime::R6, 0, &ALL[..], group.duration_s);
        covered = Some(j.carry(
            SESSION,
            class,
            Condition::Parked,
            &profile,
            Outcome::Failure,
        ));
        let n = j.run().config.needed.get();
        for _ in 0..n {
            passes.push(j.carry(SESSION, class, Condition::Parked, &profile, Outcome::Pass));
        }
        assert_eq!(
            j.run().hunt_groups()[0].outcome,
            Some(GroupOutcome::Passed),
            "the carried passes should pass group 1"
        );
    });
    let covered = covered.expect("the hunt never recorded group 1");
    let second = j
        .events
        .iter()
        .position(|event| matches!(&event.payload, Payload::HuntGroup(g) if g.group.get() == 2))
        .expect("group 2");
    for event in &j.events[second..] {
        let cites_passes = passes.iter().any(|pass| event.cause.contains(pass));
        match &event.payload {
            Payload::HuntGroup(_) | Payload::HuntEnd(_) => {
                assert!(
                    cites_passes,
                    "#{} cause {:?} cites none of group 1's carried passes {passes:?}",
                    event.seq, event.cause
                );
            }
            Payload::TunerDecision(td) if td.activity != Activity::Hunt => continue,
            Payload::TunerDecision(_) => {}
            _ => continue,
        }
        assert!(
            !event.cause.contains(&covered),
            "#{} cause {:?} cites failure #{covered}, covered before group 1 passed",
            event.seq,
            event.cause
        );
    }
    j.assert_resumes();
}

#[test]
fn hunt_end_cites_a_live_failure_inferred_group() {
    let mut j = hunt_session(&[-10, -10]);
    let plan = j.manual(
        Regime::R6,
        0,
        Load::Many(set(&[0, 1])),
        Condition::Together,
        &[-10, 0],
        120,
    );
    let failure = j.trial(&plan, Outcome::Failure);
    let start = HuntStart {
        hunt: std::num::NonZeroU32::MIN,
        failure,
        trial: None,
        regime: Regime::R6,
        workload: plan.workload_at(0),
        cores: set(&[0, 1]),
        duration_s: 120,
        failing: j.profile(&[-10, -10]).offsets(),
        parked: j.profile(&[0, 0]).offsets(),
        parked_seq: None,
        candidates: set(&[0, 1]),
        trials: u32::from(j.run().config.needed.get()),
        trial_s: 120,
        miss: j.run().config.miss,
        rate: j.run().config.rate,
        ranking: vec![core(0), core(1)],
        reason: None,
    };
    let start_seq = j.append(start.into(), &[failure]);
    let Some(Action::Decide(group)) = j.run().hunt_next() else {
        panic!("the hunt planned no group");
    };
    let DecisionPayload::HuntGroup(payload) = &group.payload else {
        panic!("expected a group, got {group:?}");
    };
    assert_eq!(payload.inferred, Some(Inference::Failure), "{payload:?}");
    assert_eq!(payload.profile, j.profile(&[-10, 0]).offsets());
    assert_eq!(group.cause, [start_seq, failure]);
    let group_seq = j.decide(group);
    let Some(Action::Decide(end)) = j.run().hunt_next() else {
        panic!("the hunt did not end");
    };
    let DecisionPayload::HuntEnd(payload) = &end.payload else {
        panic!("expected an end, got {end:?}");
    };
    assert_eq!(payload.result, HuntOutcome::Culprit);
    assert_eq!(end.cause, [start_seq, group_seq]);
    j.assert_resumes();
}

fn part_order(stage: HuntStage, granularity: u32) -> Vec<Vec<u8>> {
    if granularity == 2 {
        vec![vec![2, 3], vec![0, 1]]
    } else if stage == HuntStage::Part {
        vec![vec![3], vec![0], vec![1], vec![2]]
    } else {
        vec![vec![0, 1, 2], vec![1, 2, 3], vec![0, 2, 3], vec![0, 1, 3]]
    }
}

fn assert_fixed_group(where_: &str, group: &HuntGroup) {
    if !matches!(group.stage, HuntStage::Part | HuntStage::Complement) || group.probe.is_some() {
        return;
    }
    let order = part_order(group.stage, group.granularity);
    let cores: Vec<u8> = group.cores.iter().map(togi_machine::CoreId::get).collect();
    let index = group.index as usize;
    assert!(
        index < order.len() && cores == order[index],
        "{where_}: group {} ({:?}, granularity {}, index {}) plans cores {cores:?}, want the part fixed at hunt.start: {order:?}",
        group.group,
        group.stage,
        group.granularity,
        group.index
    );
}

fn assert_fixed_plan(where_: &str, state: &crate::State) {
    if let Ok(Action::Decide(decision)) = state.next() {
        if let DecisionPayload::HuntGroup(group) = &decision.payload {
            assert_fixed_group(where_, group);
        }
    }
    let Some(run) = state.run() else { return };
    let allowed: Vec<Vec<u8>> = [
        part_order(HuntStage::Part, 2),
        part_order(HuntStage::Part, 4),
        part_order(HuntStage::Complement, 4),
    ]
    .concat();
    for view in run.hunt_groups() {
        if matches!(view.stage, HuntStage::Part | HuntStage::Complement) && view.probe.is_none() {
            let cores: Vec<u8> = view.cores.iter().map(togi_machine::CoreId::get).collect();
            assert!(
                allowed.contains(&cores),
                "{where_}: dashboard group {} runs cores {cores:?}, not a part fixed at hunt.start",
                view.number
            );
        }
    }
}

fn crashed_application(j: &mut Journal, id: u8) {
    let mut values = [0; 4];
    values[usize::from(id)] = -30;
    let profile = j.profile(&values).offsets();
    let failure = j.failure(
        Failure {
            signal: Signal::Crash,
            attribution: Attribution::Attributed,
            core: Some(core(id)),
            offset: Some(off(-30)),
            trial: None,
            regime: Some(Regime::R6),
            condition: Some(Condition::Together),
            profile: Some(profile),
            known_failure: None,
            reason: None,
            round: None,
        },
        &[],
    );
    j.decision(TunerDecision {
        core: core(id),
        activity: Activity::Checking,
        decision: Decision::Backoff,
        from_offset: off(-30),
        to_offset: off(-29),
        pass: None,
        failure_point: Some(off(-30)),
        workloads: Vec::new(),
        reason: format!("failure #{failure} named core {id:02}"),
    });
}

#[test]
fn hunt_keeps_one_part_order_when_recent_moves_mid_hunt() {
    for crash_at in [1u32, 2, 3] {
        let mut j = limit_session();
        let plan = j.manual(
            Regime::R6,
            0,
            Load::Many(set(&ALL)),
            Condition::Together,
            &[-30; 4],
            120,
        );
        j.trial(&plan, Outcome::Failure);
        let attribution = j.next_decision();
        j.decide(attribution);
        let (_, start) = j.start_hunt();
        assert_eq!(start.candidates, set(&ALL));
        assert_eq!(j.run().constraints.recent(), set(&[3]));
        let mut crashed = false;
        let mut planned: Vec<HuntGroup> = Vec::new();
        let mut oracle = |_: &TrialPlan| Outcome::Pass;
        let mut finished = false;
        for _ in 0..400 {
            let prefix = fresh(&j.events);
            assert_eq!(
                prefix.next().ok(),
                j.state.next().ok(),
                "resume at event prefix"
            );
            assert_fixed_plan("live", &j.state);
            assert_fixed_plan("replay", &prefix);
            let action = j.step(&mut oracle);
            let Action::Decide(decision) = action else {
                continue;
            };
            match decision.payload {
                DecisionPayload::HuntGroup(group) => {
                    assert_fixed_group("planned", &group);
                    if !crashed
                        && group.stage == HuntStage::Part
                        && group.granularity == 4
                        && group.index == crash_at
                        && !group.skipped
                    {
                        crashed = true;
                        let id = group.cores.first().unwrap().get();
                        crashed_application(&mut j, id);
                        let run = j.run();
                        assert!(run.hunts.active.as_ref().unwrap().end.is_none());
                        assert_eq!(run.constraints.recent(), set(&[id]));
                    }
                    planned.push(group);
                }
                DecisionPayload::HuntEnd(_) => {
                    assert!(crashed, "the hunt ended before the crash");
                    finished = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(
            finished,
            "the hunt never ended (crash during part {crash_at})"
        );
        let got: Vec<Vec<u8>> = planned
            .iter()
            .filter(|group| group.granularity == 4)
            .map(|group| group.cores.iter().map(togi_machine::CoreId::get).collect())
            .collect();
        let want: Vec<Vec<u8>> = [
            part_order(HuntStage::Part, 4),
            part_order(HuntStage::Complement, 4),
        ]
        .concat();
        assert_eq!(got, want, "crash during part {crash_at}");
        j.assert_resumes();
    }
}
