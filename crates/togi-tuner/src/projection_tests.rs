use std::num::NonZeroU32;

use proptest::prelude::*;
use proptest::strategy::ValueTree;
use proptest::test_runner::{RngSeed, TestRunner};
use togi_journal::{Activity, CorePhase, DeepeningRound, Outcome, RoundEvent, Seq};
use togi_machine::{Condition, CoreSet, Regime, Signal, workloads};

use crate::dashboard::TrialRequirement;
use crate::phase_plan::{Candidate, PhaseNo};
use crate::projection::{CoreView, DeepeningCheck, DeepeningView, Projection, TemperaturePeak};
use crate::prop_tests::seeded;
use crate::testkit::{Journal, Start, fresh, set};
use crate::tests::{core, off};
use crate::{
    Action, BiosProfile, Confirmed, DecisionPayload, Depth, Load, Profile, State, TrialHistory,
    TrialPlan,
};

fn project(j: &Journal) -> Projection {
    let before = j.state.clone();
    let projection = j.state.project();
    assert_eq!(j.state, before);
    assert_eq!(j.state.project(), projection);
    assert_eq!(fresh(&j.events).project(), projection);
    projection
}

fn assert_roundtrip(projection: &Projection) {
    let encoded = serde_json::to_vec(projection).unwrap();
    let decoded: Projection = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(&decoded, projection);
}

fn end_phase1(j: &mut Journal, offsets: &[i64]) -> Seq {
    j.profile_change(offsets);
    j.cycle_start(1, &[Regime::R1]);
    j.cycle_end(1, true, true)
}

#[test]
fn project_fills_phases_and_bios() {
    let mut j = Journal::new(&[1, 9], &[Start::solo(-10, -13), Start::solo(-8, -9)]);
    let before = project(&j);
    assert_eq!(
        before.cores,
        [
            CoreView {
                core: core(1),
                phase: Some(CorePhase::HasRoom),
                offset: off(-10),
                pass: Some(off(-13)),
                failure_point: Some(off(-14)),
                solo_limit: Some(off(-13)),
            },
            CoreView {
                core: core(9),
                phase: Some(CorePhase::HasRoom),
                offset: off(-8),
                pass: Some(off(-9)),
                failure_point: Some(off(-10)),
                solo_limit: Some(off(-9)),
            },
        ]
    );
    let phase = before.phase_plan.unwrap();
    assert_eq!(phase.phase, PhaseNo::One);
    assert_eq!(phase.phase1_end, None);
    assert_eq!(phase.round, None);
    assert_eq!(phase.rounds_left, 0);
    assert!(phase.candidates.is_empty());
    assert_eq!(
        before.bios_profile,
        Some(BiosProfile {
            confirmed: None,
            shown: j.profile(&[-10, -8]),
            unconfirmed: Vec::new(),
            first_result: false,
        })
    );
    assert!(!before.first_result);
    assert_eq!(before.checking.profile, None);

    let end = end_phase1(&mut j, &[-10, -8]);
    let after = project(&j);
    assert_eq!(
        after.bios_profile,
        Some(BiosProfile {
            confirmed: Some(Confirmed {
                profile: j.profile(&[-10, -8]),
                seq: end,
            }),
            shown: j.profile(&[-10, -8]),
            unconfirmed: Vec::new(),
            first_result: true,
        })
    );
    let phase = after.phase_plan.as_ref().unwrap();
    assert_eq!(phase.phase, PhaseNo::Two);
    assert_eq!(phase.phase1_end, Some(end));
    assert_eq!(phase.rounds_left, 3);
    assert_eq!(
        phase.candidates,
        [
            Candidate {
                core: core(1),
                offset: off(-10),
                solo_limit: Some(off(-13)),
                gap: 3,
                carried: false,
                moving: false,
            },
            Candidate {
                core: core(9),
                offset: off(-8),
                solo_limit: Some(off(-9)),
                gap: 1,
                carried: false,
                moving: false,
            },
        ]
    );
    assert!(after.first_result);
    assert_eq!(after.clean_cycles, 0);
    assert_roundtrip(&after);
}

