use std::collections::BTreeMap;
use std::num::NonZeroU32;

use jiff::Timestamp;
use togi_journal::{
    Activity, CheckingChain, CycleEvent, Decision, FactSource, Outcome, ProfileChange, Rederived,
    Seq, TrialCarried, TrialClass, TrialId, TunerDecision,
};
use togi_machine::{BootId, Condition, CoreSet, PerCore, Regime, Signal, pick_workload};

use crate::testkit::{self, Journal, Start, build, fresh, set};
use crate::tests::{core, off};
use crate::{
    Action, ClassKey, DecisionPayload, FoldError, Load, Profile, RequirementKind, Secs,
    TrialContext, TrialPlan, Window, WorkloadChoice,
};

const SESSION: &str = "20230101T000000Z";

fn secs(value: u32) -> Secs {
    Secs::new(NonZeroU32::new(value).unwrap())
}

fn session(offsets: &[i64], json: &str) -> Journal {
    let ids: Vec<u8> = (0..offsets.len())
        .map(|id| u8::try_from(id).unwrap())
        .collect();
    let starts: Vec<Start> = offsets
        .iter()
        .map(|&offset| Start::at_limit(offset).fail(offset - 1).pass(offset))
        .collect();
    let mut j = Journal::new(&ids, &starts);
    j.ranking(&ids);
    j.load(&testkit::config(json));
    let change = j.next_decision();
    assert!(
        matches!(change.payload, DecisionPayload::ProfileChange(_)),
        "{change:?}"
    );
    j.decide(change);
    j
}

fn needed(j: &Journal) -> u16 {
    j.run().config.needed.get()
}

fn checking(j: &Journal) -> Profile {
    j.run().checking.profile().unwrap()
}

fn until_trial(j: &mut Journal) -> TrialPlan {
    for _ in 0..20 {
        let decision = match j.next() {
            Action::RunTrial(plan) => return plan,
            Action::Decide(decision) => decision,
            Action::ReadRanking => panic!("the tuner asked to read a ranking"),
        };
        assert!(
            matches!(
                decision.payload,
                DecisionPayload::CheckingCycle(_)
                    | DecisionPayload::CheckingStep(_)
                    | DecisionPayload::CheckingChain(_)
            ),
            "unexpected decision before the next trial: {decision:?}"
        );
        j.decide(decision);
    }
    panic!("no trial came");
}

fn ended(j: &mut Journal, plan: &TrialPlan, outcome: Outcome, named: Option<u8>) -> Seq {
    let (intent, trial) = j.intent(plan);
    let signal = (outcome == Outcome::Failure).then_some(Signal::ComputationError);
    let mut end = Journal::end_payload(&trial, outcome, signal);
    end.core = named.map(core);
    end.duration_s = plan.duration.get();
    j.append(end.into(), &[intent])
}

fn pass(j: &mut Journal, plan: &TrialPlan) -> Seq {
    ended(j, plan, Outcome::Pass, None)
}

fn fail(j: &mut Journal, plan: &TrialPlan, named: Option<u8>) -> Seq {
    ended(j, plan, Outcome::Failure, named);
    let attribution = j.next_decision();
    assert!(
        matches!(attribution.payload, DecisionPayload::Failure(_)),
        "expected the attribution of the failed trial, got {attribution:?}"
    );
    j.decide(attribution)
}

fn back_off(j: &mut Journal, failure: Seq) -> TunerDecision {
    let mut found: Option<TunerDecision> = None;
    for _ in 0..8 {
        let action = j.next_decision();
        match (&action.payload, found.clone()) {
            (DecisionPayload::TunerWarning(_) | DecisionPayload::CorePhase(_), _) => {
                j.decide(action);
            }
            (DecisionPayload::TunerDecision(decision), None) => {
                assert_eq!(decision.decision, Decision::Backoff, "{action:?}");
                assert_eq!(decision.activity, Activity::Checking, "{action:?}");
                assert!(action.cause.contains(&failure), "{action:?}");
                found = Some(decision.clone());
                j.decide(action);
            }
            (DecisionPayload::ProfileChange(_), Some(decision)) => {
                j.decide(action);
                return decision;
            }
            (DecisionPayload::TunerDecision(_), Some(_))
            | (DecisionPayload::ProfileChange(_), None)
            | (
                DecisionPayload::Failure(_)
                | DecisionPayload::DeadEnd(_)
                | DecisionPayload::CheckingCycle(_)
                | DecisionPayload::CheckingStep(_)
                | DecisionPayload::CheckingChain(_)
                | DecisionPayload::HuntStart(_)
                | DecisionPayload::HuntGroup(_)
                | DecisionPayload::HuntEnd(_)
                | DecisionPayload::HuntSkipped(_)
                | DecisionPayload::Combination(_)
                | DecisionPayload::DeepeningRound(_),
                _,
            ) => panic!("expected the backoff and its profile change, got {action:?}"),
        }
    }
    panic!("the backoff was never applied to the checking profile");
}

fn commit(j: &mut Journal, plan: &TrialPlan, named: u8) -> Seq {
    let failure = fail(j, plan, Some(named));
    back_off(j, failure);
    failure
}

fn rerun(j: &Journal) -> TrialPlan {
    let plan = j.next_trial();
    assert!(
        matches!(plan.context, TrialContext::Rerun { .. }),
        "expected a rerun, got {plan:?}"
    );
    plan
}

fn is_rerun(action: &Action) -> bool {
    matches!(action, Action::RunTrial(plan) if matches!(plan.context, TrialContext::Rerun { .. }))
}

fn is_cycle_trial(action: &Action) -> bool {
    matches!(action, Action::RunTrial(plan) if matches!(plan.context, TrialContext::Cycle { .. }))
}

fn pass_reruns(j: &mut Journal) -> Vec<TrialPlan> {
    let mut done = Vec::new();
    for _ in 0..100 {
        let action = j.next();
        let Action::RunTrial(plan) = action else {
            return done;
        };
        if !matches!(plan.context, TrialContext::Rerun { .. }) {
            return done;
        }
        pass(j, &plan);
        done.push(plan);
    }
    panic!("the reruns never ended");
}

