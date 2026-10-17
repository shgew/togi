use std::collections::BTreeMap;
use std::num::NonZeroU32;

use jiff::Timestamp;
use togi_journal::{
    Activity, Attribution, DeadEnd, DeadEndCondition, FactSource, HuntStart, Outcome, Payload, Seq,
    TrialCarried, TrialClass, TrialId, TrialIntent,
};
use togi_machine::{BootId, Condition, CoreSet, PerCore, Regime, Signal, WorkloadId};

use crate::testkit::{Journal, R7Report, Start, build, pairs, set};
use crate::tests::{core, off};
use crate::zero::{Diagnostic, Zero, zero_requirement};
use crate::{
    Action, ClassKey, DecisionPayload, Load, Profile, RequirementKind, Secs, TrialContext,
    TrialHistory, TrialPlan, Window, WorkloadChoice,
};

const SESSION: &str = "20230101T000000Z";

fn workload(regime: Regime) -> WorkloadId {
    WorkloadId::new(regime, 0).unwrap()
}

fn duration(regime: Regime) -> u32 {
    if regime == Regime::R6 { 900 } else { 120 }
}

fn secs(duration: u32) -> Secs {
    Secs::new(NonZeroU32::new(duration).unwrap())
}

fn class(regime: Regime, cores: &[u8]) -> ClassKey {
    ClassKey::new(workload(regime), set(cores), secs(duration(regime)))
}

fn dead_end(action: &Action) -> (&DeadEnd, &[Seq]) {
    let Action::Decide(decision) = action else {
        panic!("expected a dead end, got {action:?}");
    };
    let DecisionPayload::DeadEnd(dead_end) = &decision.payload else {
        panic!("expected a dead end, got {:?}", decision.payload);
    };
    (dead_end, &decision.cause)
}

impl Journal {
    fn hunt_after_decisions(&mut self, ranking: &[u8]) -> HuntStart {
        for _ in 0..10 {
            match self.next() {
                Action::ReadRanking => {
                    self.ranking(ranking);
                }
                Action::Decide(decision) => {
                    if let DecisionPayload::HuntStart(start) = &decision.payload {
                        let start = start.clone();
                        self.decide(decision);
                        return start;
                    }
                    assert!(
                        !matches!(decision.payload, DecisionPayload::DeadEnd(_)),
                        "dead end before the hunt: {decision:?}"
                    );
                    self.decide(decision);
                }
                Action::RunTrial(plan) => panic!("trial {plan:?} before the hunt"),
            }
        }
        panic!("the failure was not hunted")
    }
}

impl Journal {
    fn run_named(
        &mut self,
        plan: &TrialPlan,
        outcome: Outcome,
        named: Option<u8>,
    ) -> (Seq, TrialId, Seq) {
        let (intent, trial) = self.intent(plan);
        let signal = (outcome == Outcome::Failure).then_some(Signal::ComputationError);
        let mut end = Self::end_payload(&trial, outcome, signal);
        end.core = named.map(core);
        let end = self.append(end.into(), &[intent]);
        (intent, trial, end)
    }

    fn failed(&mut self, plan: &TrialPlan, named: Option<u8>) -> Seq {
        self.run_named(plan, Outcome::Failure, named);
        let decision = self.next_decision();
        assert!(
            matches!(decision.payload, DecisionPayload::Failure(_)),
            "expected the attribution of the failed trial, got {decision:?}"
        );
        self.decide(decision)
    }

    fn zero_rerun(&self, failure: Seq) -> TrialPlan {
        let plan = self.next_trial();
        assert_eq!(
            plan.context,
            TrialContext::ZeroRerun { failure },
            "expected the all-zero rerun of #{failure}, got {plan:?}"
        );
        plan
    }

    fn intent_at(&self, seq: Seq) -> &TrialIntent {
        let index = usize::try_from(seq.get() - 1).unwrap();
        let payload = &self.events[index].payload;
        let Payload::TrialIntent(intent) = payload else {
            panic!("expected a trial intent, got {payload:?}");
        };
        intent
    }