#[test]
fn cycle_start_invalidates_projected_checking() {
    let mut j = Journal::new(&[3], &[Start::has_room(-10)]);
    let profile_seq = j.profile_change(&[-10]);
    j.cycle_start(1, &[Regime::R1]);
    let plan = j.next_trial();
    let (intent, trial) = j.intent(&plan);
    let mut end = Journal::end_payload(&trial, Outcome::Pass, None);
    end.duration_s = plan.duration.get();
    end.tctl_max_c = Some(87);
    let peak = j.append(end.into(), &[intent]);
    j.cycle_end(1, true, true);
    let closed = project(&j);
    assert_eq!(closed.checking.cycle, NonZeroU32::new(1));
    assert_eq!(closed.checking.profile, Some(j.profile(&[-10])));
    assert_eq!(closed.checking.profile_seq, Some(profile_seq));
    assert_eq!(
        closed.checking.peak,
        Some(TemperaturePeak {
            celsius: 87,
            seq: peak,
        })
    );
    assert_eq!(closed.cycle_plan.number, NonZeroU32::new(1));
    assert!(!closed.cycle_plan.open);
    assert_eq!(closed.cycle_plan.current, closed.cycle_plan.steps.len());

    j.cycle_start(2, &[Regime::R2]);
    let open = project(&j);
    assert_eq!(open.checking.cycle, NonZeroU32::new(2));
    assert_eq!(open.checking.profile, closed.checking.profile);
    assert_eq!(open.checking.profile_seq, closed.checking.profile_seq);
    assert_eq!(open.checking.peak, closed.checking.peak);
    assert_eq!(open.cycle_plan.number, NonZeroU32::new(2));
    assert!(open.cycle_plan.open);
    assert_eq!(open.cycle_plan.current, 0);
    assert_eq!(open.cycle_plan.steps.len(), 1);
    assert_eq!(open.cycle_plan.steps[0].regime, Regime::R2);
    assert!(
        open.cycle_plan.steps[0]
            .parts
            .iter()
            .all(|part| part.passed == 0)
    );
    assert_eq!(closed.checking.cycle, NonZeroU32::new(1));
    assert!(!closed.cycle_plan.open);

    let changed = j.profile_change(&[-9]);
    let projection = project(&j);
    assert_eq!(projection.checking.profile, Some(j.profile(&[-9])));
    assert_eq!(projection.checking.profile_seq, Some(changed));
    assert_eq!(projection.checking.peak, None);
    assert_roundtrip(&open);
    assert_roundtrip(&projection);
}

fn round_fixture() -> (Journal, DeepeningView) {
    let mut j = Journal::new(&[1, 3, 5, 7], &[Start::has_room(-10); 4]);
    end_phase1(&mut j, &[-10; 4]);
    let target = j.profile(&[-20, -9, -10, -10]);
    let profile = j.profile(&[-15, -9, -10, -10]);
    let round = NonZeroU32::new(2).unwrap();
    let seq = j.append(
        DeepeningRound {
            round,
            event: RoundEvent::Start,
            base: j.profile(&[-10; 4]).offsets(),
            base_seq: j.run().phases.phase1_end(),
            target: target.offsets(),
            profile: profile.offsets(),
            cores: set(&[1, 3]),
            ranking: Vec::new(),
            trials: Some(2),
            trial_s: Some(120),
            passed: false,
            reason: None,
        }
        .into(),
        &[],
    );
    let check = |regime, workload, cores, condition, profile| DeepeningCheck {
        regime,
        workload,
        cores,
        condition,
        profile,
        needed: 2,
        passed: 0,
        failed: 0,
        running: false,
    };
    let mut checks = vec![
        check(
            Regime::R1,
            workloads(Regime::R1)[1].id(),
            set(&[1]),
            Condition::Alone,
            j.profile(&[-15, 0, 0, 0]),
        ),
        check(
            Regime::R2,
            workloads(Regime::R2)[1].id(),
            set(&[1]),
            Condition::Alone,
            j.profile(&[-15, 0, 0, 0]),
        ),
    ];
    checks.extend(workloads(Regime::R7).iter().map(|workload| {
        check(
            Regime::R7,
            workload.id(),
            set(&[1, 3, 5, 7]),
            Condition::Together,
            profile,
        )
    }));
    (
        j,
        DeepeningView {
            round,
            seq,
            target,
            profile,
            cores: set(&[1, 3]),
            checks,
        },
    )
}

