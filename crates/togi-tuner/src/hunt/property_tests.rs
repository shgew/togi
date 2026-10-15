use proptest::prelude::*;
use togi_journal::{HuntOutcome, Outcome};
use togi_machine::{Condition, CoreId, Regime};

use super::plan::GroupOutcome;
use super::support::{hunt_session, passed_cycle};
use crate::testkit::{Journal, config, fresh, set};
use crate::tests::core;
use crate::{Action, DecisionAction, DecisionPayload, Load, TrialPlan};

const OFFSET: i64 = -10;

fn failing_with(culprits: Vec<CoreId>) -> impl Fn(&TrialPlan) -> Outcome {
    move |plan| {
        let all = culprits.iter().all(|&culprit| {
            plan.profile
                .get(culprit)
                .is_some_and(|offset| offset.get() <= OFFSET)
        });
        if all { Outcome::Failure } else { Outcome::Pass }
    }
}

fn started(cores: usize) -> Journal {
    let offsets = vec![OFFSET; cores];
    let shallow = vec![-5; cores];
    let mut j = hunt_session(&offsets);
    passed_cycle(&mut j, &shallow, &offsets);
    j.together_failure(Regime::R6, 0, &offsets);
    j.start_hunt();
    j
}

#[test]
fn settled_group_answer_is_frozen() {
    let mut j = started(4);
    let (group, cause) = j.group_plan();
    j.append(group.clone().into(), &cause);
    for _ in 0..5 {
        let plan = j.next_trial();
        j.trial(&plan, Outcome::Pass);
    }
    let (second, cause) = j.group_plan();
    j.append(second.into(), &cause);
    let before = j.run().hunt_groups();
    assert_eq!(before[0].outcome, Some(GroupOutcome::Passed));

    let rejected = j.manual(
        Regime::R6,
        0,
        Load::Many(set(&[0, 1, 2, 3])),
        Condition::Together,
        &[OFFSET, OFFSET, 0, 0],
        group.duration_s,
    );
    j.trial(&rejected, Outcome::Failure);
    let attribution = j.next_decision();
    j.decide(attribution);

    let after = j.run().hunt_groups();
    assert_eq!(after[0].outcome, Some(GroupOutcome::Passed));
    assert_eq!(after[0], before[0]);
    j.assert_resumes();
}

#[test]
fn hunt_decide_is_pure_and_repeats_through_a_whole_hunt() {
    let mut j = started(4).resuming_every(1);
    let oracle = failing_with(vec![core(2)]);
    let mut decided = Vec::new();
    for _ in 0..400 {
        let before = j.state.clone();
        let first = j.state.next().ok();
        assert_eq!(first, j.state.next().ok());
        assert_eq!(before, j.state);
        decided.push((j.events.len(), first));
        let action = j.step(&mut |plan: &TrialPlan| oracle(plan));
        if matches!(
            action,
            Action::Decide(DecisionAction {
                payload: DecisionPayload::TunerDecision(_),
                ..
            })
        ) {
            break;
        }
    }
    for (len, expected) in decided {
        assert_eq!(
            fresh(&j.events[..len]).next().ok(),
            expected,
            "prefix {len}"
        );
    }
}