    fn queued(&self, failure: Seq) -> bool {
        self.run().failures.queue().any(|seq| seq == failure)
    }

    fn diagnostic(&self, failure: Seq) -> Option<&Diagnostic> {
        self.run().zero.get(failure)
    }

    fn carry_trial_failure(
        &mut self,
        trial: &TrialId,
        regime: Regime,
        cores: &[u8],
        condition: Condition,
        profile: &[i64],
    ) -> Seq {
        let seq = self.next_seq();
        let payload = TrialCarried {
            source: FactSource {
                session: SESSION.parse().unwrap(),
                seq,
                build: build(),
                trial: Some(trial.clone()),
                time: Timestamp::from_second(1_600_000_000).unwrap(),
                boot: BootId::from("old".to_owned()),
                backend: None,
            },
            class: TrialClass {
                regime,
                workload: Some(workload(regime)),
                cores: set(cores),
                duration_s: duration(regime),
            },
            condition,
            activity: None,
            rerun: false,
            profile: self.profile(profile).offsets(),
            outcome: Outcome::Failure,
            signal: Some(Signal::Crash),
            duration_s: duration(regime),
            core: None,
            stalled_core: None,
            voltage_requests_v: PerCore::default(),
            top_requesters: CoreSet::default(),
            ccd_mhz: BTreeMap::new(),
        };
        self.append(payload.into(), &[])
    }
}

fn together(j: &Journal, regime: Regime, cores: &[u8], profile: &[i64]) -> TrialPlan {
    let load = match cores {
        [single] => Load::Single(core(*single)),
        _ => Load::Many(set(cores)),
    };
    j.manual(
        regime,
        0,
        load,
        Condition::Together,
        profile,
        duration(regime),
    )
}

fn expected_rerun(
    j: &Journal,
    regime: Regime,
    load: Load,
    activity: Activity,
    failure: Seq,
) -> TrialPlan {
    TrialPlan {
        regime,
        workload: WorkloadChoice::Frozen(workload(regime)),
        load,
        condition: Condition::Parked,
        activity,
        profile: Profile::zero(j.run().topo_ref()),
        duration: secs(duration(regime)),
        context: TrialContext::ZeroRerun { failure },
        requirement: zero_requirement(
            ClassKey::new(workload(regime), load.cores(), secs(duration(regime))),
            failure,
        ),
        retry: false,
        cause: vec![failure],
    }
}

#[test]
fn known_failure_sharing_a_zero_rerun_trial_is_routed() {
    let mut j = Journal::has_room(&[0, -20]);
    let plan = together(&j, Regime::R6, &[0, 1], &[0, -20]);
    let failure = j.failed(&plan, Some(0));
    let rerun = j.zero_rerun(failure);
    let (_, id, end) = j.run_named(&rerun, Outcome::Pass, None);
    assert_eq!(j.diagnostic(failure), Some(&Diagnostic::Passed { end }));
    let fact = j.carry_trial_failure(&id, Regime::R6, &[0, 1], Condition::Together, &[0, -19]);
    let Action::Decide(skip) = j.run().skip_known_failure(Action::RunTrial(plan)) else {
        panic!("the known failure did not skip the trial");
    };
    let DecisionPayload::Failure(skipped) = &skip.payload else {
        panic!("expected a known-failure skip, got {skip:?}");
    };
    assert_eq!(skipped.known_failure, Some(fact));
    assert_eq!(skipped.trial.as_ref(), Some(&id));
    assert_eq!(skipped.core, Some(core(1)));
    j.decide(skip);
    assert_eq!(j.run().core(core(1)).pending, Some(fact));
    assert_eq!(j.run().core(core(0)).pending, None);
    assert_eq!(j.diagnostic(failure), Some(&Diagnostic::Passed { end }));
    let backoff = j.next_decision();
    let DecisionPayload::TunerDecision(backoff) = &backoff.payload else {
        panic!("the skipped known failure did not back off core 01: {backoff:?}");
    };
    assert_eq!(backoff.core, core(1));
    assert_eq!(backoff.decision, togi_journal::Decision::Backoff);
    assert_eq!(backoff.from_offset, off(-20));
    assert_eq!(backoff.to_offset, off(-18));
    assert_eq!(backoff.failure_point, Some(off(-19)));
    j.assert_resumes();
}