fn assert_deepening(j: &Journal, want: &DeepeningView) {
    let projection = project(j);
    assert_eq!(projection.deepening.as_ref(), Some(want));
    assert_eq!(projection.phase_plan.as_ref().unwrap().round, Some(2));
    assert_roundtrip(&projection);
}

#[test]
fn active_deepening_projection() {
    for cancelled in [false, true] {
        let (mut j, mut want) = round_fixture();
        assert_deepening(&j, &want);
        let opened = project(&j);
        assert!(opened.cores.iter().all(|view| view.offset == off(-10)));
        while let Some(action) = j.run().round_move_action() {
            assert!(matches!(action.payload, DecisionPayload::TunerDecision(_)));
            j.decide(action);
            assert_deepening(&j, &want);
        }
        let moved = project(&j);
        assert_eq!(
            moved
                .cores
                .iter()
                .map(|view| view.offset)
                .collect::<Vec<_>>(),
            [off(-15), off(-9), off(-10), off(-10)]
        );
        assert_eq!(opened.cores[0].offset, off(-10));
        j.profile_change(&[-15, -9, -10, -10]);
        assert_deepening(&j, &want);
        for index in 0..want.checks.len() {
            for passed in 0..2 {
                let Some(Action::RunTrial(plan)) = j.run().round_check_action() else {
                    panic!("the round omitted check {index}");
                };
                let expected = &want.checks[index];
                assert_eq!(plan.regime, expected.regime);
                assert_eq!(plan.workload_at(j.rotation(&plan)), expected.workload);
                assert_eq!(plan.load.cores(), expected.cores);
                assert_eq!(plan.profile, expected.profile);
                assert_eq!(plan.condition, expected.condition);
                assert_eq!(plan.activity, Activity::Deepening);
                assert_eq!(plan.duration.get(), 120);
                let (intent, trial) = j.intent(&plan);
                want.checks[index].running = true;
                assert_deepening(&j, &want);
                assert_eq!(
                    project(&j).requirement,
                    Some(TrialRequirement {
                        passed,
                        failed: 0,
                        trial: passed + 1,
                        needed: 2,
                    })
                );
                let mut end = Journal::end_payload(&trial, Outcome::Pass, None);
                end.duration_s = 120;
                j.append(end.into(), &[intent]);
                want.checks[index].running = false;
                want.checks[index].passed += 1;
                assert_deepening(&j, &want);
                let projection = project(&j);
                assert_eq!(projection.requirement, None);
                assert_eq!(
                    projection.trial_history,
                    Some(TrialHistory {
                        passed,
                        trial: passed + 1,
                        needed: 2,
                        step: None,
                    })
                );
                if cancelled {
                    j.round_end(2, want.seq);
                    let closed = project(&j);
                    assert_eq!(closed.deepening, None);
                    assert_eq!(closed.phase_plan.unwrap().round, None);
                    assert_eq!(opened.deepening.as_ref().unwrap().checks[0].passed, 0);
                    break;
                }
            }
            if cancelled {
                break;
            }
        }
        if !cancelled {
            let Some(Action::Decide(end)) = j.run().round_check_action() else {
                panic!("a fully checked round did not end");
            };
            assert!(
                matches!(&end.payload, DecisionPayload::DeepeningRound(round)
                if round.event == RoundEvent::End && round.passed)
            );
            assert_eq!(end.cause, [want.seq]);
            j.decide(end);
            let closed = project(&j);
            assert_eq!(closed.deepening, None);
            assert_eq!(closed.phase_plan.unwrap().round, None);
            assert!(opened.deepening.is_some());
        }
    }
}