fn carry(
    j: &mut Journal,
    class: ClassKey,
    profile: &Profile,
    outcome: Outcome,
    named: Option<u8>,
) -> Seq {
    carry_in(j, class, Condition::Together, profile, outcome, named)
}

fn carry_in(
    j: &mut Journal,
    class: ClassKey,
    condition: Condition,
    profile: &Profile,
    outcome: Outcome,
    named: Option<u8>,
) -> Seq {
    let seq = j.next_seq();
    let payload = TrialCarried {
        source: FactSource {
            session: SESSION.parse().unwrap(),
            seq,
            build: build(),
            trial: None,
            time: Timestamp::from_second(1_600_000_000).unwrap(),
            boot: BootId::from("old".to_owned()),
            backend: None,
        },
        class: TrialClass {
            regime: class.regime(),
            workload: Some(class.workload()),
            cores: class.loaded(),
            duration_s: class.duration().get(),
        },
        condition,
        activity: None,
        rerun: false,
        profile: profile.offsets(),
        outcome,
        signal: (outcome == Outcome::Failure).then_some(Signal::Crash),
        duration_s: class.duration().get(),
        core: named.map(core),
        stalled_core: None,
        voltage_requests_v: PerCore::default(),
        top_requesters: CoreSet::default(),
        ccd_mhz: BTreeMap::new(),
    };
    j.append(payload.into(), &[])
}

fn moved(id: u8, from: i64, to: i64, activity: Activity, decision: Decision) -> TunerDecision {
    TunerDecision {
        core: core(id),
        activity,
        decision,
        from_offset: off(from),
        to_offset: off(to),
        pass: None,
        failure_point: (decision == Decision::Backoff).then(|| off(from)),
        workloads: Vec::new(),
        reason: "test".to_owned(),
    }
}

fn workload_plan(template: &TrialPlan, regime: Regime, load: Load, duration: u32) -> TrialPlan {
    TrialPlan {
        regime,
        workload: WorkloadChoice::Frozen(pick_workload(regime, 0).id()),
        load,
        duration: secs(duration),
        ..template.clone()
    }
}

fn unscheduled(j: &Journal, plan: &TrialPlan) {
    let trial = TrialId::from("9999".to_owned());
    let event = j.event(plan.intent(trial.clone(), 0).into(), &plan.cause);
    let mut probe = j.state.clone();
    assert!(
        matches!(
            probe.fold(&event),
            Err(FoldError::UnscheduledIntent(found)) if found == trial
        ),
        "{plan:?}"
    );
    assert_eq!(
        probe.run(),
        j.state.run(),
        "a rejected intent changed the run"
    );
}

#[test]
fn rerun_long_failed_part() {
    let mut j = session(
        &[-10, -11],
        r#"{"checking":{"cycle":["R7"]},"durations":{"checking_all_core_s":600}}"#,
    );
    let n = needed(&j);
    let mut long = until_trial(&mut j);
    while long.duration != secs(600) {
        pass(&mut j, &long);
        long = until_trial(&mut j);
    }
    assert_eq!(long.load.cores(), set(&[0, 1]));
    let first = commit(&mut j, &long, 0);
    let start = rerun(&j);
    assert_eq!(start.duration, secs(120));
    assert_eq!(start.cause, vec![first]);
    for _ in 0..n {
        let plan = rerun(&j);
        assert_eq!(plan.duration, secs(120), "{plan:?}");
        pass(&mut j, &plan);
    }
    let again = rerun(&j);
    assert_eq!(again.duration, secs(600));
    assert_eq!(again.cause, vec![first]);
    let second = commit(&mut j, &again, 0);
    j.assert_resumes();

    let long = rerun(&j);
    assert_eq!(long.duration, secs(600), "the passes were lost: {long:?}");
    assert_eq!(long.cause, vec![second]);
    assert_eq!(long.context, TrialContext::Rerun { failure: first });
    pass(&mut j, &long);
    assert_eq!(j.run().reruns.head().map(|head| head.failure), Some(second));
    for _ in 0..n {
        let plan = rerun(&j);
        assert_eq!(plan.duration, secs(120), "{plan:?}");
        assert_eq!(plan.cause, vec![second]);
        pass(&mut j, &plan);
    }
    assert!(j.run().reruns.is_empty());
    assert!(!is_rerun(&j.next()));
    j.assert_resumes();
}

