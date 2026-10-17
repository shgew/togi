use serde::Deserialize;
use togi_journal::{CorePhase, CoreTransition, HuntOutcome, Outcome, Seq};
use togi_machine::{CcdId, Condition, Regime, Signal, WorkloadId};

use crate::r7::respond::R7Response;
use crate::testkit::{Journal, R7Report, Start, pairs, set};
use crate::tests::{core, off};
use crate::{Action, DecisionPayload, Load, TrialPlan};

fn values(j: &Journal) -> Vec<i64> {
    j.offsets().iter().map(|(_, offset)| offset.get()).collect()
}

fn session(ids: &[u8], offsets: &[i64]) -> Journal {
    let starts: Vec<Start> = offsets
        .iter()
        .map(|&offset| Start::has_room(offset))
        .collect();
    let mut j = Journal::new(ids, &starts).resuming_every(1);
    j.profile_change(offsets);
    j.ranking(ids);
    j
}

fn apply(j: &mut Journal) -> (Seq, togi_journal::TunerDecision) {
    let Some(R7Response::Backoff { decision, .. }) = j.run().r7_decision() else {
        panic!("expected R7 backoff, got {:?}", j.run().r7_decision());
    };
    let DecisionPayload::TunerDecision(payload) = &decision.payload else {
        panic!("expected tuner decision");
    };
    let payload = payload.clone();
    let seq = j.decide(decision);
    j.profile_change(&values(j));
    (seq, payload)
}

fn fail(j: &mut Journal, cores: &[u8], report: &R7Report) -> Seq {
    j.r7_failed(cores, &values(j), report)
}

fn probe(j: &Journal) -> TrialPlan {
    match j.run().r7_probe_action() {
        Some(Action::RunTrial(plan)) => plan,
        other => panic!("expected a partial probe, got {other:?}"),
    }
}

fn settled(j: &mut Journal) -> Action {
    loop {
        let next = j.next();
        match &next {
            Action::Decide(decision)
                if matches!(
                    decision.payload,
                    DecisionPayload::CorePhase(_) | DecisionPayload::ProfileChange(_)
                ) =>
            {
                j.decide(decision.clone());
            }
            Action::Decide(_) | Action::RunTrial(_) | Action::ReadRanking => return next,
        }
    }
}

fn finish(j: &mut Journal, plan: &TrialPlan, outcome: Outcome, named: Option<u8>) {
    let (intent, trial) = j.intent(plan);
    let mut end = Journal::end_payload(
        &trial,
        outcome,
        (outcome == Outcome::Failure).then_some(Signal::ComputationError),
    );
    end.core = named.map(core);
    end.duration_s = if outcome == Outcome::Pass {
        plan.duration.get()
    } else {
        1
    };
    j.append(end.into(), &[intent]);
}

#[test]
fn two_cumulative_charges_probe_before_full_reruns_even_with_interleaved_passes() {
    let mut j = session(&[0, 1, 2], &[-40, -42, -43]);
    let parent = [0, 1, 2];
    let top = R7Report::default().top(&[0]);
    fail(&mut j, &parent, &top);
    let (first, _) = apply(&mut j);
    assert_eq!(j.run().r7_probe_action(), None);
    for _ in 0..5 {
        j.r7_trial(&parent, &values(&j), Outcome::Pass, &top);
    }
    let failure = fail(&mut j, &parent, &top);
    let (second, moved) = apply(&mut j);
    assert_eq!((moved.core, moved.to_offset), (core(0), off(-38)));
    assert!(moved.reason.contains("two unattributed charges"));
    let part = probe(&j);
    assert_eq!(part.load, Load::Many(set(&[1, 2])));
    assert_eq!(part.profile, j.offsets());
    assert_eq!(part.cause, [failure, first, second]);
    assert_eq!(settled(&mut j), Action::RunTrial(part.clone()));
    for _ in 0..5 {
        j.r7_fact(true, &[1, 2], &values(&j), &R7Report::default());
    }
    assert_eq!(
        probe(&j),
        part,
        "carried passes cannot answer the experiment"
    );
    for passed in 0..5 {
        assert_eq!(probe(&j).requirement.needed.get(), 5);
        finish(&mut j, &part, Outcome::Pass, None);
        if passed < 4 {
            assert_eq!(probe(&j), part);
        }
    }
    assert_eq!(j.run().r7_probe_action(), None);
    assert_eq!(j.offsets().get(core(0)), Some(off(-38)));
    let Action::RunTrial(rerun) = settled(&mut j) else {
        panic!("the parent rerun must follow the probe");
    };
    assert_eq!(rerun.load.cores(), set(&parent));
    // A completed experiment rearms after two new charges, not after one.
    fail(&mut j, &parent, &top);
    apply(&mut j);
    assert_eq!(j.run().r7_probe_action(), None);
    j.assert_resumes();
}