#[test]
fn projection_serde_roundtrip_and_owned_snapshot() {
    assert_roundtrip(&State::new().project());
    let mut j = Journal::new(&[1, 9], &[Start::solo(-10, -13), Start::solo(-8, -9)]);
    j.combination(1, &[(1, -12), (9, -9)]);
    end_phase1(&mut j, &[-10, -8]);
    j.cycle_start(2, &[Regime::R2]);
    let plan = j.next_trial();
    j.trial(&plan, Outcome::Pass);
    let plan = j.next_trial();
    j.intent(&plan);
    let saved = project(&j);
    assert!(saved.requirement.is_some());
    assert!(saved.trial_history.is_some());
    assert!(!saved.cycle_plan.steps[0].parts.is_empty());
    assert_eq!(saved.constraints.as_ref().unwrap().combinations.len(), 1);
    assert_roundtrip(&saved);
    let encoded = serde_json::to_vec(&saved).unwrap();

    let mut detached = saved.clone();
    detached.cores[0].offset = off(0);
    detached.constraints.as_mut().unwrap().combinations.clear();
    detached.cycle_plan.steps.clear();
    detached.phase_plan.as_mut().unwrap().candidates.clear();
    detached.bios_profile.as_mut().unwrap().unconfirmed.clear();
    assert_eq!(project(&j), saved);
    assert_ne!(detached, saved);

    j.combination(2, &[(1, -11), (9, -9)]);
    j.profile_change(&[-9, -8]);
    let latest = project(&j);
    assert_eq!(latest.constraints.unwrap().combinations.len(), 2);
    assert_eq!(latest.checking.profile, Some(j.profile(&[-9, -8])));
    assert_eq!(saved.checking.profile, Some(j.profile(&[-10, -8])));
    assert_eq!(serde_json::to_vec(&saved).unwrap(), encoded);
}

fn assert_trial_profile(j: &Journal, plan: &TrialPlan, seed: u64, step: usize) {
    assert_eq!(plan.profile.cores(), j.run().topo_ref().all());
    assert_eq!(
        j.state.constraints().unwrap().reaches(&plan.profile),
        Ok(None),
        "seed {seed}, step {step}: scheduled a known failing profile"
    );
    for (id, offset) in plan.profile.iter() {
        assert!((-50..=0).contains(&offset.get()));
        if plan.condition == Condition::Alone {
            let Load::Single(loaded) = plan.load else {
                panic!("seed {seed}, step {step}: alone trial loads multiple cores");
            };
            if id != loaded {
                assert_eq!(offset, off(0));
            }
        }
    }
}

