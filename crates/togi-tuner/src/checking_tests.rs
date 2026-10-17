use std::collections::BTreeMap;

use proptest::prelude::*;
use togi_journal::{
    Attribution, CheckingCycle, CycleEvent, Decision, Event, Mce, Outcome, Payload, Seq,
    TrialCarried, TrialClass, TrialId, TunerDecision,
};
use togi_machine::{BankType, Condition, CoreId, CoreSet, CpuId, PerCore, Regime, Signal};

use crate::testkit::{
    Journal, MPRIME_NEW, MPRIME_OLD, Start, YCRUNCHER_NEW, YCRUNCHER_OLD, config, infos, pairs, set,
};
use crate::tests::core;
use crate::{
    Action, ClassKey, DecisionAction, DecisionPayload, Load, Profile, Secs, TrialContext, TrialPlan,
};

fn decision_cycle(action: &Action) -> Option<&CheckingCycle> {
    if let Action::Decide(DecisionAction {
        payload: DecisionPayload::CheckingCycle(cycle),
        ..
    }) = action
    {
        Some(cycle)
    } else {
        None
    }
}

fn finish_cycle(j: &mut Journal) -> CheckingCycle {
    for _ in 0..1000 {
        let action = j.step(&mut |_| Outcome::Pass);
        if let Some(cycle) = decision_cycle(&action) {
            if cycle.event == CycleEvent::End {
                return cycle.clone();
            }
        }
    }
    panic!("the cycle never ended");
}

fn started(offsets: &[i64]) -> Journal {
    let mut j = Journal::has_room(offsets);
    let change = j.next_decision();
    j.decide(change);
    j
}

fn named_failure(j: &mut Journal, plan: &TrialPlan, named: Option<u8>) -> Seq {
    let (intent, trial) = j.intent(plan);
    let mut end = Journal::end_payload(&trial, Outcome::Failure, Some(Signal::ComputationError));
    end.core = named.map(core);
    j.append(end.into(), &[intent]);
    let decision = j.next_decision();
    assert!(matches!(decision.payload, DecisionPayload::Failure(_)));
    j.decide(decision)
}

fn failure_record(j: &Journal, seq: Seq) -> togi_journal::Failure {
    j.run().failures.pending(seq).unwrap().failure.clone()
}

fn backoff(action: &DecisionAction) -> &TunerDecision {
    let DecisionPayload::TunerDecision(decision) = &action.payload else {
        panic!("expected a decision, got {:?}", action.payload);
    };
    decision
}

fn mce_of(trial: &TrialId, id: u8) -> Mce {
    Mce {
        cpu: CpuId(u16::from(id)),
        core: Some(core(id)),
        bank: 0,
        bank_type: BankType::LoadStore,
        corrected: false,
        monotonic_ns: None,
        from_boot: None,
        trial: Some(trial.clone()),
        between_trials: false,
        lines: Vec::new(),
    }
}

fn r6(j: &Journal, cores: &[u8], profile: &[i64]) -> TrialPlan {
    j.manual(
        Regime::R6,
        0,
        Load::Many(set(cores)),
        Condition::Together,
        profile,
        j.run().config.checking_idle.get(),
    )
}