#[test]
fn a_changed_pass_count_mid_hunt_does_not_change_the_hunt_threshold() {
    let mut j = started(4);
    let hunt = j.run().hunts.active.as_ref().unwrap().needed;
    j.load(&config(r#"{"evidence":{"miss":0.9,"rate":0.5}}"#));
    assert_ne!(j.run().config.needed, hunt);
    let (group, cause) = j.group_plan();
    j.append(group.into(), &cause);
    let plan = j.next_trial();
    assert_eq!(plan.requirement.needed, hunt);
    for _ in 0..hunt.get() - 1 {
        j.trial(&plan, Outcome::Pass);
    }
    assert!(matches!(j.next(), Action::RunTrial(_)));
    j.trial(&plan, Outcome::Pass);
    assert!(matches!(j.next(), Action::Decide(_)));
    j.assert_resumes();
}

#[test]
fn a_group_reaching_a_new_constraint_is_replaced_by_a_skipped_group_of_the_same_number() {
    let mut j = started(4);
    let (group, cause) = j.group_plan();
    j.append(group.clone().into(), &cause);
    j.combination(1, &[(0, OFFSET), (1, OFFSET)]);
    let Action::Decide(replacement) = j.next() else {
        panic!("expected the skipped replacement");
    };
    let DecisionPayload::HuntGroup(skipped) = &replacement.payload else {
        panic!("expected a hunt group");
    };
    assert_eq!(skipped.group, group.group);
    assert!(skipped.skipped);
    assert_eq!(skipped.inferred, None);
    j.decide(replacement);
    let active = j.run().hunts.active.as_ref().unwrap();
    assert_eq!(active.groups.len(), 1);
    j.assert_resumes();
}

#[test]
fn hunt_decide_visits_distinct_cells_not_passes() {
    fn visits(passes: usize) -> u64 {
        let mut j = hunt_session(&[OFFSET; 4]);
        j.load(&config(r#"{"evidence":{"miss":0.9,"rate":0.5}}"#));
        passed_cycle(&mut j, &[-5; 4], &[OFFSET; 4]);
        j.together_failure(Regime::R6, 0, &[OFFSET; 4]);
        j.start_hunt();
        let (group, cause) = j.group_plan();
        j.append(group.into(), &cause);
        let plan = j.next_trial();
        for _ in 0..passes {
            j.trial(&plan, Outcome::Pass);
        }
        j.state.visits();
        let _ = j.state.next().ok();
        j.state.visits()
    }
    let small = visits(50);
    let large = visits(5000);
    assert!(small > 0);
    assert_eq!(small, large);
}

proptest! {
    #![proptest_config(crate::prop_tests::seeded(24))]

    #[test]
    fn culprit_and_pair_terminate(
        first in 0u8..16,
        second in 0u8..16,
        pair in any::<bool>(),
    ) {
        let culprits: Vec<CoreId> = if pair && first != second {
            vec![core(first.min(second)), core(first.max(second))]
        } else {
            vec![core(first)]
        };
        let mut j = started(16);
        let end = j.run_hunt(1200, failing_with(culprits.clone()));
        prop_assert!(end.groups <= 194, "{} groups", end.groups);
        match end.result {
            HuntOutcome::Culprit => {
                prop_assert_eq!(culprits.len(), 1);
                prop_assert_eq!(end.cores.first(), Some(culprits[0]));
            }
            HuntOutcome::Combination => {
                let found: Vec<CoreId> = end.members.iter().map(|m| m.core).collect();
                prop_assert_eq!(found, culprits);
            }
            other => prop_assert!(false, "unexpected end {other:?}"),
        }
    }
}

#[test]
fn a_probe_that_passed_can_become_a_failure_and_the_hunt_reads_it_again() {
    let mut j = started(4);
    let oracle = failing_with(vec![core(0), core(1)]);
    let probe = loop {
        let action = j.step(&mut |plan: &TrialPlan| oracle(plan));
        if let Action::Decide(DecisionAction {
            payload: DecisionPayload::HuntGroup(group),
            ..
        }) = action
        {
            if group.probe.is_some() {
                break group;
            }
        }
    };
    for _ in 0..5 {
        let plan = j.next_trial();
        j.trial(&plan, Outcome::Pass);
    }
    let last = |j: &Journal| j.run().hunt_groups().pop().unwrap();
    assert_eq!(last(&j).outcome, Some(GroupOutcome::Passed));
    let values: Vec<i64> = probe
        .profile
        .iter()
        .map(|(_, offset)| offset.get())
        .collect();
    let rejected = j.manual(
        Regime::R6,
        0,
        Load::Many(set(&[0, 1, 2, 3])),
        Condition::Together,
        &values,
        probe.duration_s,
    );
    j.trial(&rejected, Outcome::Failure);
    let attribution = j.next_decision();
    j.decide(attribution);
    assert_eq!(last(&j).outcome, Some(GroupOutcome::Failed));
    j.assert_resumes();
}

#[test]
fn the_cut_counts_the_splits_the_hunt_ran_and_a_located_source_is_registered_only_for_located_hunts()
 {
    let mut j = started(4);
    assert_eq!(j.run().hunt_cut(), 0);
    let source = j.run().hunts.active.as_ref().unwrap().start.failure;
    assert!(!j.run().hunts.is_located(source));
    let oracle = failing_with(vec![core(2)]);
    for _ in 0..40 {
        let action = j.step(&mut |plan: &TrialPlan| oracle(plan));
        if matches!(
            action,
            Action::Decide(DecisionAction {
                payload: DecisionPayload::HuntEnd(_),
                ..
            })
        ) {
            break;
        }
    }
    assert!(j.run().hunt_cut() >= 2);
}

fn carry_parked(j: &mut Journal, named: u8, profile: &[i64], short: u32) -> togi_journal::Seq {
    use std::collections::BTreeMap;
    use togi_machine::{BootId, PerCore, Signal, WorkloadId};
    let seq = j.next_seq();
    let payload = togi_journal::TrialCarried {
        source: togi_journal::FactSource {
            session: "20230101T000000Z".parse().unwrap(),
            seq,
            build: crate::testkit::build(),
            trial: None,
            time: jiff::Timestamp::from_second(1_600_000_000).unwrap(),
            boot: BootId::from("old".to_owned()),
            backend: None,
        },
        class: togi_journal::TrialClass {
            regime: Regime::R6,
            workload: WorkloadId::new(Regime::R6, 0),
            cores: set(&[0, 1, 2, 3]),
            duration_s: short,
        },
        condition: Condition::Parked,
        activity: None,
        rerun: false,
        profile: j.profile(profile).offsets(),
        outcome: Outcome::Failure,
        signal: Some(Signal::ComputationError),
        duration_s: short,
        core: Some(core(named)),
        stalled_core: None,
        voltage_requests_v: PerCore::default(),
        top_requesters: togi_machine::CoreSet::default(),
        ccd_mhz: BTreeMap::new(),
    };
    j.append(payload.into(), &[])
}

fn repeated_session() -> (Journal, togi_journal::Seq, togi_journal::Seq, u32) {
    let offsets = vec![OFFSET; 4];
    let mut j = hunt_session(&offsets);
    let short = j.run().config.short_trial.get();
    j.core_phase(togi_journal::CoreTransition {
        core: core(2),
        from: None,
        to: togi_journal::CorePhase::AtLimit,
        offset: crate::tests::off(OFFSET),
        pass: None,
        failure_point: Some(crate::tests::off(-11)),
        check_solo_limit: false,
        workloads: Vec::new(),
        cleared_combination: Vec::new(),
        reason: "test".to_owned(),
    });
    let older = carry_parked(&mut j, 2, &[0, 0, -12, 0], short);
    let newer = carry_parked(&mut j, 2, &[0, 0, -11, 0], short);
    (j, older, newer, short)
}

#[test]
fn repeated_core_probe_is_decided_when_the_hunt_starts() {
    let (mut j, older, newer, _) = repeated_session();
    j.together_failure(Regime::R6, 0, &[OFFSET; 4]);
    let (_, start) = j.start_hunt();
    let repeated = j.run().hunts.active.as_ref().unwrap().repeated.unwrap();
    assert_eq!(repeated.core, core(2));
    assert_eq!(repeated.failures, [older, newer]);
    let (group, cause) = j.group_plan();
    assert_eq!(group.cores, set(&[2]));
    assert_eq!(
        group.granularity,
        u32::try_from(start.candidates.len()).unwrap()
    );
    assert!(cause.contains(&older) && cause.contains(&newer));
    let reason = group.reason.as_deref().unwrap();
    assert!(
        reason.contains("two parked failures in this class followed one-count backoffs on core 02")
    );
    assert!(!reason.contains("valid short trials"), "{reason}");
    assert!(!reason.contains("test the failed duration"), "{reason}");
    j.append(group.clone().into(), &cause);

    carry_parked(&mut j, 1, &[0, -12, 0, 0], group.duration_s);
    carry_parked(&mut j, 1, &[0, -11, 0, 0], group.duration_s);
    let active = j.run().hunts.active.as_ref().unwrap();
    assert_eq!(active.repeated.unwrap().core, core(2));
    j.assert_resumes();
}

#[test]
fn repeated_core_probe_uses_the_pair_before_its_source() {
    let (mut j, older, newer, short) = repeated_session();
    let source = j.together_failure(Regime::R6, 0, &[OFFSET; 4]);
    carry_parked(&mut j, 1, &[0, -12, 0, 0], short);
    carry_parked(&mut j, 1, &[0, -11, 0, 0], short);
    let (_, start) = j.start_hunt();
    assert_eq!(start.failure, source);
    let repeated = j.run().hunts.active.as_ref().unwrap().repeated.unwrap();
    assert_eq!(repeated.core, core(2));
    assert_eq!(repeated.failures, [older, newer]);
    let (group, _) = j.group_plan();
    assert_eq!(group.cores, set(&[2]));
    j.assert_resumes();
}

#[test]
fn a_newer_failure_in_the_pair_cell_after_the_source_does_not_hide_the_pair_before_it() {
    let (mut j, older, newer, short) = repeated_session();
    j.together_failure(Regime::R6, 0, &[OFFSET; 4]);
    carry_parked(&mut j, 2, &[0, 0, -11, 0], short);
    carry_parked(&mut j, 2, &[0, 0, -12, 0], short);
    j.start_hunt();
    let repeated = j.run().hunts.active.as_ref().unwrap().repeated.unwrap();
    assert_eq!(repeated.core, core(2));
    assert_eq!(repeated.failures, [older, newer]);
    j.assert_resumes();
}

#[test]
fn the_pre_source_pair_lookup_does_not_grow_with_the_parked_failure_history() {
    use super::state::{LOOKUP_PROBES, Parked, ParkedFailure};
    fn probes(history: u64) -> u64 {
        let mut parked = Parked::default();
        for seq in 1..=history {
            parked.add(ParkedFailure {
                seq: togi_journal::Seq::new(seq).unwrap(),
                core: core(2),
                offset: crate::tests::off(-12),
                carried: true,
            });
        }
        LOOKUP_PROBES.with(|count| count.set(0));
        let before = togi_journal::Seq::new(history / 2).unwrap();
        let [newer, older] = parked.newest_two_before(before).unwrap();
        assert_eq!(
            (older.seq.get(), newer.seq.get()),
            (history / 2 - 2, history / 2 - 1)
        );
        LOOKUP_PROBES.with(std::cell::Cell::get)
    }
    let small = probes(64);
    let large = probes(65_536);
    assert!(small <= 7 && large <= 17, "{small} {large}");
}

#[test]
fn a_reset_of_a_loaded_core_discards_the_carried_failures_the_repeated_core_probe_needs() {
    let (mut j, ..) = repeated_session();
    j.reset(3);
    j.core_phase(togi_journal::CoreTransition {
        core: core(3),
        from: None,
        to: togi_journal::CorePhase::HasRoom,
        offset: crate::tests::off(OFFSET),
        pass: None,
        failure_point: None,
        check_solo_limit: false,
        workloads: Vec::new(),
        cleared_combination: Vec::new(),
        reason: "test".to_owned(),
    });
    j.together_failure(Regime::R6, 0, &[OFFSET; 4]);
    let (_, _) = j.start_hunt();
    assert_eq!(j.run().hunts.active.as_ref().unwrap().repeated, None);
    j.assert_resumes();
}

#[test]
fn an_attributed_idle_failure_during_a_group_application_ends_the_hunt_direct() {
    use togi_journal::{Attribution, Failure};
    use togi_machine::Signal;
    let mut j = started(4);
    let (group, cause) = j.group_plan();
    j.append(group.clone().into(), &cause);
    let applied = crate::tests::off(OFFSET);
    let failure = j.failure(
        Failure {
            signal: Signal::Crash,
            attribution: Attribution::Attributed,
            core: Some(core(1)),
            offset: Some(applied),
            trial: None,
            regime: Some(Regime::R6),
            condition: Some(Condition::Parked),
            profile: Some(group.profile),
            known_failure: None,
            reason: None,
            round: None,
        },
        &[],
    );
    let Action::Decide(decision) = j.next() else {
        panic!("expected the hunt end");
    };
    let DecisionPayload::HuntEnd(end) = &decision.payload else {
        panic!("expected a hunt end, got {decision:?}");
    };
    assert_eq!(end.result, HuntOutcome::Direct);
    assert_eq!(end.cores, set(&[1]));
    assert_eq!(decision.cause, [failure]);
    j.decide(decision);
    j.assert_resumes();
}

fn member_probe_oracle(plan: &TrialPlan) -> Outcome {
    let deep = |core_id: u8, limit: i64| {
        plan.profile
            .get(core(core_id))
            .is_some_and(|offset| offset.get() <= limit)
    };
    if deep(0, -7) && deep(1, -9) {
        Outcome::Failure
    } else {
        Outcome::Pass
    }
}

fn probing(redundant: usize, settled_probes: usize) -> (Journal, u64, crate::Action) {
    let mut j = started(4);
    let mut seen: Vec<(Vec<i64>, u32, Outcome)> = Vec::new();
    for _ in 0..400 {
        let action = j.next();
        if let Action::Decide(DecisionAction {
            payload: DecisionPayload::HuntGroup(group),
            ..
        }) = &action
        {
            if group.probe.is_some() {
                if seen.len() == settled_probes {
                    break;
                }
                let values: Vec<i64> = group.profile.iter().map(|(_, o)| o.get()).collect();
                let plan = j.manual(
                    Regime::R6,
                    0,
                    Load::Many(set(&[0, 1, 2, 3])),
                    Condition::Together,
                    &values,
                    group.duration_s,
                );
                seen.push((values, group.duration_s, member_probe_oracle(&plan)));
            }
        }
        j.step(&mut |plan: &TrialPlan| member_probe_oracle(plan));
    }
    assert_eq!(seen.len(), settled_probes, "the hunt reached the probe");
    for (values, duration, outcome) in &seen {
        for _ in 0..redundant {
            j.carry(
                "20261002T004254Z",
                (Regime::R6, 0, &[0, 1, 2, 3], *duration),
                Condition::Together,
                values,
                *outcome,
            );
        }
    }
    j.state.visits();
    let action = j.next();
    let visits = j.state.visits();
    (j, visits, action)
}

#[test]
fn hunt_cost_distinct_probes() {
    for settled_probes in [2, 3, 4] {
        let (small, small_visits, small_next) = probing(3, settled_probes);
        let (large, large_visits, large_next) = probing(300, settled_probes);
        assert!(small_visits > 0);
        assert_eq!(small_visits, large_visits, "{settled_probes} probes");
        let probe_of = |action: &Action| match action {
            Action::Decide(DecisionAction {
                payload: DecisionPayload::HuntGroup(group),
                ..
            }) => (group.probe, group.profile),
            other => panic!("expected the next probe group, got {other:?}"),
        };
        assert_eq!(probe_of(&small_next), probe_of(&large_next));
        assert_eq!(
            small.run().hunt_groups().len(),
            large.run().hunt_groups().len()
        );
    }
}