#[test]
fn single_core_zero_rerun_keeps_its_shape() {
    let mut j = Journal::has_room(&[0, -10]);
    let plan = together(&j, Regime::R5, &[0], &[0, -10]);
    let failure = j.failed(&plan, Some(0));
    let record = &j.run().failures.pending(failure).unwrap().failure;
    assert_eq!((record.core, record.offset), (Some(core(0)), Some(off(0))));
    let rerun = j.zero_rerun(failure);
    assert_eq!(
        rerun,
        expected_rerun(
            &j,
            Regime::R5,
            Load::Single(core(0)),
            Activity::Checking,
            failure
        )
    );
    let (intent, _, _) = j.run_named(&rerun, Outcome::Pass, None);
    let intent = j.intent_at(intent);
    assert_eq!(intent.core, Some(core(0)));
    assert!(intent.cores.is_empty());
    assert!(intent.rerun);
    assert_eq!(intent.condition, Condition::Parked);
    assert_eq!(
        j.run().failures.pending(failure).unwrap().class,
        class(Regime::R5, &[0])
    );
    assert!(j.queued(failure));
    assert_eq!(j.run().core(core(0)).pending, None);
    let start = j.hunt_after_decisions(&[0, 1]);
    assert_eq!(start.failure, failure);
    assert_eq!(start.candidates, set(&[1]));
    j.assert_resumes();
}

#[test]
fn direct_failure_at_zero() {
    struct Case {
        name: &'static str,
        starts: [Start; 2],
        profile: [i64; 2],
        trial: Option<(Regime, Condition)>,
        named: Option<u8>,
        rerun: bool,
    }
    let cases = [
        Case {
            name: "alone search",
            starts: [Start::search(0), Start::has_room(0)],
            profile: [0, 0],
            trial: Some((Regime::R1, Condition::Alone)),
            named: Some(0),
            rerun: false,
        },
        Case {
            name: "attributed together",
            starts: [Start::has_room(0), Start::has_room(-10)],
            profile: [0, -10],
            trial: Some((Regime::R6, Condition::Together)),
            named: Some(0),
            rerun: true,
        },
        Case {
            name: "attributed all zero",
            starts: [Start::has_room(0), Start::has_room(0)],
            profile: [0, 0],
            trial: Some((Regime::R6, Condition::Together)),
            named: Some(0),
            rerun: false,
        },
        Case {
            name: "unattributed together",
            starts: [Start::has_room(0), Start::has_room(0)],
            profile: [0, 0],
            trial: Some((Regime::R6, Condition::Together)),
            named: None,
            rerun: false,
        },
        Case {
            name: "unattributed idle",
            starts: [Start::has_room(0), Start::has_room(0)],
            profile: [0, 0],
            trial: None,
            named: None,
            rerun: false,
        },
    ];
    for case in cases {
        let name = case.name;
        let mut j = Journal::new(&[0, 1], &case.starts);
        let failure = if let Some((regime, condition)) = case.trial {
            let load = if condition == Condition::Alone {
                Load::Single(core(0))
            } else {
                Load::Many(set(&[0, 1]))
            };
            let plan = if condition == Condition::Alone {
                j.next_trial()
            } else {
                j.manual(regime, 0, load, condition, &case.profile, duration(regime))
            };
            let failure = j.failed(&plan, case.named);
            let record = &j.run().failures.pending(failure).unwrap().failure;
            assert_eq!(record.core, case.named.map(core), "{name}");
            failure
        } else {
            let profile = j.profile(&case.profile).offsets();
            j.idle_failure(profile, Condition::Together)
        };
        let mut cause = vec![failure];
        if case.rerun {
            let rerun = j.zero_rerun(failure);
            assert_eq!(
                rerun,
                expected_rerun(
                    &j,
                    Regime::R6,
                    Load::Many(set(&[0, 1])),
                    Activity::Checking,
                    failure
                ),
                "{name}"
            );
            let (_, _, end) = j.run_named(&rerun, Outcome::Failure, None);
            let attribution = j.next_decision();
            j.decide(attribution);
            assert_eq!(
                j.diagnostic(failure),
                Some(&Diagnostic::Failed { end }),
                "{name}"
            );
            cause.push(end);
        }
        let action = j.next();
        let (stop, stop_cause) = dead_end(&action);
        assert_eq!(stop.condition, DeadEndCondition::FailureAtZero, "{name}");
        assert_eq!(stop.core, case.named.map(core), "{name}");
        assert_eq!(stop_cause, cause.as_slice(), "{name}");
        let Action::Decide(decision) = action.clone() else {
            unreachable!()
        };
        let checking = j.run().checking.profile();
        let offsets = j.offsets();
        j.decide(decision);
        j.assert_resumes();
        for _ in 0..if case.named.is_some() { 3 } else { 0 } {
            let action = j.next();
            let (stop, _) = dead_end(&action);
            assert_eq!(stop.condition, DeadEndCondition::FailureAtZero, "{name}");
            assert_eq!(j.run().checking.profile(), checking, "{name}");
            assert_eq!(j.offsets(), offsets, "{name}");
        }
    }
}