#[test]
fn a_probe_failure_charges_its_named_core_not_the_idled_requester() {
    let mut j = session(&[0, 1, 2], &[-30, -40, -45]);
    let top = R7Report::default().top(&[0]);
    for _ in 0..2 {
        fail(&mut j, &[0, 1, 2], &top);
        apply(&mut j);
    }
    let part = probe(&j);
    finish(&mut j, &part, Outcome::Failure, Some(2));
    assert_eq!(j.run().r7_probe_action(), None);
    let attribution = j.next_decision();
    j.decide(attribution);
    let (_, moved) = apply(&mut j);
    assert_eq!((moved.core, moved.to_offset), (core(2), off(-44)));
    assert_eq!(j.offsets().get(core(0)), Some(off(-28)));
    j.assert_resumes();
}

#[test]
fn a_probe_failure_without_a_name_uses_its_own_top_group() {
    let mut j = session(&[0, 1, 2], &[-30, -40, -45]);
    for _ in 0..2 {
        fail(&mut j, &[0, 1, 2], &R7Report::default());
        apply(&mut j);
    }
    let part = probe(&j);
    finish(&mut j, &part, Outcome::Failure, None);
    j.decide(j.next_decision());
    let (_, moved) = apply(&mut j);
    assert_eq!(moved.core, core(1));
    assert_eq!(j.offsets().get(core(0)), Some(off(-28)));
}

#[test]
fn a_stall_outside_the_charged_group_accelerates_a_probe_never_blame() {
    for (stalled, wanted) in [(Some(1), true), (Some(0), false), (None, false)] {
        let mut j = session(&[0, 1, 2], &[-30, -40, -45]);
        let report = R7Report {
            stalled,
            ..R7Report::default().top(&[0])
        };
        fail(&mut j, &[0, 1, 2], &report);
        let (_, moved) = apply(&mut j);
        assert_eq!(moved.core, core(0), "stall is not attribution");
        assert_eq!(j.run().r7_probe_action().is_some(), wanted, "{stalled:?}");
        if wanted {
            assert_eq!(probe(&j).load.cores(), set(&[1, 2]));
            assert!(moved.reason.contains("chooses a probe, not a culprit"));
        }
    }
}

#[test]
fn all_core_loads_probe_each_charged_ccd_without_idling_the_other_ccd() {
    let ids = [0, 1, 2, 8, 9, 10];
    let mut j = session(&ids, &[-30, -40, -45, -31, -41, -46]);
    let report = R7Report::default().top(&[0, 8]);
    for _ in 0..2 {
        fail(&mut j, &ids, &report);
        apply(&mut j);
        apply(&mut j);
    }
    let first = probe(&j);
    assert_eq!(first.load.cores(), set(&[1, 2, 8, 9, 10]));
    for _ in 0..5 {
        finish(&mut j, &first, Outcome::Pass, None);
    }
    let second = probe(&j);
    assert_eq!(second.load.cores(), set(&[0, 1, 2, 9, 10]));
    for _ in 0..5 {
        finish(&mut j, &second, Outcome::Pass, None);
    }
    assert_eq!(j.run().r7_probe_action(), None);
    j.assert_resumes();
}