#[test]
fn rerun_repeated_commitment_cites_latest_failure() {
    let mut j = session(&[-10], r#"{"checking":{"cycle":["R1"]}}"#);
    let n = needed(&j);
    let plan = until_trial(&mut j);
    let class = plan.requirement.class;
    let first = commit(&mut j, &plan, 0);
    let again = rerun(&j);
    assert_eq!(again.cause, vec![first]);
    let second = commit(&mut j, &again, 0);
    let head = j.run().reruns.head().unwrap();
    assert_eq!((head.class, head.failure), (class, first));

    for _ in 0..n {
        let plan = rerun(&j);
        assert_eq!(plan.cause, vec![second]);
        assert_eq!(plan.context, TrialContext::Rerun { failure: first });
        pass(&mut j, &plan);
    }
    assert!(j.run().reruns.is_empty(), "{:?}", j.run().reruns);
    assert!(!is_rerun(&j.next()));
    j.assert_resumes();
}

#[test]
fn rerun_fifo_and_shared_duration() {
    let mut j = session(
        &[-10, -11, -12, -13],
        r#"{"checking":{"cycle":["R1","R1","R6","R6"]},"durations":{"checking_idle_s":600}}"#,
    );
    let n = usize::from(needed(&j));
    let first_step = until_trial(&mut j);
    let r1 = TrialPlan {
        workload: WorkloadChoice::Frozen(pick_workload(Regime::R1, 1).id()),
        context: TrialContext::Cycle {
            cycle: NonZeroU32::MIN,
            step: crate::StepNo::new(NonZeroU32::new(2).unwrap()),
            part: crate::PartNo::new(NonZeroU32::MIN),
        },
        ..first_step
    };
    assert_eq!(r1.load, Load::Single(core(0)));
    let first = commit(&mut j, &r1, 0);
    let all = set(&[0, 1, 2, 3]);
    let r6 = TrialPlan {
        workload: WorkloadChoice::Frozen(pick_workload(Regime::R6, 1).id()),
        context: TrialContext::Cycle {
            cycle: NonZeroU32::MIN,
            step: crate::StepNo::new(NonZeroU32::new(4).unwrap()),
            part: crate::PartNo::new(NonZeroU32::MIN),
        },
        profile: checking(&j),
        ..workload_plan(&r1, Regime::R6, Load::Many(all), 600)
    };
    let second = commit(&mut j, &r6, 1);
    j.assert_resumes();

    let done = pass_reruns(&mut j);
    assert_eq!(done.len(), 2 * n + 1, "{done:?}");
    for (at, plan) in done.iter().enumerate() {
        let (regime, load, cause) = if at < n {
            (Regime::R1, Load::Single(core(0)), first)
        } else {
            (Regime::R6, Load::Many(all), second)
        };
        assert_eq!(
            plan.workload,
            WorkloadChoice::Frozen(pick_workload(regime, 1).id()),
            "trial {at}"
        );
        let duration = if at == 2 * n { 600 } else { 120 };
        assert_eq!(plan.regime, regime, "trial {at}");
        assert_eq!(plan.load, load, "trial {at}");
        assert_eq!(plan.duration, secs(duration), "trial {at}");
        assert_eq!(plan.cause, vec![cause], "trial {at}");
    }
    assert!(j.run().reruns.is_empty());
    j.assert_resumes();
}

#[test]
fn rerun_derived_commitments() {
    for kind in ["attributed", "idle"] {
        let mut j = session(&[-10, -11, -12, -13], r#"{"checking":{"cycle":["R6"]}}"#);
        let n = usize::from(needed(&j));
        let idle = until_trial(&mut j);
        assert_eq!(idle.duration, secs(900));
        let source = if kind == "attributed" {
            commit(&mut j, &idle, 0)
        } else {
            let source = j.idle_failure(checking(&j).offsets(), Condition::Together);
            j.append(
                moved(0, -10, -9, Activity::Checking, Decision::Backoff).into(),
                &[source],
            );
            let change = j.next_decision();
            assert!(
                matches!(change.payload, DecisionPayload::ProfileChange(_)),
                "{kind}: {change:?}"
            );
            j.decide(change);
            source
        };
        j.assert_resumes();
        let done = pass_reruns(&mut j);
        assert_eq!(done.len(), n + 1, "{kind}: {done:?}");
        for (at, plan) in done.iter().enumerate() {
            let duration = if at == n { 900 } else { 120 };
            assert_eq!(plan.condition, Condition::Together, "{kind} {at}");
            assert_eq!(plan.regime, Regime::R6, "{kind} {at}");
            assert_eq!(plan.workload, idle.workload, "{kind} {at}");
            assert_eq!(plan.load, idle.load, "{kind} {at}");
            assert_eq!(plan.duration, secs(duration), "{kind} {at}");
            assert_eq!(plan.cause, vec![source], "{kind} {at}");
        }
        assert!(j.run().reruns.is_empty(), "{kind}");
        j.assert_resumes();
    }
}

#[test]
fn checking_carries_deeper_pass_across_backoff() {
    let mut j = session(&[-20], r#"{"checking":{"cycle":["R1"]}}"#);
    let first = until_trial(&mut j);
    assert_eq!(first.regime, Regime::R1);
    pass(&mut j, &first);
    j.decision(moved(0, -20, -19, Activity::Checking, Decision::Backoff));
    let change = j.next_decision();
    assert!(
        matches!(change.payload, DecisionPayload::ProfileChange(_)),
        "{change:?}"
    );
    j.decide(change);
    let action = j.run().cycle_action();
    assert!(
        matches!(&action, Some(Action::Decide(decision)) if matches!(&decision.payload, DecisionPayload::CheckingCycle(cycle) if cycle.event == CycleEvent::End)),
        "the deeper pass was lost after the backoff: {action:?}"
    );
    j.assert_resumes();
}

#[test]
fn rerun_retry_belongs_to_its_obligation() {
    let mut j = session(&[-10, -11, -12, -13], r#"{"checking":{"cycle":["R1"]}}"#);
    let n = needed(&j);
    let template = until_trial(&mut j);
    let first = commit(&mut j, &template, 0);
    let other = TrialPlan {
        load: Load::Single(core(1)),
        profile: checking(&j),
        ..template
    };
    let second = commit(&mut j, &other, 1);
    for _ in 1..n {
        let plan = rerun(&j);
        assert_eq!(plan.load, Load::Single(core(0)));
        pass(&mut j, &plan);
    }
    let last = rerun(&j);
    assert_eq!(last.load, Load::Single(core(0)));
    ended(&mut j, &last, Outcome::Inconclusive, None);
    let again = rerun(&j);
    assert_eq!(
        again,
        TrialPlan {
            retry: true,
            ..last
        }
    );
    assert_eq!(again.cause, vec![first]);
    j.assert_resumes();

    j.load(&testkit::config(
        r#"{"checking":{"cycle":["R1"]},"evidence":{"miss":0.1}}"#,
    ));
    assert!(needed(&j) < n, "n = {} did not drop", needed(&j));
    assert!(j.run().retry.is_none());
    assert_eq!(j.run().reruns.head().map(|head| head.failure), Some(second));
    let next = rerun(&j);
    assert!(!next.retry);
    assert_eq!(next.load, Load::Single(core(1)));
    assert_eq!(next.cause, vec![second]);
    assert_eq!(next.context, TrialContext::Rerun { failure: second });
    assert_eq!(
        next.requirement.window,
        Window::LiveOrCarried {
            since: Some(second)
        }
    );
    j.assert_resumes();
}

#[test]
fn cycle_retry_ends_with_its_cycle() {
    let mut j = session(&[-10, -20], r#"{"checking":{"cycle":["R1"]}}"#);
    let plan = until_trial(&mut j);
    ended(&mut j, &plan, Outcome::Inconclusive, None);
    let again = j.next_trial();
    assert!(again.retry);
    assert!(matches!(again.context, TrialContext::Cycle { cycle, .. } if cycle.get() == 1));
    j.cycle_end(1, false, false);
    j.cycle_start(2, &[Regime::R1]);
    let next = j.next_trial();
    assert!(!next.retry, "cycle 2 ran cycle 1's retry: {next:?}");
    assert!(matches!(next.context, TrialContext::Cycle { cycle, .. } if cycle.get() == 2));
    j.assert_resumes();
}

#[test]
fn cycle_retry_is_served_and_dropped_with_its_requirement() {
    let cycle = r#"{"checking":{"cycle":["R1"]}}"#;
    let mut j = session(&[-10, -11], cycle);
    let plan = until_trial(&mut j);
    ended(&mut j, &plan, Outcome::Inconclusive, None);
    assert_eq!(
        j.next_trial(),
        TrialPlan {
            retry: true,
            ..plan.clone()
        }
    );
    j.cycle_end(1, false, false);
    assert!(j.run().retry.is_none());
    j.assert_resumes();

    let mut j = session(&[-10, -11], cycle);
    let plan = until_trial(&mut j);
    ended(&mut j, &plan, Outcome::Inconclusive, None);
    j.load(&testkit::config(cycle));
    assert!(
        j.next_trial().retry,
        "an unchanged config dropped the retry"
    );
    j.load(&testkit::config(
        r#"{"checking":{"cycle":["R1"]},"durations":{"checking_trial_s":100}}"#,
    ));
    assert!(j.run().retry.is_none());
    let next = j.next_trial();
    assert!(!next.retry);
    assert_eq!(next.duration, secs(100));
    j.assert_resumes();

    let mut j = session(
        &[-10, -11, -12, -13],
        r#"{"checking":{"cycle":["R7"]},"durations":{"checking_all_core_s":480}}"#,
    );
    let all = set(&[0, 1, 2, 3]);
    let mut chain: Option<(CheckingChain, Vec<Seq>)> = None;
    let partial = 'found: {
        for _ in 0..40 {
            match j.next() {
                Action::RunTrial(plan) if plan.load.cores() != all => break 'found plan,
                Action::RunTrial(plan) => {
                    pass(&mut j, &plan);
                }
                Action::Decide(decision) => {
                    if let DecisionPayload::CheckingChain(derived) = &decision.payload {
                        chain = Some((derived.clone(), decision.cause.clone()));
                    }
                    j.decide(decision);
                }
                Action::ReadRanking => panic!("the tuner asked to read a ranking"),
            }
        }
        panic!("no partial part came");
    };
    let (chain, cause) = chain.unwrap();
    assert_eq!(partial.load.cores(), chain.cores);
    ended(&mut j, &partial, Outcome::Inconclusive, None);
    assert!(j.next_trial().retry);
    j.append(
        CheckingChain {
            rederived: Some(Rederived::Unchanged),
            ..chain.clone()
        }
        .into(),
        &cause,
    );
    assert!(
        j.next_trial().retry,
        "an unchanged re-derivation dropped the retry"
    );
    let changed = if chain.cores == set(&[2, 3]) {
        set(&[1, 2])
    } else {
        set(&[2, 3])
    };
    j.append(
        CheckingChain {
            cores: changed,
            rederived: Some(Rederived::Changed),
            ..chain
        }
        .into(),
        &cause,
    );
    assert!(j.run().retry.is_none());
    let next = j.next_trial();
    assert!(!next.retry);
    assert_eq!(next.load.cores(), changed);
    j.assert_resumes();
}

#[test]
fn skipped_deepening_and_rerun_failures() {
    let mut j = Journal::new(
        &[0, 1],
        &[Start::has_room(-19), Start::at_limit(-20).fail(-21)],
    );
    let change = j.next_decision();
    j.decide(change);
    let class = ClassKey::new(pick_workload(Regime::R1, 0).id(), set(&[0]), secs(120));
    let at = j.profile(&[-20, -20]);
    let failure = carry(&mut j, class, &at, Outcome::Failure, Some(0));
    j.append(
        moved(0, -20, -19, Activity::Checking, Decision::Backoff).into(),
        &[failure],
    );
    assert_eq!(
        j.run().reruns.head().map(|head| head.failure),
        Some(failure)
    );
    j.decision(moved(0, -19, -20, Activity::Deepening, Decision::Deepen));
    let from = checking(&j).offsets();
    let to = j.profile(&[-20, -20]).offsets();
    j.append(
        ProfileChange {
            from: Some(from),
            to,
        }
        .into(),
        &[],
    );

    let skip = j.next_decision();
    let DecisionPayload::Failure(known) = &skip.payload else {
        panic!("the known failure scheduled a live rerun: {skip:?}");
    };
    assert_eq!(known.known_failure, Some(failure));
    j.decide(skip);
    let action = j.next_decision();
    let DecisionPayload::TunerDecision(backoff) = &action.payload else {
        panic!("the skipped rerun lost its backoff: {action:?}");
    };
    assert_eq!(backoff.decision, Decision::Backoff);
    assert_eq!(backoff.to_offset, off(-19));
    assert_eq!(backoff.activity, Activity::Checking);
    assert!(action.cause.contains(&failure), "{action:?}");
    j.decide(action);
    j.assert_resumes();
}

#[test]
fn carried_rerun_citation_replay() {
    for reload in [false, true] {
        let cycle = r#"{"checking":{"cycle":["R1"]}}"#;
        let mut j = session(&[-10], cycle).resuming_every(1);
        let n = needed(&j);
        let profile = checking(&j);
        let long = ClassKey::new(pick_workload(Regime::R1, 0).id(), set(&[0]), secs(900));
        let failure = carry(&mut j, long, &profile, Outcome::Failure, None);
        let count = if reload { n - 1 } else { n };
        let mut facts: Vec<Seq> = (0..count)
            .map(|_| {
                carry(
                    &mut j,
                    long.with_duration(secs(120)),
                    &profile,
                    Outcome::Pass,
                    None,
                )
            })
            .collect();
        facts.push(carry(&mut j, long, &profile, Outcome::Pass, None));
        j.append(
            moved(0, -10, -9, Activity::Checking, Decision::Backoff).into(),
            &[failure],
        );
        assert_eq!(j.run().reruns.is_empty(), !reload, "reload {reload}");
        let change = j.next_decision();
        assert!(
            matches!(change.payload, DecisionPayload::ProfileChange(_)),
            "{change:?}"
        );
        j.decide(change);
        if reload {
            j.load(&testkit::config(
                r#"{"checking":{"cycle":["R1"]},"evidence":{"miss":0.1}}"#,
            ));
            assert!(needed(&j) < n, "n did not drop");
        }
        assert!(j.run().reruns.is_empty(), "reload {reload}");
        let mut cited = j.run().reruns.citations().to_vec();
        cited.sort_unstable();
        assert_eq!(cited, facts, "reload {reload}");

        let start = j.next_decision();
        let DecisionPayload::CheckingCycle(opened) = &start.payload else {
            panic!("the reruns did not lead to a cycle start: {start:?}");
        };
        assert_eq!(opened.event, CycleEvent::Start);
        let reason = opened.reason.clone().unwrap_or_default();
        assert!(reason.contains("rerun checks passed"), "{reason}");
        assert!(reason.contains(SESSION), "{reason}");
        for seq in &facts {
            assert!(start.cause.contains(seq), "#{seq} not cited: {start:?}");
        }
        j.decide(start);
        assert!(j.run().reruns.citations().is_empty());

        let live = j.next_trial();
        assert!(
            matches!(live.context, TrialContext::Cycle { .. }),
            "{live:?}"
        );
        assert_eq!(live.regime, Regime::R1);
        pass(&mut j, &live);
        let want = j.state.next().ok();
        assert_eq!(fresh(&j.events).next().ok(), want);
        let Some(Action::Decide(next)) = want else {
            panic!("expected the next decision, got {want:?}");
        };
        if let DecisionPayload::CheckingCycle(cycle) = &next.payload {
            let reason = cycle.reason.clone().unwrap_or_default();
            assert!(!reason.contains("rerun checks passed"), "{reason}");
        }
        for seq in &facts {
            assert!(!next.cause.contains(seq), "#{seq} cited again: {next:?}");
        }
    }
}

#[test]
fn rerun_window_includes_passes_between_the_failure_and_the_backoff() {
    for (checking_trial, live) in [(120, true), (120, false), (300, false)] {
        let json = format!(
            r#"{{"checking":{{"cycle":["R1"]}},"durations":{{"checking_trial_s":{checking_trial}}}}}"#
        );
        for fewer in [1, 0] {
            let label = format!("checking {checking_trial} s, live {live}, {fewer} fewer");
            let mut j = session(&[-10, -11], &json);
            let n = needed(&j);
            let plan = until_trial(&mut j);
            let failure = fail(&mut j, &plan, Some(0));
            let failing = checking(&j);
            let short = plan.requirement.class.with_duration(secs(120));
            let equal = failing.with(core(0), off(-9));
            for at in 0..n - fewer {
                if live {
                    pass(&mut j, &plan);
                } else {
                    let profile = if at == 0 { &equal } else { &failing };
                    carry(&mut j, short, profile, Outcome::Pass, None);
                }
            }
            back_off(&mut j, failure);
            let done = pass_reruns(&mut j);
            let durations: Vec<Secs> = done.iter().map(|plan| plan.duration).collect();
            let mut want = vec![secs(120); usize::from(fewer)];
            if checking_trial != 120 {
                want.push(secs(checking_trial));
            }
            assert_eq!(durations, want, "{label}");
            assert!(j.run().reruns.is_empty(), "{label}");
            j.assert_resumes();
        }
    }
}

#[test]
fn rerun_window_starts_at_the_failure_it_answers() {
    let mut j = session(&[-10, -11], r#"{"checking":{"cycle":["R1"]}}"#);
    let n = usize::from(needed(&j));
    let plan = until_trial(&mut j);
    for _ in 0..n {
        pass(&mut j, &plan);
    }
    commit(&mut j, &plan, 0);
    j.assert_resumes();
    let done = pass_reruns(&mut j);
    assert_eq!(done.len(), n, "{done:?}");
    assert!(j.run().reruns.is_empty());
    j.assert_resumes();
}

#[test]
fn a_failure_of_the_class_after_the_passes_invalidates_them_for_the_rerun() {
    for failed in [false, true] {
        let mut j = session(&[-10, -11], r#"{"checking":{"cycle":["R1"]}}"#);
        let n = usize::from(needed(&j));
        let plan = until_trial(&mut j);
        let class = plan.requirement.class;
        let first = commit(&mut j, &plan, 0);
        let at = checking(&j);
        for _ in 1..n {
            carry(&mut j, class, &at, Outcome::Pass, None);
        }
        if failed {
            carry(&mut j, class, &at, Outcome::Failure, Some(0));
        }
        let Some(Action::RunTrial(rerun)) = j.run().rerun_action() else {
            panic!("no rerun owed");
        };
        assert_eq!(rerun.context, TrialContext::Rerun { failure: first });
        pass(&mut j, &rerun);
        assert_eq!(
            j.run().reruns.is_empty(),
            !failed,
            "the failure after the passes did not decide whether one more pass answers"
        );
        j.assert_resumes();
    }
}

#[test]
fn rerun_retired_in_fold_and_next_is_pure() {
    let mut j = session(&[-10, -11], r#"{"checking":{"cycle":["R1"]}}"#);
    let n = needed(&j);
    let plan = until_trial(&mut j);
    let failure = commit(&mut j, &plan, 0);
    for _ in 1..n {
        let plan = rerun(&j);
        pass(&mut j, &plan);
    }
    let last = rerun(&j);
    assert_eq!(j.next(), j.next());
    let before = j.state.clone();
    let _ = j.next();
    let _ = j.run().rerun_action();
    assert_eq!(j.state, before);

    let (intent, trial) = j.intent(&last);
    assert_eq!(
        j.run().reruns.head().map(|head| head.failure),
        Some(failure)
    );
    j.end(intent, &trial, Outcome::Pass);
    assert_eq!(j.run().reruns.head(), None);
    assert!(j.run().rerun_action().is_none());
    j.assert_resumes();
}

#[test]
fn rerun_plan_shape() {
    let mut j = session(
        &[-10, -11],
        r#"{"checking":{"cycle":["R1"]},"durations":{"checking_trial_s":300}}"#,
    );
    let n = needed(&j);
    let plan = until_trial(&mut j);
    let class = plan.requirement.class;
    let failure = commit(&mut j, &plan, 0);
    let window = Window::LiveOrCarried {
        since: Some(failure),
    };
    for at in 0..=n {
        let rerun = rerun(&j);
        let (duration, needed) = if at < n { (120, n) } else { (300, 1) };
        assert_eq!(rerun.condition, Condition::Together);
        assert_eq!(rerun.activity, Activity::Checking);
        assert_eq!(rerun.context, TrialContext::Rerun { failure });
        assert_eq!(rerun.regime, Regime::R1);
        assert_eq!(rerun.workload, plan.workload);
        assert_eq!(rerun.load, Load::Single(core(0)));
        assert_eq!(rerun.profile, checking(&j));
        assert_eq!(rerun.duration, secs(duration));
        assert!(!rerun.retry);
        assert_eq!(rerun.cause, vec![failure]);
        assert_eq!(rerun.requirement.kind, RequirementKind::Rerun);
        assert_eq!(rerun.requirement.class, class.with_duration(secs(duration)));
        assert_eq!(rerun.requirement.needed.get(), needed);
        assert_eq!(rerun.requirement.window, window);
        assert_eq!(rerun.requirement.step, None);
        assert_eq!(rerun.requirement.part, None);
        pass(&mut j, &rerun);
    }
    assert!(!is_rerun(&j.next()));
    j.assert_resumes();
}

#[test]
fn live_rerun_pass_after_the_cycle_start_also_counts_for_the_cycle_requirement_once() {
    let mut j = session(
        &[-10, -11],
        r#"{"checking":{"cycle":["R7"]},"evidence":{"miss":0.3},"durations":{"checking_all_core_s":480}}"#,
    );
    assert_eq!(needed(&j), 2);
    let first = until_trial(&mut j);
    assert_eq!(first.duration, secs(120));
    commit(&mut j, &first, 0);
    let done = pass_reruns(&mut j);
    assert_eq!(done.len(), 2, "{done:?}");
    j.assert_resumes();

    let mut short = 0;
    loop {
        let plan = until_trial(&mut j);
        assert!(
            matches!(plan.context, TrialContext::Cycle { .. }),
            "{plan:?}"
        );
        if plan.duration != secs(120) {
            assert_eq!(plan.duration, secs(480));
            break;
        }
        short += 1;
        pass(&mut j, &plan);
        assert!(short < 4, "the reruns did not count for the cycle");
    }
    assert_eq!(short, 1);
    j.assert_resumes();
}

#[test]
fn reruns_run_before_the_open_cycle() {
    let mut j = session(&[-10, -11], r#"{"checking":{"cycle":["R1"]}}"#);
    let plan = until_trial(&mut j);
    commit(&mut j, &plan, 0);
    assert!(j.run().checking.open().is_some());
    assert!(
        j.run().cycle_action().as_ref().is_some_and(is_cycle_trial),
        "the cycle has no unmet requirement"
    );
    assert!(is_rerun(&j.next()));
    pass_reruns(&mut j);
    let next = j.next();
    assert!(is_cycle_trial(&next), "{next:?}");
    let Action::RunTrial(next) = next else {
        unreachable!()
    };
    assert_eq!(next.load, Load::Single(core(1)));
    j.assert_resumes();
}

#[test]
fn intent_context_rejects_a_rerun_of_the_wrong_class() {
    let mut j = session(&[-10, -11], r#"{"checking":{"cycle":["R1"]}}"#);
    let plan = until_trial(&mut j);
    commit(&mut j, &plan, 0);
    let rerun = rerun(&j);
    unscheduled(
        &j,
        &TrialPlan {
            load: Load::Single(core(1)),
            ..rerun.clone()
        },
    );
    unscheduled(
        &j,
        &TrialPlan {
            duration: secs(300),
            ..rerun.clone()
        },
    );
    unscheduled(
        &j,
        &workload_plan(&rerun, Regime::R2, Load::Single(core(0)), 120),
    );
    j.assert_resumes();
    pass(&mut j, &rerun);
}

#[test]
fn intent_context_rejects_a_cycle_trial_outside_the_schedule() {
    let mut j = session(&[-10, -11], r#"{"checking":{"cycle":["R1"]}}"#);
    let plan = until_trial(&mut j);
    unscheduled(
        &j,
        &TrialPlan {
            context: TrialContext::Cycle {
                cycle: NonZeroU32::new(2).unwrap(),
                step: crate::StepNo::new(NonZeroU32::MIN),
                part: crate::PartNo::new(NonZeroU32::MIN),
            },
            ..plan.clone()
        },
    );
    unscheduled(
        &j,
        &workload_plan(&plan, Regime::R2, Load::Single(core(0)), 120),
    );
    unscheduled(
        &j,
        &TrialPlan {
            duration: secs(300),
            ..plan.clone()
        },
    );
    j.assert_resumes();
    pass(&mut j, &plan);
}

fn deepening_rerun_session(failing: i64) -> Journal {
    let mut j = Journal::new(
        &[0],
        &[Start::at_limit(failing).fail(failing - 1).pass(failing)],
    );
    j.load(&testkit::config(r#"{"checking":{"cycle":["R1"]}}"#));
    j.profile_change(&[failing]);
    j
}

fn back_off_in_round(j: &mut Journal, failure: Seq, from: i64, to: i64) {
    j.append(
        moved(0, from, to, Activity::Checking, Decision::Backoff).into(),
        &[failure],
    );
    let from = checking(j).offsets();
    let to = j.profile(&[to]).offsets();
    j.append(
        ProfileChange {
            from: Some(from),
            to,
        }
        .into(),
        &[],
    );
}

#[test]
fn carried_rerun_citation_replay_ends_a_deepening_round() {
    for reload in [false, true] {
        let mut j = deepening_rerun_session(-10).resuming_every(1);
        let n = needed(&j);
        let profile = checking(&j);
        let long = ClassKey::new(pick_workload(Regime::R1, 0).id(), set(&[0]), secs(900));
        let failure = carry(&mut j, long, &profile, Outcome::Failure, None);
        let count = if reload { n - 1 } else { n };
        let mut facts: Vec<Seq> = (0..count)
            .map(|_| {
                carry(
                    &mut j,
                    long.with_duration(secs(120)),
                    &profile,
                    Outcome::Pass,
                    None,
                )
            })
            .collect();
        facts.push(carry(&mut j, long, &profile, Outcome::Pass, None));
        j.round_start(1, &[-9], &[0], u32::from(n));
        back_off_in_round(&mut j, failure, -10, -9);
        if reload {
            j.load(&testkit::config(
                r#"{"checking":{"cycle":["R1"]},"evidence":{"miss":0.1}}"#,
            ));
            assert!(needed(&j) < n, "n did not drop");
        }
        let action = j.next_decision();
        let DecisionPayload::DeepeningRound(end) = &action.payload else {
            panic!("the rerun did not finish the round: {action:?}");
        };
        assert!(end.passed, "reload {reload}");
        for seq in &facts {
            assert!(action.cause.contains(seq), "#{seq} not cited: {action:?}");
        }
        assert!(
            end.reason
                .as_deref()
                .unwrap_or_default()
                .contains("rerun checks passed"),
            "{end:?}"
        );
        j.decide(action);
        let want = j.state.next().ok();
        assert_eq!(fresh(&j.events).next().ok(), want);
        if let Some(Action::Decide(next)) = &want {
            for seq in &facts {
                assert!(!next.cause.contains(seq), "#{seq} cited again: {next:?}");
            }
        }
    }
}

#[test]
fn a_carried_rerun_boundary_preserves_the_pending_checks() {
    for deepening in [false, true] {
        for missing_long in [false, true] {
            let label = format!("deepening {deepening}, missing long {missing_long}");
            let mut j = deepening_rerun_session(-10);
            let n = needed(&j);
            let profile = checking(&j);
            let long = ClassKey::new(pick_workload(Regime::R1, 0).id(), set(&[0]), secs(900));
            let failure = carry(&mut j, long, &profile, Outcome::Failure, None);
            let (count, duration) = if missing_long { (n, 900) } else { (n - 1, 120) };
            for _ in 0..count {
                carry(
                    &mut j,
                    long.with_duration(secs(120)),
                    &profile,
                    Outcome::Pass,
                    None,
                );
            }
            back_off_in_round(&mut j, failure, -10, -9);
            if deepening {
                j.round_start(1, &[-9], &[0], u32::from(n));
                let round = std::num::NonZeroU32::MIN;
                let end = togi_journal::DeepeningRound {
                    round,
                    event: togi_journal::RoundEvent::End,
                    base: PerCore::default(),
                    base_seq: None,
                    target: PerCore::default(),
                    profile: PerCore::default(),
                    cores: CoreSet::default(),
                    ranking: Vec::new(),
                    trials: None,
                    trial_s: None,
                    passed: false,
                    reason: Some("test".to_owned()),
                };
                j.append(end.into(), &[]);
            } else {
                j.cycle_start(1, &[Regime::R1]);
            }
            let want = j.state.next().ok();
            assert_eq!(fresh(&j.events).next().ok(), want, "{label}");
            let Some(Action::RunTrial(plan)) = want else {
                panic!("{label}: the boundary lost the pending rerun: {want:?}");
            };
            assert!(
                matches!(plan.context, TrialContext::Rerun { .. }),
                "{label}"
            );
            assert_eq!(plan.duration, secs(duration), "{label}");
            assert_eq!(plan.cause, [failure], "{label}");
        }
    }
}

#[test]
fn carried_rerun_citations_wait_for_a_cycle_boundary_not_a_step() {
    let mut j = session(&[-10], r#"{"checking":{"cycle":["R7"]}}"#);
    j.cycle_start(1, &[Regime::R7]);
    let profile = checking(&j);
    let class = ClassKey::new(
        pick_workload(Regime::R1, 0).id(),
        set(&[0]),
        j.run().config.short_trial,
    );
    let failure = carry(&mut j, class, &profile, Outcome::Failure, None);
    let facts: Vec<_> = (0..needed(&j))
        .map(|_| carry(&mut j, class, &profile, Outcome::Pass, None))
        .collect();
    j.append(
        moved(0, -10, -9, Activity::Checking, Decision::Backoff).into(),
        &[failure],
    );
    j.profile_change(&[-9]);
    assert!(j.run().reruns.is_empty());
    assert_eq!(j.run().reruns.citations(), facts);
    let step = j.run().cycle_action().unwrap();
    assert!(matches!(
        &step,
        Action::Decide(decision)
            if matches!(decision.payload, DecisionPayload::CheckingStep(_))
    ));
    assert_eq!(j.run().after_reruns(step.clone()), step);
    assert_eq!(j.run().reruns.citations(), facts);
    j.assert_resumes();
}

fn ordinals(requirement: &crate::Requirement) -> (Option<u32>, Option<u32>) {
    (
        requirement.step.map(|step| step.get().get()),
        requirement.part.map(|part| part.get().get()),
    )
}

/// A stored requirement as the dashboard shows it: the count, and the (step, part) ordinals the
/// cycle plan marks running.
type Shown = (u32, Vec<(Option<u32>, Option<u32>)>);

/// What the open trial's stored requirement shows; the cycle plan reads the running part from
/// that requirement.
fn shown(j: &Journal) -> Shown {
    let running = j
        .state
        .cycle_plan()
        .steps
        .iter()
        .zip(1..)
        .flat_map(|(step, number)| {
            step.parts
                .iter()
                .zip(1..)
                .filter(|(part, _)| part.running)
                .map(move |(_, part)| (Some(number), Some(part)))
        })
        .collect();
    (j.requirement().needed, running)
}

/// What `shown` reads once the fold stores `plan`'s requirement.
fn scheduled(plan: &TrialPlan) -> Shown {
    let running = if plan.requirement.kind == RequirementKind::Cycle {
        vec![ordinals(&plan.requirement)]
    } else {
        Vec::new()
    };
    (u32::from(plan.requirement.needed.get()), running)
}

#[test]
fn resumed_short_trial_change_drops_a_rerun_retry() {
    let cycle = r#"{"checking":{"cycle":["R1"]}}"#;
    let cases = [
        ("unchanged", cycle, true, 120),
        (
            "short_trial_s",
            r#"{"checking":{"cycle":["R1"]},"durations":{"short_trial_s":100}}"#,
            false,
            100,
        ),
    ];
    for (name, json, retry, duration) in cases {
        let mut j = session(&[-10], cycle);
        let template = until_trial(&mut j);
        let failure = commit(&mut j, &template, 0);
        let first = rerun(&j);
        assert_eq!(first.duration, secs(120), "{name}");
        ended(&mut j, &first, Outcome::Inconclusive, None);
        j.load(&testkit::config(json));
        assert_eq!(j.run().retry.is_some(), retry, "{name}");
        let next = rerun(&j);
        assert_eq!(
            (next.retry, next.duration, next.load, next.cause.clone()),
            (retry, secs(duration), Load::Single(core(0)), vec![failure]),
            "{name}"
        );
        j.intent(&next);
        assert_eq!(shown(&j), scheduled(&next), "{name}");
        j.assert_resumes();
    }
}

#[test]
fn cycle_retries_attach_the_requirement_the_fold_stores() {
    let r1 = r#"{"checking":{"cycle":["R1"]}}"#;
    let r7 = r#"{"checking":{"cycle":["R7"]}}"#;
    let cases = [
        ("same shape cycle", r1, r1, true),
        (
            "stale cycle duration",
            r1,
            r#"{"checking":{"cycle":["R1"]},"durations":{"checking_trial_s":100}}"#,
            false,
        ),
        ("same shape R7 part", r7, r7, true),
        (
            "stale R7 part duration",
            r7,
            r#"{"checking":{"cycle":["R7"]},"durations":{"short_trial_s":100}}"#,
            false,
        ),
    ];
    for (name, before, after, retry) in cases {
        let mut j = session(&[-10, -11, -12, -13], before);
        let first = until_trial(&mut j);
        assert_eq!(first.requirement.kind, RequirementKind::Cycle, "{name}");
        assert_eq!(ordinals(&first.requirement), (Some(1), Some(1)), "{name}");
        ended(&mut j, &first, Outcome::Inconclusive, None);
        j.load(&testkit::config(after));
        assert_eq!(j.run().retry.is_some(), retry, "{name}");
        let next = until_trial(&mut j);
        assert_eq!(next.retry, retry, "{name}: {next:?}");
        let duration = if retry { first.duration } else { secs(100) };
        assert_eq!(next.duration, duration, "{name}");
        j.intent(&next);
        assert_eq!(shown(&j), scheduled(&next), "{name}");
        j.assert_resumes();
    }
}

#[test]
fn stored_requirement_separates_class_count_and_part() {
    let mut j = session(&[-10, -11, -12, -13], r#"{"checking":{"cycle":["R7"]}}"#);
    let all = set(&[0, 1, 2, 3]);
    let partial = 'found: {
        for _ in 0..40 {
            match j.next() {
                Action::RunTrial(plan) if plan.load.cores() != all => break 'found plan,
                Action::RunTrial(plan) => {
                    pass(&mut j, &plan);
                }
                Action::Decide(decision) => {
                    j.decide(decision);
                }
                Action::ReadRanking => panic!("the tuner asked to read a ranking"),
            }
        }
        panic!("no partial part came");
    };
    let (intent, trial) = j.intent(&partial);
    assert_eq!(ordinals(&partial.requirement), (Some(1), Some(2)));
    assert_eq!(shown(&j), (3, vec![(Some(1), Some(2))]));
    let mut end = Journal::end_payload(&trial, Outcome::Inconclusive, None);
    end.duration_s = partial.duration.get();
    j.append(end.into(), &[intent]);
    let history = j.history();
    assert_eq!(history.needed, 3);
    assert_eq!(history.step.map(|step| step.get().get()), Some(1));
    let Action::RunTrial(again) = j.next() else {
        panic!("expected the partial part's retry");
    };
    assert!(again.retry, "{again:?}");
    assert_eq!(again.requirement, partial.requirement);
    j.assert_resumes();
}

#[test]
fn a_rerun_owed_before_any_checking_profile_needs_the_full_short_count() {
    let mut j = Journal::new(&[0], &[Start::at_limit(-20).fail(-21)]);
    assert!(j.run().checking.profile().is_none());
    let class = ClassKey::new(pick_workload(Regime::R1, 0).id(), set(&[0]), secs(900));
    let at = j.profile(&[-20]);
    let failure = carry(&mut j, class, &at, Outcome::Failure, Some(0));
    j.append(
        moved(0, -20, -19, Activity::Checking, Decision::Backoff).into(),
        &[failure],
    );
    let head = j.run().reruns.head().unwrap();
    assert_eq!((head.class, head.failure), (class, failure));
    let need = j.run().rerun_need(head).unwrap();
    assert_eq!(need.class, class.with_duration(secs(120)));
    assert_eq!(need.needed, j.run().config.needed);
    j.assert_resumes();
}