#[test]
fn zero_rerun_requirement_is_one_trial() {
    let mut j = Journal::has_room(&[0, -10]);
    let plan = together(&j, Regime::R5, &[0], &[0, -10]);
    j.run_named(&plan, Outcome::Pass, None);
    j.run_named(&plan, Outcome::Pass, None);
    let failure = j.failed(&plan, Some(0));
    let rerun = j.zero_rerun(failure);
    let (intent, trial) = j.intent(&rerun);
    assert!(j.intent_at(intent).rerun);
    let stored = rerun.requirement;
    assert_eq!(stored.kind, RequirementKind::ZeroRerun);
    assert_eq!(
        stored.window,
        Window::LiveOrCarried {
            since: Some(failure)
        }
    );
    assert_eq!(stored.needed.get(), 1);
    assert_eq!(j.requirement().needed, 1);
    j.end(intent, &trial, Outcome::Pass);
    assert_eq!(
        j.history(),
        TrialHistory {
            passed: 0,
            trial: 1,
            needed: 1,
            step: None,
        }
    );
}

#[test]
fn all_zero_rerun_retry_belongs_to_its_failure() {
    let mut j = Journal::has_room(&[0, -10, 0, -10]);
    let profile = [0, -10, 0, -10];
    let mut failures = BTreeMap::new();
    for id in [0, 2] {
        let plan = together(&j, Regime::R5, &[id], &profile);
        failures.insert(id, j.failed(&plan, Some(id)));
    }
    let first = j.zero_rerun(failures[&0]);
    assert_eq!(first.load, Load::Single(core(0)));
    assert!(!first.retry);
    j.run_named(&first, Outcome::Inconclusive, None);
    let again = j.zero_rerun(failures[&0]);
    assert!(again.retry);
    assert_eq!(again.load, Load::Single(core(0)));
    assert_eq!(again.requirement, first.requirement);
    assert_eq!(again.cause, vec![failures[&0]]);

    j.reset(0);
    assert!(j.run().retry.is_none());
    assert_eq!(j.diagnostic(failures[&0]), None);
    let mut next = j.next();
    for _ in 0..10 {
        let Action::Decide(decision) = next else {
            break;
        };
        j.decide(decision);
        next = j.next();
    }
    let Action::RunTrial(rerun) = next else {
        panic!("expected the other failure's rerun, got {next:?}");
    };
    assert_eq!(
        rerun,
        expected_rerun(
            &j,
            Regime::R5,
            Load::Single(core(2)),
            Activity::Checking,
            failures[&2]
        )
    );
    j.assert_resumes();
}