#[test]
fn tied_requesters_idle_together_and_no_singleton_or_empty_part_runs() {
    for (ids, top, expected) in [
        (vec![0, 1, 2, 3], vec![0, 1], Some(vec![2, 3])),
        (vec![0, 1, 2], vec![0, 1], None),
        (vec![0, 1], vec![0], None),
        (vec![0, 1], vec![0, 1], None),
    ] {
        let mut j = session(&ids, &vec![-30; ids.len()]);
        let report = R7Report::default().top(&top);
        for _ in 0..2 {
            fail(&mut j, &ids, &report);
            apply(&mut j);
        }
        assert_eq!(
            j.run().r7_probe_action().map(|action| {
                let Action::RunTrial(plan) = action else {
                    panic!("not a trial: {action:?}");
                };
                plan.load.cores()
            }),
            expected.as_deref().map(set),
        );
    }
}

#[test]
fn probe_retry_resume_duration_and_reset_are_context_scoped() {
    let mut j = session(&[0, 1, 2], &[-30, -40, -45]);
    for _ in 0..2 {
        fail(&mut j, &[0, 1, 2], &R7Report::default());
        apply(&mut j);
    }
    let part = probe(&j);
    finish(&mut j, &part, Outcome::Pass, None);
    finish(&mut j, &part, Outcome::Inconclusive, None);
    let retry = probe(&j);
    assert!(retry.retry);
    assert_eq!(retry.requirement, part.requirement);
    assert_eq!(
        j.resumed().run().r7_probe_action(),
        Some(Action::RunTrial(retry))
    );
    j.reset(1);
    for _ in 0..8 {
        let action = j.next();
        let Action::Decide(decision) = action else {
            break;
        };
        j.decide(decision);
        if j.run().r7_probe_action().is_none() {
            break;
        }
    }
    assert_eq!(j.run().r7_probe_action(), None);
    j.assert_resumes();
}

#[test]
fn all_core_stall_limits_the_charge_ccd_and_retains_the_stalled_core_in_its_probe() {
    let ids = [0, 1, 2, 8, 9, 10];
    let mut j = session(&ids, &[-30, -40, -45, -31, -41, -46]);
    fail(&mut j, &ids, &R7Report::default().top(&[0, 8]).stalled(9));
    let (_, moved) = apply(&mut j);
    assert_eq!(moved.core, core(8));
    assert_eq!(j.offsets().get(core(0)), Some(off(-30)));
    assert_eq!(probe(&j).load.cores(), set(&[0, 1, 2, 9, 10]));
}

#[test]
fn charge_counts_do_not_cross_workloads() {
    let mut j = session(&[0, 1, 2], &[-30, -40, -45]);
    fail(&mut j, &[0, 1, 2], &R7Report::default());
    apply(&mut j);
    let other = j.manual(
        Regime::R7,
        1,
        Load::Many(set(&[0, 1, 2])),
        Condition::Together,
        &values(&j),
        120,
    );
    finish(&mut j, &other, Outcome::Failure, None);
    j.decide(j.next_decision());
    apply(&mut j);
    assert_eq!(j.run().r7_probe_action(), None);
    fail(&mut j, &[0, 1, 2], &R7Report::default());
    apply(&mut j);
    assert_eq!(probe(&j).load.cores(), set(&[1, 2]));
}

#[test]
fn a_short_crash_probes_at_its_intended_duration_and_needs_fresh_passes() {
    let mut j = session(&[0, 1, 2, 8], &[-30, -40, -45, -49]);
    for _ in 0..2 {
        let plan = j.manual(
            Regime::R7,
            0,
            Load::Many(set(&[0, 1, 2])),
            Condition::Together,
            &values(&j),
            300,
        );
        finish(&mut j, &plan, Outcome::Failure, None);
        j.decide(j.next_decision());
        apply(&mut j);
    }
    let part = probe(&j);
    assert_eq!(part.duration.get(), 300);
    assert_eq!(part.requirement.needed.get(), 5);
    finish(&mut j, &part, Outcome::Pass, None);
    assert_eq!(probe(&j).duration.get(), 300);
}

#[test]
fn named_and_carried_charges_do_not_trigger_a_partial_probe() {
    let mut j = session(&[0, 1, 2], &[-30, -40, -45]);
    fail(&mut j, &[0, 1, 2], &R7Report::default().named(0));
    apply(&mut j);
    fail(&mut j, &[0, 1, 2], &R7Report::default());
    apply(&mut j);
    assert_eq!(j.run().r7_probe_action(), None);
    let mut j = session(&[0, 1, 2], &[-30, -40, -45]);
    for _ in 0..2 {
        j.r7_fact(
            false,
            &[0, 1, 2],
            &values(&j),
            &R7Report::default().top(&[0]),
        );
        apply(&mut j);
    }
    assert_eq!(j.run().r7_probe_action(), None);
}