#[test]
fn cycle_coverage() {
    let mut j = started(&[-10, -12, -13, -11]);
    let end = finish_cycle(&mut j);
    assert!(end.passed && end.full);
    assert!(end.missing.is_empty());

    let mut j = Journal::has_room(&[-10, -12]);
    j.load(&config(r#"{"checking":{"cycle":["R1","R7"]}}"#));
    let change = j.next_decision();
    j.decide(change);
    let end = finish_cycle(&mut j);
    assert!(end.passed && !end.full);
    assert_eq!(
        end.missing,
        [
            "R1: 2 more steps",
            "R2: 3 more steps",
            "R3: 1 more steps",
            "R4: 1 more steps",
            "R5: 1 more steps",
            "R6: 1 more steps",
            "R7: 2 more steps",
        ]
    );
}

#[test]
fn the_first_profile_change_has_no_from_and_the_cycle_captures_its_schedule() {
    let ids = [0, 1, 2, 3];
    let mut j = Journal::on(&ids, pairs(&ids), &[Start::has_room(-10); 4]);
    let change = j.next_decision();
    let DecisionPayload::ProfileChange(first) = &change.payload else {
        panic!("expected the first profile change, got {change:?}");
    };
    assert_eq!(first.from, None);
    j.decide(change);
    let start = j.next_decision();
    let DecisionPayload::CheckingCycle(cycle) = &start.payload else {
        panic!("expected a cycle start, got {start:?}");
    };
    assert_eq!(cycle.event, CycleEvent::Start);
    assert_eq!(cycle.steps, j.run().config.cycle.to_vec());
    j.decide(start);
    j.load(&config(r#"{"checking":{"cycle":["R6"]}}"#));
    let open = j.run().checking.open().unwrap();
    assert_eq!(open.steps_done(), 0);
    let action = j.next_decision();
    let DecisionPayload::CheckingStep(step) = &action.payload else {
        panic!("the captured schedule did not start with an R7 step: {action:?}");
    };
    assert_eq!(step.step.get(), 1);
    j.assert_resumes();
}

#[test]
fn r7_trials_and_shared_duration() {
    let ids = [0, 1, 2, 3];
    let mut j = Journal::on(&ids, pairs(&ids), &[Start::has_room(-10); 4]);
    j.load(&config(
        r#"{"checking":{"cycle":["R7"]},"durations":{"checking_all_core_s":480}}"#,
    ));
    let change = j.next_decision();
    j.decide(change);
    let mut trials = Vec::new();
    loop {
        let action = j.step(&mut |_| Outcome::Pass);
        match &action {
            Action::RunTrial(plan) => trials.push((
                plan.load
                    .cores()
                    .iter()
                    .map(CoreId::get)
                    .collect::<Vec<_>>(),
                plan.duration.get(),
            )),
            Action::Decide(_)
                if decision_cycle(&action).is_some_and(|cycle| cycle.event == CycleEvent::End) =>
            {
                break;
            }
            Action::Decide(_) | Action::ReadRanking => {}
        }
    }
    let part = |cores: &[u8], durations: [u32; 4]| -> Vec<(Vec<u8>, u32)> {
        durations
            .iter()
            .map(|&seconds| (cores.to_vec(), seconds))
            .collect()
    };
    let want: Vec<(Vec<u8>, u32)> = [
        part(&[0, 1], [120; 4]),
        part(&[2, 3], [120; 4]),
        part(&[0, 1, 2, 3], [120, 120, 120, 240]),
    ]
    .concat();
    assert_eq!(trials, want);
    j.assert_resumes();
}

#[test]
fn an_attributed_failure_backs_off_one_count_and_an_already_shallower_core_still_answers() {
    let mut j = started(&[-10, -12]);
    let plan = r6(&j, &[0, 1], &[-10, -12]);
    let first = named_failure(&mut j, &plan, Some(0));
    let record = failure_record(&j, first);
    assert_eq!(record.attribution, Attribution::Attributed);
    assert_eq!(
        record.offset.map(togi_machine::TargetOffset::get),
        Some(-10)
    );
    let action = j.next_decision();
    let step = backoff(&action);
    assert_eq!(
        (step.decision, step.from_offset.get(), step.to_offset.get()),
        (Decision::Backoff, -10, -9)
    );
    assert_eq!(step.activity, togi_journal::Activity::Checking);
    assert_eq!(
        step.failure_point.map(togi_machine::TargetOffset::get),
        Some(-10)
    );
    assert!(step.reason.contains("backed off"));
    assert_eq!(action.cause, [first]);
    j.decide(action);

    let second = named_failure(&mut j, &plan, Some(0));
    let action = j.next_decision();
    let step = backoff(&action);
    assert_eq!((step.from_offset.get(), step.to_offset.get()), (-9, -9));
    assert!(step.reason.ends_with("already shallower"));
    assert_eq!(
        step.failure_point.map(togi_machine::TargetOffset::get),
        Some(-10)
    );
    assert_eq!(action.cause, [second]);
    j.decide(action);
    j.assert_resumes();
}

#[test]
fn a_backoff_drops_the_pass_the_failure_contradicts() {
    let ids = [0, 1];
    let mut j = Journal::on(
        &ids,
        infos(&ids),
        &[Start::solo(-10, -11), Start::has_room(-12)],
    );
    let change = j.next_decision();
    j.decide(change);
    let plan = r6(&j, &[0, 1], &[-10, -12]);
    named_failure(&mut j, &plan, Some(0));
    let action = j.next_decision();
    let step = backoff(&action);
    assert_eq!(step.pass, None);
    assert_eq!(
        step.failure_point.map(togi_machine::TargetOffset::get),
        Some(-10)
    );
}

#[test]
fn together_single_nonzero_attribution() {
    let ids = [0, 1];
    let mut j = Journal::on(
        &ids,
        infos(&ids),
        &[Start::at_limit(0), Start::has_room(-12)],
    );
    let change = j.next_decision();
    j.decide(change);
    let plan = r6(&j, &ids, &[0, -12]);
    j.trial(&plan, Outcome::Failure);
    let decision = j.next_decision();
    let DecisionPayload::Failure(failure) = &decision.payload else {
        panic!("expected a failure, got {decision:?}");
    };
    assert_eq!(failure.attribution, Attribution::Attributed);
    assert_eq!(failure.core, Some(core(1)));
    assert_eq!(
        failure.offset.map(togi_machine::TargetOffset::get),
        Some(-12)
    );
}

#[test]
fn together_crash_names_sole_nonzero_core_by_id() {
    let ids = [5, 12];
    let mut j = Journal::on(
        &ids,
        infos(&ids),
        &[Start::has_room(-12), Start::has_room(0)],
    );
    let change = j.next_decision();
    j.decide(change);
    let plan = r6(&j, &ids, &[-12, 0]);
    j.trial(&plan, Outcome::Failure);
    let decision = j.next_decision();
    let DecisionPayload::Failure(failure) = &decision.payload else {
        panic!("expected a failure, got {decision:?}");
    };
    assert_eq!(failure.core, Some(core(5)));
    assert_eq!(
        failure.offset.map(togi_machine::TargetOffset::get),
        Some(-12)
    );
}

#[test]
fn together_multiple_mce_cores_remain_unattributed() {
    let ids = [0, 1, 2, 3];
    let mut j = Journal::on(&ids, pairs(&ids), &[Start::has_room(-12); 4]);
    let change = j.next_decision();
    j.decide(change);
    for (plan, mces, named) in [
        (j.r7_plan(&[0, 1], &[-12; 4]), vec![0_u8, 1], None),
        (r6(&j, &ids, &[-12; 4]), vec![0, 1], None),
        (r6(&j, &ids, &[-12; 4]), vec![1], Some(core(1))),
    ] {
        let (intent, trial) = j.intent(&plan);
        let seqs: Vec<Seq> = mces
            .iter()
            .map(|&id| j.append(mce_of(&trial, id).into(), &[]))
            .collect();
        let mut cause = vec![intent];
        cause.extend(seqs);
        let end = Journal::end_payload(&trial, Outcome::Failure, Some(Signal::Crash));
        j.append(end.into(), &cause);
        let decision = j.next_decision();
        let DecisionPayload::Failure(failure) = &decision.payload else {
            panic!("expected a failure, got {decision:?}");
        };
        assert_eq!(failure.core, named);
        assert_eq!(
            failure.attribution,
            if named.is_some() {
                Attribution::Attributed
            } else {
                Attribution::Unattributed
            }
        );
        j.decide(decision);
        j.assert_resumes();
    }
}

#[test]
fn a_multi_r7_failure_named_by_one_mce_moves_with_a_reset_of_its_core() {
    let ids = [0, 1, 2, 3];
    let mut j = Journal::r7_session();
    let plan = j.r7_plan(&[0, 1], &[-30; 4]);
    let (intent, trial) = j.intent(&plan);
    let mce = j.append(mce_of(&trial, 3).into(), &[]);
    let end = Journal::end_payload(&trial, Outcome::Failure, Some(Signal::Crash));
    j.append(end.into(), &[intent, mce]);
    let decision = j.next_decision();
    let DecisionPayload::Failure(failure) = &decision.payload else {
        panic!("expected a failure, got {decision:?}");
    };
    assert_eq!(failure.core, Some(core(3)));
    j.decide(decision);
    let class = ClassKey::new(
        plan.workload_at(0),
        set(&[0, 1]),
        Secs::new(std::num::NonZeroU32::new(120).unwrap()),
    );
    let profile = j.profile(&[-30; 4]);
    assert!(
        j.run()
            .evidence
            .newest_failure(class, &profile, None)
            .unwrap()
            .is_some()
    );
    j.reset(ids[3]);
    assert_eq!(
        j.run()
            .evidence
            .newest_failure(class, &profile, None)
            .unwrap(),
        None
    );
}

fn carried_failure(j: &mut Journal, named: Option<u8>, profile: &[i64]) -> Seq {
    let seq = j.next_seq();
    let idle = j.run().config.checking_idle.get();
    let payload = TrialCarried {
        source: togi_journal::FactSource {
            session: "20230101T000000Z".parse().unwrap(),
            seq,
            build: crate::testkit::build(),
            trial: Some(TrialId::from("0304".to_owned())),
            time: jiff::Timestamp::from_second(1_600_000_000).unwrap(),
            boot: togi_machine::BootId::from("old".to_owned()),
            backend: None,
        },
        class: TrialClass {
            regime: Regime::R6,
            workload: togi_machine::WorkloadId::new(Regime::R6, 0),
            cores: set(&[0, 1]),
            duration_s: idle,
        },
        condition: Condition::Together,
        activity: None,
        rerun: false,
        profile: j.profile(profile).offsets(),
        outcome: Outcome::Failure,
        signal: Some(Signal::Crash),
        duration_s: idle,
        core: named.map(core),
        stalled_core: None,
        voltage_requests_v: PerCore::default(),
        top_requesters: CoreSet::default(),
        ccd_mhz: BTreeMap::new(),
    };
    j.append(payload.into(), &[])
}

#[test]
fn a_skipped_attributed_failure_backs_off_and_the_next_trial_runs() {
    let mut j = started(&[-20, -20]);
    let fact = carried_failure(&mut j, Some(0), &[-20, -20]);
    j.cycle_start(1, &[Regime::R6]);
    let skip = j.next_decision();
    let DecisionPayload::Failure(skipped) = &skip.payload else {
        panic!("a known failure scheduled a live check: {skip:?}");
    };
    assert_eq!(skipped.known_failure, Some(fact));
    assert_eq!(skipped.core, Some(core(0)));
    j.decide(skip);
    let action = j.next_decision();
    let step = backoff(&action);
    assert_eq!((step.core, step.to_offset.get()), (core(0), -19));
    assert!(action.cause.contains(&fact));
    j.decide(action);
    for _ in 0..20 {
        match j.next() {
            Action::RunTrial(_) => return,
            Action::Decide(decision) => {
                j.decide(decision);
            }
            Action::ReadRanking => panic!("no ranking was asked for"),
        }
    }
    panic!("the skip loop kept the next live trial from running");
}

#[test]
fn a_skipped_unattributed_failure_is_queued_for_its_hunt_once() {
    let mut j = started(&[-20, -20]);
    let fact = carried_failure(&mut j, None, &[-20, -20]);
    j.cycle_start(1, &[Regime::R6]);
    let skip = j.next_decision();
    let DecisionPayload::Failure(skipped) = &skip.payload else {
        panic!("a known failure scheduled a live check: {skip:?}");
    };
    assert_eq!(skipped.known_failure, Some(fact));
    j.decide(skip);
    assert!(j.run().failures.queue().any(|seq| seq == fact));
    assert_eq!(j.state.next().ok(), Some(Action::ReadRanking));
    j.assert_resumes();
}

#[test]
fn an_unattributed_failure_reaching_a_recorded_constraint_is_skipped_not_hunted() {
    let ids = [0, 1];
    let mut j = Journal::on(
        &ids,
        infos(&ids),
        &[Start::has_room(-10).fail(-12), Start::has_room(-10)],
    );
    j.ranking(&ids);
    let change = j.next_decision();
    j.decide(change);
    let plan = r6(&j, &ids, &[-12, -10]);
    let failure = named_failure(&mut j, &plan, None);
    assert!(j.run().failures.queue().any(|seq| seq == failure));
    let action = j.next_decision();
    let DecisionPayload::HuntSkipped(skipped) = &action.payload else {
        panic!("expected hunt.skipped, got {action:?}");
    };
    assert_eq!(skipped.failure, failure);
    assert!(skipped.reason.contains("failure point"));
    assert_eq!(action.cause, [failure]);
    j.decide(action);
    assert!(j.run().failures.queue().next().is_none());
    assert!(matches!(j.next(), Action::RunTrial(_)));
    j.assert_resumes();
}

#[test]
fn an_unattributed_failure_that_reaches_nothing_waits_for_its_hunt() {
    let ids = [0, 1];
    let mut j = Journal::on(
        &ids,
        infos(&ids),
        &[Start::has_room(-10).fail(-12), Start::has_room(-10)],
    );
    let change = j.next_decision();
    j.decide(change);
    let plan = r6(&j, &ids, &[-10, -10]);
    let failure = named_failure(&mut j, &plan, None);
    assert!(j.run().failures.queue().any(|seq| seq == failure));
    assert_eq!(j.state.next().ok(), Some(Action::ReadRanking));
}

#[test]
fn repeated_cycle_classes_add_trials() {
    for regime in togi_machine::REGIMES {
        let catalog = togi_machine::workloads(regime).len();
        let steps: Vec<String> = (0..=catalog).map(|_| format!("\"{regime}\"")).collect();
        let mut j = Journal::has_room(&[-10]);
        j.load(&config(&format!(
            r#"{{"checking":{{"cycle":[{}]}}}}"#,
            steps.join(",")
        )));
        let change = j.next_decision();
        j.decide(change);
        let mut classes: BTreeMap<u32, BTreeMap<ClassKey, u16>> = BTreeMap::new();
        loop {
            let action = j.step(&mut |_| Outcome::Pass);
            if let Action::RunTrial(plan) = &action {
                if let TrialContext::Cycle { step, .. } = plan.context {
                    classes
                        .entry(step.get().get())
                        .or_default()
                        .insert(plan.requirement.class, plan.requirement.needed.get());
                }
            }
            if decision_cycle(&action).is_some_and(|cycle| cycle.event == CycleEvent::End) {
                break;
            }
        }
        let first = &classes[&1];
        let again = &classes[&u32::try_from(catalog + 1).unwrap()];
        let doubled: BTreeMap<ClassKey, u16> = first
            .iter()
            .map(|(&class, &needed)| (class, needed * 2))
            .collect();
        assert_eq!(again, &doubled, "{regime}");
    }
}

#[test]
fn reset_closes_the_open_cycle_before_the_core_searches() {
    let mut j = started(&[-10, -12]);
    let start = j.next_decision();
    j.decide(start);
    let reset = j.reset(1);
    let action = j.next_decision();
    let DecisionPayload::CheckingCycle(end) = &action.payload else {
        panic!("expected the cycle to end first, got {action:?}");
    };
    assert_eq!((end.event, end.passed), (CycleEvent::End, false),);
    assert!(end.reason.as_deref().unwrap().contains("core 01 was reset"));
    assert_eq!(action.cause, [reset]);
    j.decide(action);
    assert!(j.run().checking.open().is_none());
    let action = j.next_decision();
    assert!(matches!(action.payload, DecisionPayload::CorePhase(_)));
    j.assert_resumes();
}

fn visits(passes: usize) -> (u64, u64) {
    let mut j = started(&[-10]);
    let plan = j.manual(
        Regime::R1,
        0,
        Load::Single(core(0)),
        Condition::Together,
        &[-10],
        120,
    );
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
fn cycle_progress_visits_distinct_classes_not_passes() {
    let small = visits(20);
    let large = visits(2000);
    assert!(small.0 > 0);
    assert_eq!(small, large);
}

fn outcome(byte: u8) -> Outcome {
    match byte {
        0..=219 => Outcome::Pass,
        220..=244 => Outcome::Failure,
        _ => Outcome::Inconclusive,
    }
}

fn class_of(event: &Event) -> Option<(TrialId, ClassKey)> {
    let Payload::TrialIntent(intent) = &event.payload else {
        return None;
    };
    let loaded = intent.core.map_or(intent.cores, |core| set(&[core.get()]));
    let secs = Secs::new(std::num::NonZeroU32::new(intent.duration_s).unwrap());
    Some((
        intent.trial.clone(),
        ClassKey::new(intent.workload, loaded, secs),
    ))
}

fn expected_counts(j: &Journal) -> BTreeMap<ClassKey, u32> {
    let topo = j.run().topo_ref();
    let mut classes: BTreeMap<TrialId, (ClassKey, Profile)> = BTreeMap::new();
    let mut passes: Vec<(Seq, ClassKey, Profile)> = Vec::new();
    let mut failures: Vec<(Seq, ClassKey, Profile)> = Vec::new();
    let mut checking: Option<Profile> = None;
    let mut start = None;
    for event in &j.events {
        let payload = &event.payload;
        if let Payload::ProfileChange(change) = payload {
            checking = Some(Profile::new(topo, change.to).unwrap());
        } else if let Payload::CheckingCycle(cycle) = payload {
            start = (cycle.event == CycleEvent::Start).then_some(event.seq);
        } else if let Payload::TrialIntent(intent) = payload {
            let (trial, class) = class_of(event).unwrap();
            classes.insert(trial, (class, Profile::new(topo, intent.profile).unwrap()));
        } else if let Payload::TrialEnd(end) = payload
            && let Some((class, profile)) = classes.get(&end.trial).copied()
        {
            match end.outcome {
                Outcome::Pass => passes.push((event.seq, class, profile)),
                Outcome::Failure => failures.push((event.seq, class, profile)),
                Outcome::Inconclusive => {}
            }
        }
    }
    let (Some(checking), Some(start)) = (checking, start) else {
        return BTreeMap::new();
    };
    let Some(tracked) = j.run().checking.counts() else {
        return BTreeMap::new();
    };
    tracked
        .keys()
        .map(|&class| {
            let floor = failures
                .iter()
                .filter(|(_, other, profile)| *other == class && profile.within(&checking))
                .map(|&(seq, _, _)| seq)
                .max()
                .max(Some(start));
            let valid = passes
                .iter()
                .filter(|(seq, other, profile)| {
                    *other == class && Some(*seq) > floor && profile.covers(&checking)
                })
                .count();
            (class, u32::try_from(valid).unwrap())
        })
        .collect()
}

fn drive(j: &mut Journal, ids: &[u8], byte: u8) -> bool {
    match j.state.next().ok() {
        None => return false,
        Some(Action::Decide(decision)) => {
            let dead = matches!(decision.payload, DecisionPayload::DeadEnd(_));
            j.decide(decision);
            return !dead;
        }
        Some(Action::ReadRanking) => {
            j.ranking(ids);
        }
        Some(Action::RunTrial(plan)) => match outcome(byte) {
            Outcome::Failure => {
                let named = plan.load.cores().first().map(CoreId::get);
                let (intent, trial) = j.intent(&plan);
                let mut end =
                    Journal::end_payload(&trial, Outcome::Failure, Some(Signal::ComputationError));
                end.core = named.map(core);
                j.append(end.into(), &[intent]);
            }
            other @ (Outcome::Pass | Outcome::Inconclusive) => {
                j.trial(&plan, other);
            }
        },
    }
    true
}

proptest! {
    #![proptest_config(crate::prop_tests::seeded(40))]

    #[test]
    fn cycle_counts_maintained_at_fold_equal_a_brute_force_recount(
        script in proptest::collection::vec(any::<u8>(), 1..160),
    ) {
        let ids = [0_u8, 1, 2, 3];
        let mut j = Journal::on(&ids, pairs(&ids), &[Start::has_room(-20); 4]).resuming_every(7);
        for byte in script {
            if !drive(&mut j, &ids, byte) {
                break;
            }
            if let Some(counts) = j.run().checking.counts() {
                prop_assert_eq!(counts, &expected_counts(&j));
            }
        }
        j.assert_resumes();
    }
}

#[test]
fn an_r7_cycle_intent_waits_for_its_recorded_step() {
    let mut j = started(&[-10, -12]);
    j.cycle_start(1, &[Regime::R7]);
    let mut plan = j.manual(
        Regime::R7,
        0,
        Load::Many(set(&[0, 1])),
        Condition::Together,
        &[-10, -12],
        j.run().config.short_trial.get(),
    );
    plan.context = TrialContext::Cycle {
        cycle: std::num::NonZeroU32::MIN,
        step: crate::StepNo::new(std::num::NonZeroU32::MIN),
        part: crate::PartNo::new(std::num::NonZeroU32::MIN),
    };
    let intent = plan.intent(TrialId::from("r7-step".to_owned()), 0);
    let class = j.run().class_of_intent(&intent).unwrap();
    assert!(matches!(
        j.run().schedule_intent(&intent, class, &[]),
        Err(crate::FoldError::UnscheduledIntent(_))
    ));
    let step = j.next_decision();
    assert!(matches!(step.payload, DecisionPayload::CheckingStep(_)));
    j.decide(step);
    let (requirement, turn) = j.run().schedule_intent(&intent, class, &[]).unwrap();
    assert_eq!(requirement.kind, crate::RequirementKind::Cycle);
    assert_eq!(requirement.class, class);
    assert_eq!(
        requirement.step,
        Some(crate::StepNo::new(std::num::NonZeroU32::MIN))
    );
    assert!(turn.is_none());
    j.assert_resumes();
}

#[test]
fn a_checking_chain_rejects_an_unknown_ccd_without_changing_state() {
    let mut j = started(&[-10, -12]);
    j.cycle_start(1, &[Regime::R7]);
    let step = j.next_decision();
    j.decide(step);
    let before = j.state.clone();
    let ccd = togi_machine::CcdId(99);
    let result = j.try_append(
        togi_journal::CheckingChain {
            cycle: std::num::NonZeroU32::MIN,
            step: std::num::NonZeroU32::MIN,
            ccd,
            workload: togi_machine::pick_workload(Regime::R7, 0).id(),
            groups: vec![set(&[0, 1])],
            cores: CoreSet::default(),
            source_seqs: Vec::new(),
            profile: j.offsets().offsets(),
            part: std::num::NonZeroU32::MIN,
            rederived: None,
        }
        .into(),
        &[],
    );
    assert!(matches!(result, Err(crate::FoldError::UnknownCcd(found)) if found == ccd));
    assert_eq!(j.state.run(), before.run());
}

#[test]
fn a_single_ccd_r7_cycle_finishes_without_a_cross_ccd_trial() {
    let mut j = started(&[-10, -12]);
    j.load(&config(
        r#"{"checking":{"cycle":["R7"]},"durations":{"checking_all_core_s":120}}"#,
    ));
    let end = finish_cycle(&mut j);
    assert!(end.passed);
    assert!(!end.full);
    let intents: Vec<_> = j
        .events
        .iter()
        .filter_map(|event| {
            if let Payload::TrialIntent(intent) = &event.payload {
                Some(intent)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(intents.len(), 4);
    assert!(intents.iter().all(|intent| {
        intent.cores == set(&[0, 1])
            && intent.duration_s == j.run().config.short_trial.get()
            && intent.regime == Regime::R7
    }));
    j.assert_resumes();
}

#[test]
fn carried_passes_never_fulfill_a_checking_cycle_step() {
    let mut j = Journal::has_room(&[-20]);
    j.load(&config(r#"{"checking":{"cycle":["R1"]}}"#));
    let change = j.next_decision();
    j.decide(change);
    let class = (Regime::R1, 0, &[0u8][..], 120);
    let needed = j.run().config.needed.get();
    for _ in 0..needed {
        j.carry(
            "20230101T000000Z",
            class,
            Condition::Together,
            &[-20],
            Outcome::Pass,
        );
    }
    let start = j.next_decision();
    assert!(
        matches!(&start.payload, DecisionPayload::CheckingCycle(cycle) if cycle.event == CycleEvent::Start),
        "{start:?}"
    );
    j.decide(start);
    let mut action = j.next();
    while let Action::Decide(decision) = action {
        assert!(
            !matches!(&decision.payload, DecisionPayload::CheckingCycle(cycle) if cycle.event == CycleEvent::End),
            "carried passes finished the cycle: {decision:?}"
        );
        j.decide(decision);
        action = j.next();
    }
    let Action::RunTrial(plan) = action else {
        panic!("no live trial was asked for: {action:?}");
    };
    assert_eq!(
        (plan.regime, plan.condition),
        (Regime::R1, Condition::Together)
    );
    assert_eq!(j.run().checking.open().unwrap().steps_done(), 0);
    assert_eq!(finish_cycle(&mut j).event, CycleEvent::End);
    j.assert_resumes();
}

fn changed_config(byte: u8, other: u8) -> String {
    let pick = |from: &[u32]| from[usize::from(other) % from.len()];
    match byte % 4 {
        0 => format!(
            r#"{{"durations":{{"short_trial_s":{},"checking_all_core_s":{}}}}}"#,
            pick(&[30, 45, 60]),
            pick(&[90, 120, 180]),
        ),
        1 => format!(
            r#"{{"durations":{{"checking_trial_s":{},"checking_idle_s":{}}}}}"#,
            pick(&[60, 90, 120]),
            pick(&[30, 60, 90]),
        ),
        2 => format!(
            r#"{{"backends":{{"mprime":"{}","ycruncher":"{}"}}}}"#,
            pick_path(other, &[MPRIME_OLD, MPRIME_NEW]),
            pick_path(other >> 1, &[YCRUNCHER_OLD, YCRUNCHER_NEW]),
        ),
        _ => "{}".to_owned(),
    }
}

fn pick_path(byte: u8, paths: &[&'static str]) -> &'static str {
    paths[usize::from(byte) % paths.len()]
}

proptest! {
    #![proptest_config(crate::prop_tests::seeded(40))]

    #[test]
    fn the_cycle_and_exposure_kept_at_fold_equal_a_rebuild_from_scratch(
        script in proptest::collection::vec((any::<u8>(), any::<u8>(), any::<u8>()), 1..160),
    ) {
        let ids = [0_u8, 1, 2, 3];
        let mut j = Journal::on(&ids, pairs(&ids), &[Start::has_room(-20); 4]);
        for (op, byte, other) in script {
            match op % 12 {
                0 => {
                    j.load(&config(&changed_config(byte, other)));
                }
                1 => {
                    let depth = -i64::from(10 + byte % 30);
                    j.profile_change(&[depth, depth, depth, depth]);
                }
                _ => {
                    if !drive(&mut j, &ids, byte) {
                        break;
                    }
                }
            }
            let run = j.run();
            if let Some(open) = run.checking.open() {
                let (plan, valid, done) = run.debug_rebuild_cycle().unwrap();
                prop_assert_eq!(open.plan(), plan.as_slice());
                prop_assert_eq!(run.checking.counts().unwrap(), &valid);
                prop_assert_eq!(open.steps_done(), done);
            }
            let [maintained, rebuilt] = run.evidence.debug_rebuild();
            prop_assert_eq!(maintained, rebuilt);
        }
        j.assert_resumes();
    }
}