fn zero_carry(profile: [i64; 4], failure_point: bool) -> (Journal, Seq) {
    let ids = [0, 1, 2, 3];
    let mut starts = profile.map(Start::has_room);
    if failure_point {
        starts[1] = starts[1].fail(0);
    }
    let mut j = Journal::on(&ids, pairs(&ids), &starts);
    let fact = j.carry(
        SESSION,
        (Regime::R6, 0, &ids, duration(Regime::R6)),
        Condition::Parked,
        &profile,
        Outcome::Failure,
    );
    (j, fact)
}

#[test]
fn carried_unconfirmed_failure_at_zero_is_hunted() {
    let profile = [0, 0, -20, -25];
    let (mut j, fact) = zero_carry(profile, false);
    assert_eq!(j.run().failure_point(core(1)), None);
    let plan = together(&j, Regime::R6, &[0, 1, 2, 3], &profile);
    let Action::Decide(skip) = j.run().skip_known_failure(Action::RunTrial(plan)) else {
        panic!("the carried failure no longer constrains its class");
    };
    let DecisionPayload::Failure(skipped) = &skip.payload else {
        panic!("expected a known-failure skip, got {skip:?}");
    };
    assert_eq!(skipped.known_failure, Some(fact));
    assert_eq!(skipped.attribution, Attribution::Unattributed);
    j.decide(skip);
    assert!(j.queued(fact));
    let start = j.hunt_after_decisions(&[0, 1, 2, 3]);
    assert_eq!(start.failure, fact);
    assert_eq!(start.candidates, set(&[2, 3]));
    j.assert_resumes();
}

#[test]
fn carried_failure_at_zero_with_all_zero_profile_dead_ends() {
    let (j, _) = zero_carry([0, 0, 0, 0], true);
    let action = j.next();
    let (stop, _) = dead_end(&action);
    assert_eq!(stop.condition, DeadEndCondition::FailureAtZero);
    assert_eq!(stop.core, Some(core(1)));
}

#[test]
fn zero_diagnostic_ignores_known_failure_skips_and_stale_trials() {
    let mut j = Journal::has_room(&[0, -20]);
    let plan = together(&j, Regime::R6, &[0, 1], &[0, -20]);
    let failure = j.failed(&plan, Some(0));
    let rerun = j.zero_rerun(failure);
    let (_, id, _) = j.run_named(&rerun, Outcome::Inconclusive, None);
    let running = Diagnostic::Running { trial: id.clone() };
    assert_eq!(j.diagnostic(failure), Some(&running));
    let fact = j.carry_trial_failure(&id, Regime::R6, &[0, 1], Condition::Together, &[0, -19]);
    let Action::Decide(skip) = j.run().skip_known_failure(Action::RunTrial(plan)) else {
        panic!("the known failure did not skip the trial");
    };
    let DecisionPayload::Failure(skipped) = &skip.payload else {
        panic!("expected a known-failure skip, got {skip:?}");
    };
    assert_eq!(
        (skipped.known_failure, skipped.trial.as_ref()),
        (Some(fact), Some(&id))
    );
    j.decide(skip);
    assert_eq!(j.diagnostic(failure), Some(&running));
    j.zero_rerun(failure);
    j.assert_resumes();

    let (first_intent, first) = j.intent(&rerun);
    j.end(first_intent, &first, Outcome::Inconclusive);
    let (_, second) = j.intent(&rerun);
    assert_eq!(
        j.diagnostic(failure),
        Some(&Diagnostic::Running {
            trial: second.clone()
        })
    );
    let late = Journal::end_payload(&first, Outcome::Failure, Some(Signal::ComputationError));
    assert!(j.try_append(late.into(), &[first_intent]).is_err());
    assert_eq!(
        j.diagnostic(failure),
        Some(&Diagnostic::Running {
            trial: second.clone()
        })
    );

    let mut zero = Zero::default();
    let seq = |n| Seq::new(n).unwrap();
    let (old, new) = (
        TrialId::from("old".to_owned()),
        TrialId::from("new".to_owned()),
    );
    zero.run(seq(5), old.clone());
    zero.run(seq(5), new.clone());
    assert_eq!(zero.end(&old, seq(9), Outcome::Failure), None);
    assert_eq!(
        zero.get(seq(5)),
        Some(&Diagnostic::Running { trial: new.clone() })
    );
    assert_eq!(zero.end(&new, seq(10), Outcome::Pass), Some(seq(5)));
    assert_eq!(zero.end(&old, seq(11), Outcome::Failure), None);
    assert_eq!(zero.get(seq(5)), Some(&Diagnostic::Passed { end: seq(10) }));
}