fn set_core(j: &mut Journal, id: u8, value: i64) {
    j.core_phase(CoreTransition {
        core: core(id),
        from: None,
        to: CorePhase::HasRoom,
        offset: off(value),
        pass: None,
        failure_point: None,
        check_solo_limit: false,
        workloads: Vec::new(),
        cleared_combination: Vec::new(),
        reason: "test".to_owned(),
    });
    j.profile_change(&values(j));
}

fn loaded_locate() -> (Journal, Seq) {
    let ids = [0, 1, 2, 3];
    let mut j = Journal::on(&ids, pairs(&ids), &[Start::has_room(-30); 4]).resuming_every(1);
    j.profile_change(&[-30; 4]);
    j.ranking(&ids);
    for _ in 0..2 {
        fail(&mut j, &[0, 1], &R7Report::default().top(&[0]));
        apply(&mut j);
    }
    fail(&mut j, &[0, 1], &R7Report::default().top(&[0]));
    let Action::Decide(start) = settled(&mut j) else {
        panic!("expected hunt.start")
    };
    assert!(matches!(start.payload, DecisionPayload::HuntStart(_)));
    j.decide(start);
    let Action::Decide(group) = settled(&mut j) else {
        panic!("expected hunt.group")
    };
    assert!(matches!(group.payload, DecisionPayload::HuntGroup(_)));
    j.decide(group);
    let Action::RunTrial(plan) = settled(&mut j) else {
        panic!("expected locate trial")
    };
    finish(&mut j, &plan, Outcome::Failure, None);
    j.decide(j.next_decision());
    let Action::Decide(end) = settled(&mut j) else {
        panic!("expected hunt.end")
    };
    assert!(
        matches!(&end.payload, DecisionPayload::HuntEnd(end) if end.result == HuntOutcome::Loaded)
    );
    let ended = j.decide(end);
    apply(&mut j);
    (j, ended)
}

#[test]
fn loaded_locate_is_not_repeated_after_each_loaded_backoff() {
    let (mut j, ended) = loaded_locate();
    for _ in 0..3 {
        fail(&mut j, &[0, 1], &R7Report::default().top(&[0]));
        assert_eq!(j.run().locate_due(), None);
        let Some(R7Response::Backoff { decision, .. }) = j.run().r7_decision() else {
            panic!("expected loaded backoff")
        };
        assert!(decision.cause.contains(&ended));
        let DecisionPayload::TunerDecision(moved) = &decision.payload else {
            panic!("backoff")
        };
        assert!(
            moved
                .reason
                .contains("already kept this load's failure on the loaded cores")
        );
        apply(&mut j);
    }
    j.assert_resumes();
}

#[test]
fn loaded_locate_rearms_on_a_pass_or_unloaded_change_not_a_loaded_change() {
    let (mut j, ended) = loaded_locate();
    let load = j.run().r7.loaded.keys().copied().next().unwrap();
    set_core(&mut j, 0, -20);
    assert_eq!(j.run().r7_loaded_verdict(load, &j.offsets()), Some(ended));
    set_core(&mut j, 2, -29);
    assert_eq!(j.run().r7_loaded_verdict(load, &j.offsets()), None);
    fail(&mut j, &[0, 1], &R7Report::default().top(&[0]));
    assert!(j.run().locate_due().is_some());
    let (mut j, _) = loaded_locate();
    j.r7_trial(&[0, 1], &values(&j), Outcome::Pass, &R7Report::default());
    assert!(j.run().r7.loaded.is_empty());
    for _ in 0..2 {
        fail(&mut j, &[0, 1], &R7Report::default().top(&[0]));
        assert_eq!(j.run().locate_due(), None);
        apply(&mut j);
    }
    fail(&mut j, &[0, 1], &R7Report::default().top(&[0]));
    assert!(j.run().locate_due().is_some());
    j.assert_resumes();
    let (mut carried, _) = loaded_locate();
    carried.r7_fact(true, &[0, 1], &values(&carried), &R7Report::default());
    assert!(carried.run().r7.loaded.is_empty());
    let (mut reset, _) = loaded_locate();
    reset.reset(2);
    assert!(reset.run().r7.loaded.is_empty());
    reset.assert_resumes();
}