#[test]
fn random_outcomes_preserve_profiles() {
    let topologies: [&[u8]; 3] = [&[1, 3, 5, 7], &[1, 4, 9, 14], &[0, 3, 6, 9, 12, 15]];
    let mut parked_failures = 0;
    let mut combinations_seen = 0;
    for seed in 1..=12 {
        let mut runner = TestRunner::new(ProptestConfig {
            rng_seed: RngSeed::Fixed(seed),
            ..seeded(1)
        });
        let script = proptest::collection::vec(0_u16..1000, 700)
            .new_tree(&mut runner)
            .unwrap()
            .current();
        let ids = topologies[usize::try_from(seed - 1).unwrap() % topologies.len()];
        let mut j = Journal::new(ids, &vec![Start::has_room(-30); ids.len()]);
        let mut started = 0;
        let mut ended = 0;
        let mut forced_together = false;
        let mut forced_parked = false;
        for (step, draw) in script.into_iter().enumerate() {
            if step >= 350 && started > 0 && started == ended && j.state.hunt_plan().is_none() {
                break;
            }
            let before = j.state.project();
            let old = before.constraints.as_ref().unwrap();
            let action = j.next();
            match action {
                Action::Decide(decision) => {
                    if matches!(decision.payload, DecisionPayload::DeadEnd(_)) {
                        break;
                    }
                    match &decision.payload {
                        DecisionPayload::ProfileChange(change) => {
                            let profile = Profile::new(j.run().topo_ref(), change.to).unwrap();
                            assert_eq!(
                                j.state.constraints().unwrap().reaches(&profile),
                                Ok(None),
                                "seed {seed}, step {step}: profile change reaches a constraint"
                            );
                        }
                        DecisionPayload::HuntStart(_) => started += 1,
                        DecisionPayload::HuntEnd(_) => ended += 1,
                        _ => {}
                    }
                    j.decide(decision);
                }
                Action::ReadRanking => {
                    j.ranking(ids);
                }
                Action::RunTrial(plan) => {
                    assert_trial_profile(&j, &plan, seed, step);
                    let force_together = !forced_together && plan.condition == Condition::Together;
                    let force_parked = !forced_parked && plan.condition == Condition::Parked;
                    forced_together |= force_together;
                    forced_parked |= force_parked;
                    let outcome = if step < 350 && (force_together || force_parked || draw < 35) {
                        Outcome::Failure
                    } else if step < 350 && draw < 95 {
                        Outcome::Inconclusive
                    } else {
                        Outcome::Pass
                    };
                    if outcome == Outcome::Failure && plan.condition == Condition::Parked {
                        parked_failures += 1;
                    }
                    let (intent, trial) = j.intent(&plan);
                    let signal = (outcome == Outcome::Failure).then_some(Signal::Crash);
                    let mut end = Journal::end_payload(&trial, outcome, signal);
                    end.duration_s = match outcome {
                        Outcome::Pass => plan.duration.get(),
                        Outcome::Failure => 10,
                        Outcome::Inconclusive => 0,
                    };
                    j.append(end.into(), &[intent]);
                }
            }
            let after = j.state.project();
            let new = after.constraints.as_ref().unwrap();
            for combination in &old.combinations {
                assert!(
                    new.combinations.contains(combination),
                    "seed {seed}, step {step}: combination disappeared without reset"
                );
            }
            for (id, point) in old.failure_points.iter() {
                let current = new.failure_points.get(id).unwrap();
                assert!(
                    !current.offset().deeper_than(point.offset()),
                    "seed {seed}, step {step}: failure point became deeper without reset"
                );
            }
            assert_eq!(
                after
                    .cores
                    .iter()
                    .map(|view| view.core)
                    .collect::<CoreSet>(),
                set(ids)
            );
            for view in &after.cores {
                assert!((-50..=0).contains(&view.offset.get()));
                assert_eq!(
                    view.failure_point,
                    new.failure_points
                        .get(view.core)
                        .map(crate::FailurePoint::offset)
                );
            }
            if let Some(hunt) = &after.hunt_plan {
                assert_eq!(hunt.number, j.run().hunts.active.as_ref().unwrap().id.get());
                assert!(hunt.candidates.iter().all(|id| set(ids).contains(id)));
                assert!(!hunt.candidates.is_empty());
                for group in &hunt.groups {
                    assert_eq!(
                        group.profile.iter().map(|(id, _)| id).collect::<CoreSet>(),
                        set(ids)
                    );
                    assert!(
                        group
                            .profile
                            .iter()
                            .all(|(_, offset)| (-50..=0).contains(&offset.get()))
                    );
                }
            }
            if step.is_multiple_of(70) {
                let snapshot = project(&j);
                assert_roundtrip(&snapshot);
            }
        }
        assert!(forced_together, "seed {seed}: no together workload");
        assert!(forced_parked, "seed {seed}: no parked workload");
        assert!(started > 0 && ended > 0, "seed {seed}: no complete hunt");
        assert_eq!(
            started, ended,
            "seed {seed}: an unfinished hunt after 700 steps"
        );
        assert!(j.state.hunt_plan().is_none());
        combinations_seen += j.state.project().constraints.unwrap().combinations.len();
        assert_roundtrip(&project(&j));
    }
    assert!(parked_failures > 0);
    assert!(combinations_seen > 0);
}