#[test]
fn zero_diagnostic_lifecycle() {
    for outcome in [Outcome::Pass, Outcome::Failure] {
        let mut j = Journal::has_room(&[0, -20]);
        let plan = together(&j, Regime::R6, &[0, 1], &[0, -20]);
        let failure = j.failed(&plan, Some(0));
        let rerun = j.zero_rerun(failure);
        assert_eq!(j.diagnostic(failure), None);

        let (intent, trial) = j.intent(&rerun);
        let running = Diagnostic::Running {
            trial: trial.clone(),
        };
        assert_eq!(j.diagnostic(failure), Some(&running));
        j.end(intent, &trial, Outcome::Inconclusive);
        assert_eq!(j.diagnostic(failure), Some(&running));

        let retry = j.zero_rerun(failure);
        assert!(retry.retry);
        assert_eq!(retry.requirement, rerun.requirement);
        assert_eq!(
            (retry.load, retry.profile, retry.condition),
            (rerun.load, rerun.profile, rerun.condition)
        );
        assert_eq!(retry.cause, vec![failure]);

        let (intent, trial) = j.intent(&retry);
        assert_eq!(
            j.diagnostic(failure),
            Some(&Diagnostic::Running {
                trial: trial.clone()
            })
        );
        let end = j.end(intent, &trial, outcome);
        let want = match outcome {
            Outcome::Pass => Diagnostic::Passed { end },
            Outcome::Failure => Diagnostic::Failed { end },
            Outcome::Inconclusive => unreachable!("the lifecycle ends only in a pass or a failure"),
        };
        assert_eq!(j.diagnostic(failure), Some(&want), "{outcome:?}");
        j.assert_resumes();
    }
}

#[test]
fn zero_release_after_a_passed_rerun_outside_multi_r7() {
    type Case = (
        &'static str,
        Regime,
        &'static [u8],
        Condition,
        Activity,
        bool,
    );
    let cases: [Case; 2] = [
        (
            "together",
            Regime::R6,
            &[0, 1],
            Condition::Together,
            Activity::Checking,
            true,
        ),
        (
            "single core",
            Regime::R5,
            &[0],
            Condition::Together,
            Activity::Checking,
            true,
        ),
    ];
    for (name, regime, cores, condition, activity, queued) in cases {
        let mut j = Journal::has_room(&[0, -20]);
        let mut plan = together(&j, regime, cores, &[0, -20]);
        plan.condition = condition;
        let failure = j.failed(&plan, Some(0));
        assert_eq!(j.run().core(core(0)).pending, Some(failure), "{name}");
        let rerun = j.zero_rerun(failure);
        assert_eq!(rerun.activity, activity, "{name}");
        let (_, _, end) = j.run_named(&rerun, Outcome::Pass, None);
        assert_eq!(
            j.diagnostic(failure),
            Some(&Diagnostic::Passed { end }),
            "{name}"
        );
        assert_eq!(j.run().core(core(0)).pending, None, "{name}");
        assert_eq!(j.queued(failure), queued, "{name}");
        j.assert_resumes();
    }

    let ids = [0, 1, 2, 3];
    let starts = [
        Start::has_room(0),
        Start::has_room(-30),
        Start::has_room(-30),
        Start::has_room(-30),
    ];
    let mut j = Journal::on(&ids, pairs(&ids), &starts);
    j.profile_change(&[0, -30, -30, -30]);
    j.ranking(&ids);
    let failure = j.r7_failed(
        &ids,
        &[0, -30, -30, -30],
        &R7Report::default().named(0).top(&[0]),
    );
    let rerun = j.zero_rerun(failure);
    let (_, _, end) = j.run_named(&rerun, Outcome::Pass, None);
    assert_eq!(j.diagnostic(failure), Some(&Diagnostic::Passed { end }));
    assert!(!j.queued(failure));
    j.assert_resumes();
}