#[test]
fn a_loaded_verdict_does_not_suppress_the_zero_offset_diagnostic() {
    let (mut j, ended) = loaded_locate();
    let load = j.run().r7.loaded.keys().copied().next().unwrap();
    set_core(&mut j, 0, 0);
    set_core(&mut j, 1, 0);
    assert_eq!(j.run().r7_loaded_verdict(load, &j.offsets()), Some(ended));
    fail(&mut j, &[0, 1], &R7Report::default().top(&[0]));
    assert!(j.run().locate_due().is_some());
    j.assert_resumes();
}

#[derive(Deserialize)]
struct Ratchet {
    initial_profile: Vec<i64>,
    parent: Vec<u8>,
    workload: WorkloadId,
    trials: Vec<RecordedTrial>,
    partial: RecordedPartial,
}

#[derive(Deserialize)]
struct RecordedTrial {
    core03: i64,
    planned_s: u32,
    duration_s: u32,
    outcome: Outcome,
    signal: Option<Signal>,
    requests: Vec<(u8, f64)>,
    mhz: u32,
}

#[derive(Deserialize)]
struct RecordedPartial {
    cores: Vec<u8>,
    planned_s: u32,
    duration_s: u32,
    named: u8,
}

#[test]
fn recorded_core03_ratchet_probes_after_two_charges_despite_five_interleaved_passes() {
    // Trials 0843–0849 are extracted observations, not old tuner decisions. The new decisions
    // reproduce their applied profiles until this rule diverts to the partial before trial 0850.
    let recorded: Ratchet = serde_json::from_str(include_str!("core03-charge-loop.json")).unwrap();
    let ids: Vec<u8> = (0..16).collect();
    let mut j = session(&ids, &recorded.initial_profile);
    let mut moved = Vec::new();
    for sample in recorded.trials {
        assert_eq!(j.offsets().get(core(3)), Some(off(sample.core03)));
        let mut plan = j.manual(
            Regime::R7,
            1,
            Load::Many(set(&recorded.parent)),
            Condition::Together,
            &values(&j),
            sample.planned_s,
        );
        plan.workload = crate::WorkloadChoice::Frozen(recorded.workload);
        let (intent, trial) = j.intent(&plan);
        let mut end = Journal::end_payload(&trial, sample.outcome, sample.signal);
        end.duration_s = sample.duration_s;
        end.voltage_requests_v = sample
            .requests
            .into_iter()
            .map(|(id, request)| (core(id), request))
            .collect();
        end.top_requesters = set(&[3]);
        end.ccd_mhz.insert(CcdId(0), sample.mhz);
        j.append(end.into(), &[intent]);
        if sample.outcome == Outcome::Failure {
            j.decide(j.next_decision());
            let (_, decision) = apply(&mut j);
            assert_eq!(decision.core, core(3));
            moved.push(decision.to_offset.get());
        }
    }
    assert_eq!(moved, [-3, -2]);
    let part = probe(&j);
    assert_eq!(part.load.cores(), set(&recorded.partial.cores));
    assert_eq!(part.duration.get(), recorded.partial.planned_s);
    assert_eq!(settled(&mut j), Action::RunTrial(part.clone()));
    // Counterfactual input, explicitly not a prediction: the first recorded partial (0858)
    // failed on core 00 with exactly this load. It ran later with idle core 03 at -1, not -2.
    let (intent, trial) = j.intent(&part);
    let mut end = Journal::end_payload(&trial, Outcome::Failure, Some(Signal::ComputationError));
    end.core = Some(core(recorded.partial.named));
    end.duration_s = recorded.partial.duration_s;
    j.append(end.into(), &[intent]);
    j.decide(j.next_decision());
    let (_, decision) = apply(&mut j);
    assert_eq!(decision.core, core(0));
    assert_eq!(j.offsets().get(core(3)), Some(off(-2)));
    j.assert_resumes();
}