#[test]
fn a_passed_zero_diagnostic_releases_the_original_obligation() {
    let mut j = Journal::has_room(&[0, -20]);
    let plan = together(&j, Regime::R6, &[0, 1], &[0, -20]);
    let failure = j.failed(&plan, Some(0));
    let record = j.run().failures.pending(failure).unwrap().clone();
    let rerun = j.zero_rerun(failure);
    j.run_named(&rerun, Outcome::Pass, None);
    assert_eq!(
        j.run()
            .at_zero(failure, &record, core(0), "failure", vec![failure]),
        crate::zero::AtZero::Released
    );
    assert!(j.run().pending_core_action().is_none());
    assert!(j.queued(failure));
    j.assert_resumes();
}

#[test]
fn a_single_core_zero_rerun_rejects_equivalent_many_and_ambiguous_load_shapes() {
    let mut j = Journal::has_room(&[0, -20]);
    let plan = together(&j, Regime::R5, &[0], &[0, -20]);
    let failure = j.failed(&plan, Some(0));
    let rerun = j.zero_rerun(failure);
    let trial = TrialId::from("zero-shape".to_owned());
    let valid = rerun.intent(trial.clone(), 0);
    let class = j.run().class_of_intent(&valid).unwrap();
    assert_eq!(
        j.run().zero_context(&valid, class, &[failure]),
        Some(rerun.requirement)
    );
    for core in [None, Some(core(0))] {
        let intent = TrialIntent {
            core,
            cores: set(&[0]),
            ..valid.clone()
        };
        let event = j.event(intent.into(), &[failure]);
        let mut probe = j.state.clone();
        assert!(matches!(
            probe.fold(&event),
            Err(crate::FoldError::UnscheduledIntent(found)) if found == trial
        ));
        assert_eq!(probe.run(), j.state.run());
    }
    j.assert_resumes();
}

#[test]
fn an_unrelated_checking_retry_does_not_replace_a_zero_diagnostic() {
    let mut j = Journal::has_room(&[0, -20]);
    j.ranking(&[0, 1]);
    j.profile_change(&[0, -20]);
    j.open_default_cycle();
    let cycle = j.next_trial();
    j.run_named(&cycle, Outcome::Inconclusive, None);
    assert!(matches!(
        j.run().retry.as_ref().unwrap().plan.context,
        TrialContext::Cycle { .. }
    ));
    let plan = together(&j, Regime::R6, &[0, 1], &[0, -20]);
    let failure = j.failed(&plan, Some(0));
    let rerun = j.zero_rerun(failure);
    assert_eq!(rerun.context, TrialContext::ZeroRerun { failure });
    assert!(!rerun.retry);
    assert_eq!(rerun.load, plan.load);
    assert_eq!(rerun.duration, plan.duration);
    assert!(rerun.profile.is_zero());
    assert_eq!(rerun.cause, [failure]);
    j.assert_resumes();
}

#[test]
fn a_passed_rerun_of_a_failure_decided_meanwhile_releases_and_queues_nothing() {
    let mut j = Journal::has_room(&[0, -10, 0, -10]);
    let profile = [0, -10, 0, -10];
    let plan = together(&j, Regime::R5, &[0], &profile);
    let failure = j.failed(&plan, Some(0));
    let rerun = j.zero_rerun(failure);
    let (intent, trial) = j.intent(&rerun);
    j.append(
        togi_journal::TunerDecision {
            core: core(0),
            activity: Activity::Checking,
            decision: togi_journal::Decision::Backoff,
            from_offset: off(0),
            to_offset: off(0),
            pass: None,
            failure_point: Some(off(0)),
            workloads: Vec::new(),
            reason: "test".to_owned(),
        }
        .into(),
        &[failure],
    );
    assert!(j.run().failures.pending(failure).is_none());
    j.end(intent, &trial, Outcome::Pass);
    assert!(j.run().failures.queue_is_empty());
    assert_eq!(j.run().core(core(0)).pending, None);
    j.assert_resumes();
}
