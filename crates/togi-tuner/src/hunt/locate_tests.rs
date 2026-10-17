use togi_journal::{
    Activity, Attribution, CorePhase, CoreTransition, DeadEnd, DeadEndCondition, Decision, HuntEnd,
    HuntGroup, HuntOutcome, HuntStage, HuntStart, Member, Outcome, Seq, TunerDecision,
};
use togi_machine::{Condition, Regime, Signal, workloads};

use crate::r7::escalation::ESCALATE_AFTER;
use crate::r7::respond::R7Route;
use crate::testkit::{Journal, R7Report, Start, config, fresh, set};
use crate::tests::{core, off};
use crate::{
    Action, ClassKey, DecisionAction, DecisionPayload, Load, Profile, Requirement, Secs,
    TrialContext, TrialPlan, Window, WorkloadChoice,
};

fn values(profile: Profile) -> Vec<i64> {
    profile.iter().map(|(_, offset)| offset.get()).collect()
}

fn at(plan: &TrialPlan, id: u8) -> i64 {
    plan.profile.get(core(id)).unwrap().get()
}

fn top0() -> R7Report {
    R7Report::default().top(&[0])
}

struct Located {
    action: Action,
    failures: Vec<Seq>,
}

fn decision(action: &Action) -> &DecisionAction {
    let Action::Decide(decision) = action else {
        panic!("expected a decision, got {action:?}");
    };
    decision
}

fn hunt_end(action: &Action) -> &HuntEnd {
    let payload = &decision(action).payload;
    let DecisionPayload::HuntEnd(end) = payload else {
        panic!("expected a hunt end, got {payload:?}");
    };
    end
}

fn hunt_group(action: &Action) -> &HuntGroup {
    let payload = &decision(action).payload;
    let DecisionPayload::HuntGroup(group) = payload else {
        panic!("expected a hunt group, got {payload:?}");
    };
    group
}

fn hunt_start(action: &Action) -> &HuntStart {
    let payload = &decision(action).payload;
    let DecisionPayload::HuntStart(start) = payload else {
        panic!("expected a hunt start, got {payload:?}");
    };
    start
}

fn backoff(action: &Action) -> &TunerDecision {
    let payload = &decision(action).payload;
    if let DecisionPayload::TunerDecision(move_) = payload
        && move_.decision == Decision::Backoff
    {
        return move_;
    }
    panic!("expected a backoff, got {payload:?}");
}

fn dead_end(action: &Action) -> &DeadEnd {
    let payload = &decision(action).payload;
    let DecisionPayload::DeadEnd(dead) = payload else {
        panic!("expected a dead end, got {payload:?}");
    };
    dead
}

fn trial(action: &Action) -> &TrialPlan {
    let Action::RunTrial(plan) = action else {
        panic!("expected a trial, got {action:?}");
    };
    plan
}

fn missing(text: &str, tokens: &[String]) -> Vec<String> {
    tokens
        .iter()
        .filter(|token| !text.contains(token.as_str()))
        .cloned()
        .collect()
}

#[derive(Clone, Copy)]
enum Verdict {
    Pass,
    Unnamed,
    Named(u8),
}

fn joint_failure(duration: u32) -> impl FnMut(&TrialPlan) -> Verdict {
    move |plan| {
        if plan.duration.get() == duration && at(plan, 2) != 0 && at(plan, 3) != 0 {
            Verdict::Unnamed
        } else {
            Verdict::Pass
        }
    }
}

fn never(_: &TrialPlan) -> Verdict {
    Verdict::Pass
}

fn fails_as(failed: bool, verdict: Verdict) -> Verdict {
    if failed { verdict } else { Verdict::Pass }
}

fn full_group(duration: u32) -> impl FnMut(&HuntGroup) -> bool {
    move |group| group.stage == HuntStage::Full && group.duration_s == duration
}

impl Journal {
    fn loc_ids(&self) -> Vec<u8> {
        self.run()
            .topo_ref()
            .order()
            .iter()
            .copied()
            .map(togi_machine::CoreId::get)
            .collect()
    }

    fn loc_poll(&mut self) -> Option<Action> {
        for _ in 0..8 {
            match self.state.next().ok()? {
                Action::ReadRanking => {
                    let ids = self.loc_ids();
                    self.ranking(&ids);
                }
                other @ (Action::Decide(_) | Action::RunTrial(_)) => return Some(other),
            }
        }
        panic!("stuck reading the ranking");
    }

    fn loc_act(&mut self) -> Action {
        self.loc_poll().expect("the tuner has a next action")
    }

    fn loc_checking(&self) -> Vec<i64> {
        values(self.run().checking.profile().expect("a checking profile"))
    }

    fn loc_set_core(&mut self, id: u8, offset: i64) -> Seq {
        self.core_phase(CoreTransition {
            core: core(id),
            from: None,
            to: CorePhase::HasRoom,
            offset: off(offset),
            pass: None,
            failure_point: None,
            check_solo_limit: false,
            workloads: Vec::new(),
            cleared_combination: Vec::new(),
            reason: "test".to_owned(),
        })
    }

    fn loc_finish(&mut self, plan: &TrialPlan, outcome: Outcome, report: &R7Report) -> Seq {
        let (intent, trial) = self.intent(plan);
        let signal = (outcome == Outcome::Failure).then_some(Signal::ComputationError);
        let mut end = Self::end_payload(&trial, outcome, signal);
        end.duration_s = if outcome == Outcome::Pass {
            plan.duration.get()
        } else {
            41
        };
        end.core = report.named.map(core);
        end.top_requesters = set(&report.top);
        self.append(end.into(), &[intent])
    }

    fn loc_fail_live(&mut self, report: &R7Report) -> (Seq, Seq) {
        let profile = self.loc_checking();
        let end = self.r7_trial(&[0, 1], &profile, Outcome::Failure, report);
        let attribution = self.next_decision();
        let DecisionPayload::Failure(failure) = &attribution.payload else {
            panic!("expected the attribution, got {attribution:?}");
        };
        assert_eq!(
            failure.attribution == Attribution::Unattributed,
            report.named.is_none(),
            "{failure:?}"
        );
        (end, self.decide(attribution))
    }

    fn settle_phases(&mut self) -> Action {
        loop {
            let action = self.loc_act();
            match &action {
                Action::Decide(decision)
                    if matches!(decision.payload, DecisionPayload::CorePhase(_)) =>
                {
                    self.decide(decision.clone());
                }
                Action::Decide(_) | Action::RunTrial(_) | Action::ReadRanking => return action,
            }
        }
    }

    fn loc_back_off(&mut self, report: &R7Report) -> Vec<Seq> {
        let offsets = values(self.offsets());
        let profile = self.loc_checking();
        let prior = R7Report {
            named: None,
            ..report.clone()
        };
        let mut backoffs = Vec::new();
        for _ in 0..ESCALATE_AFTER {
            self.loc_fail_live(&prior);
            let action = self.settle_phases();
            backoff(&action);
            backoffs.push(self.decide(decision(&action).clone()));
            for (id, (&now, &was)) in (0u8..).zip(values(self.offsets()).iter().zip(&offsets)) {
                if now != was {
                    self.loc_set_core(id, was);
                }
            }
            self.profile_change(&profile);
        }
        backoffs
    }

    fn loc_fail_escalated(&mut self, report: &R7Report) -> (Seq, Seq, Vec<Seq>) {
        let backoffs = self.loc_back_off(report);
        let (end, failure) = self.loc_fail_live(report);
        (end, failure, backoffs)
    }

    fn loc_run_until(
        &mut self,
        mut fails: impl FnMut(&TrialPlan) -> Verdict,
        mut stop: impl FnMut(&HuntGroup) -> bool,
    ) -> Located {
        let mut failures = Vec::new();
        for _ in 0..200 {
            let action = self.loc_act();
            match &action {
                Action::Decide(decision) => {
                    if let DecisionPayload::HuntGroup(group) = &decision.payload {
                        if stop(group) {
                            return Located { action, failures };
                        }
                    } else if !matches!(decision.payload, DecisionPayload::HuntStart(_)) {
                        return Located { action, failures };
                    }
                    self.decide(decision.clone());
                }
                Action::RunTrial(plan) if plan.condition == Condition::Parked => {
                    match fails(plan) {
                        Verdict::Pass => {
                            self.loc_finish(plan, Outcome::Pass, &R7Report::default());
                        }
                        failed @ (Verdict::Unnamed | Verdict::Named(_)) => {
                            let report = R7Report {
                                named: if let Verdict::Named(core) = failed {
                                    Some(core)
                                } else {
                                    None
                                },
                                ..R7Report::default()
                            };
                            failures.push(self.loc_finish(plan, Outcome::Failure, &report));
                            let attribution = self.next_decision();
                            self.decide(attribution);
                        }
                    }
                }
                Action::RunTrial(_) | Action::ReadRanking => return Located { action, failures },
            }
        }
        panic!("the located hunt did not end");
    }

    fn loc_run(&mut self, fails: impl FnMut(&TrialPlan) -> Verdict) -> Located {
        self.loc_run_until(fails, |_| false)
    }

    fn loc_end_cycle_for_reset(&mut self, id: u8) {
        let Some(Action::Decide(decision)) = self.state.next().ok() else {
            return;
        };
        let DecisionPayload::CheckingCycle(cycle) = &decision.payload else {
            return;
        };
        assert!(!cycle.passed, "{cycle:?}");
        assert_eq!(
            cycle.reason.as_deref(),
            Some(format!("core {id:02} was reset").as_str())
        );
        self.decide(decision);
    }

    fn loc_drain_without_moves(&mut self, name: &str) -> Option<TrialPlan> {
        loop {
            match self.loc_poll()? {
                Action::Decide(decision) => {
                    assert!(
                        !matches!(decision.payload, DecisionPayload::TunerDecision(_)),
                        "{name}: the located failure moved another core: {decision:?}"
                    );
                    self.decide(decision);
                }
                Action::RunTrial(plan) => return Some(plan),
                Action::ReadRanking => unreachable!(),
            }
        }
    }

    fn loc_assert_loaded_kept(&mut self) {
        while let Some(Action::Decide(decision)) = self.loc_poll() {
            if let DecisionPayload::TunerDecision(move_) = &decision.payload {
                assert!(
                    move_.core != core(0) && move_.core != core(1),
                    "a loaded core moved: {move_:?}"
                );
            }
            self.decide(decision.clone());
        }
        assert_eq!(values(self.offsets())[..2], [-30, -30]);
    }

    fn loc_assert_resumed_next(&self) {
        assert_eq!(self.resumed().state.next().ok(), self.state.next().ok());
        self.assert_resumes();
    }
}

fn assert_full_failing_group(action: &Action, duration: u32) {
    let group = hunt_group(action);
    assert_eq!(group.stage, HuntStage::Full, "{group:?}");
    assert_eq!(group.duration_s, duration, "{group:?}");
    assert_eq!(group.cores, set(&[2, 3]), "{group:?}");
    assert!(
        group.profile.iter().all(|(_, offset)| offset.get() == -30),
        "{group:?}"
    );
}

#[test]
fn located_hunt_retests_the_full_failing_profile_before_ending_loaded() {
    let mut j = Journal::r7_session();
    j.loc_fail_escalated(&top0());
    let located = j.loc_run_until(joint_failure(120), full_group(120));
    assert_full_failing_group(&located.action, 120);
    assert_eq!(j.run().hunt_groups().len(), 3);
    let located = j.loc_run(joint_failure(120));
    let end = hunt_end(&located.action);
    assert_eq!(end.result, HuntOutcome::Combination, "{end:?}");
    assert_eq!(end.cores, set(&[2, 3]));
    j.decide(decision(&located.action).clone());
    j.loc_assert_loaded_kept();
    j.assert_resumes();

    let mut j = Journal::r7_session();
    j.loc_fail_escalated(&top0());
    let located = j.loc_run_until(never, full_group(120));
    assert_full_failing_group(&located.action, 120);
    j.decide(decision(&located.action).clone());
    let first = j.next_trial();
    j.loc_finish(&first, Outcome::Pass, &R7Report::default());
    let plan = j.next_trial();
    assert_eq!(
        plan.requirement.context,
        TrialContext::Hunt {
            hunt: std::num::NonZeroU32::MIN,
            group: std::num::NonZeroU32::new(4).unwrap(),
        }
    );
    assert_eq!(plan.duration.get(), 120);
    assert_eq!(values(plan.profile), [-30, -30, -30, -30]);
    j.loc_assert_resumed_next();
    let full = &j.resumed().run().hunt_groups()[3];
    assert_eq!((full.passed, full.outcome), (1, None));
    let located = j.loc_run(never);
    let end = hunt_end(&located.action).clone();
    assert_eq!(end.result, HuntOutcome::Loaded, "{end:?}");
    assert_eq!((end.cores, end.groups), (set(&[0, 1]), 4));
    let finished = j.state.hunt_plan().expect("the hunt is still active");
    assert!(finished.parts.is_empty() && finished.probes.is_empty());
    assert_eq!(finished.cut, 0);
    let ended = j.decide(decision(&located.action).clone());
    let action = j.loc_act();
    let move_ = backoff(&action);
    assert_eq!((move_.core, move_.to_offset), (core(0), off(-29)));
    assert!(decision(&action).cause.contains(&ended));
    j.assert_resumes();
}

#[test]
fn escalated_located_hunt_retests_the_full_failing_profile_at_the_failed_duration() {
    type Fails = fn(&TrialPlan) -> Verdict;
    let cases: [(&str, Fails, HuntOutcome, &[u8]); 2] = [
        ("full profile passes", never, HuntOutcome::Loaded, &[0, 1]),
        (
            "full profile fails",
            |plan| joint_failure(120)(plan),
            HuntOutcome::Combination,
            &[2, 3],
        ),
    ];
    for (name, fails, result, cores) in cases {
        let mut j = Journal::r7_session();
        j.load(&config(
            r#"{"durations":{"short_trial_s":60,"checking_all_core_s":480}}"#,
        ));
        j.loc_fail_escalated(&top0());
        let located = j.loc_run_until(fails, full_group(120));
        assert_full_failing_group(&located.action, 120);
        let groups = &j.run().hunts.active.as_ref().unwrap().groups;
        let stages: Vec<HuntStage> = groups.iter().map(|group| group.payload.stage).collect();
        assert_eq!(
            stages,
            [
                HuntStage::Locate,
                HuntStage::Part,
                HuntStage::Part,
                HuntStage::Full,
                HuntStage::Part,
                HuntStage::Part,
            ],
            "{name}"
        );
        for group in groups {
            if group.payload.stage == HuntStage::Full {
                assert_eq!(group.payload.duration_s, 60, "{name}");
            }
        }
        j.decide(decision(&located.action).clone());
        let located = j.loc_run(fails);
        let end = hunt_end(&located.action);
        assert_eq!((end.result, end.cores), (result, set(cores)), "{name}");
        j.decide(decision(&located.action).clone());
        if result != HuntOutcome::Loaded {
            j.loc_assert_loaded_kept();
        }
        j.assert_resumes();
    }
}

#[test]
fn an_already_broken_combination_retires_the_located_hunt_entry() {
    let mut j = Journal::r7_session();
    let (_, failure, _) = j.loc_fail_escalated(&top0());
    let located = j.loc_run(joint_failure(120));
    let end = hunt_end(&located.action);
    assert_eq!(end.result, HuntOutcome::Combination, "{end:?}");
    j.decide(decision(&located.action).clone());
    assert!(j.run().hunts.is_located(failure));
    j.profile_change(&[-30, -30, 0, -30]);
    let action = j.loc_act();
    let DecisionPayload::Combination(combination) = &decision(&action).payload else {
        panic!("expected the combination, got {action:?}");
    };
    assert!(combination.reason.contains("no backoff"), "{combination:?}");
    j.decide(decision(&action).clone());
    assert!(j.run().hunts.active.is_none());
    assert!(!j.run().hunts.is_located(failure));
    j.assert_resumes();
}

#[test]
fn reset_of_a_candidate_cancels_the_located_hunt() {
    let mut j = Journal::r7_session();
    let (_, failure, _) = j.loc_fail_escalated(&top0());
    for _ in 0..2 {
        let action = j.loc_act();
        j.decide(decision(&action).clone());
    }
    let locate = j.next_trial();
    j.loc_finish(&locate, Outcome::Pass, &R7Report::default());
    j.reset(2);
    let action = j.loc_act();
    let end = hunt_end(&action);
    assert_eq!(
        (end.result, end.hunt.get()),
        (HuntOutcome::Cancelled, 1),
        "{end:?}"
    );
    j.decide(decision(&action).clone());
    assert!(j.run().hunts.active.is_none());
    assert!(!j.run().hunts.located.contains_key(&failure));
    j.loc_end_cycle_for_reset(2);
    let action = j.loc_act();
    let DecisionPayload::CorePhase(phase) = &decision(&action).payload else {
        panic!("expected the reset, got {action:?}");
    };
    assert_eq!((phase.core, phase.to), (core(2), CorePhase::Search));
    j.decide(decision(&action).clone());
    assert!(matches!(
        j.run().locate_due(),
        Some(R7Route::Locate { failure: due, .. }) if due == failure
    ));
    j.loc_assert_resumed_next();
    j.core_phase(CoreTransition {
        core: core(2),
        from: Some(CorePhase::Search),
        to: CorePhase::HasRoom,
        offset: off(-30),
        pass: None,
        failure_point: None,
        check_solo_limit: false,
        workloads: Vec::new(),
        cleared_combination: Vec::new(),
        reason: "test".to_owned(),
    });
    for _ in 0..10 {
        let action = j.loc_act();
        let decision = decision(&action).clone();
        if let DecisionPayload::HuntStart(start) = &decision.payload {
            assert_eq!(start.hunt.get(), 2, "{start:?}");
            assert_eq!(start.failure, failure);
            assert_eq!(start.candidates, set(&[2, 3]));
            j.assert_resumes();
            return;
        }
        if let DecisionPayload::TunerDecision(move_) = &decision.payload {
            panic!("the cancelled located failure moved a core: {move_:?}")
        }
        j.decide(decision);
    }
    panic!("the failure was not located again");
}

#[test]
fn r7_escalated_failure_locates_with_idle_cores_at_zero() {
    let mut j = Journal::r7_session();
    let (end, failure, backoffs) = j.loc_fail_escalated(&top0());
    assert_eq!(backoffs.len(), ESCALATE_AFTER as usize);
    let action = j.loc_act();
    let start = hunt_start(&action).clone();
    assert_eq!(decision(&action).cause, [failure, backoffs[0], backoffs[1]]);
    let failed_trial = j
        .events
        .iter()
        .find_map(|event| {
            if let togi_journal::Payload::TrialEnd(ended) = &event.payload
                && Some(event.seq) == Some(end)
            {
                Some(ended.trial.clone())
            } else {
                None
            }
        })
        .unwrap();
    let r7 = workloads(Regime::R7)[0].id();
    assert_eq!(start.hunt.get(), 1);
    assert_eq!(start.failure, failure);
    assert_eq!(start.trial, Some(failed_trial));
    assert_eq!((start.regime, start.workload), (Regime::R7, r7));
    assert_eq!(start.cores, set(&[0, 1]));
    assert_eq!((start.duration_s, start.trial_s), (120, 120));
    assert_eq!(start.failing, j.profile(&[-30, -30, -30, -30]).offsets());
    assert_eq!(start.parked, j.profile(&[-30, -30, 0, 0]).offsets());
    assert_eq!(start.parked_seq, None);
    assert_eq!(start.candidates, set(&[2, 3]));
    assert_eq!(start.trials, u32::from(j.run().config.needed.get()));
    assert_eq!(start.ranking, [core(0), core(1), core(2), core(3)].to_vec());
    let reason = start.reason.clone().unwrap();
    assert!(
        missing(
            &reason,
            &[
                "no core is named".to_owned(),
                "located on the unloaded cores".to_owned(),
                format!("backed off {ESCALATE_AFTER} times"),
                format!("#{}, #{}", backoffs[0], backoffs[1]),
            ]
        )
        .is_empty(),
        "{reason}"
    );
    j.decide(decision(&action).clone());
    let action = j.loc_act();
    let group = hunt_group(&action);
    assert_eq!(group.stage, HuntStage::Locate, "{group:?}");
    assert!(group.cores.is_empty());
    assert_eq!(group.duration_s, 120);
    assert_eq!(group.profile, j.profile(&[-30, -30, 0, 0]).offsets());
    let group_seq = j.decide(decision(&action).clone());
    let class = ClassKey::new(
        r7,
        set(&[0, 1]),
        Secs::new(std::num::NonZeroU32::new(120).unwrap()),
    );
    let want = TrialPlan {
        regime: Regime::R7,
        workload: WorkloadChoice::Frozen(r7),
        load: Load::Many(set(&[0, 1])),
        condition: Condition::Parked,
        activity: Activity::Hunt,
        profile: j.profile(&[-30, -30, 0, 0]),
        duration: class.duration(),
        requirement: Requirement {
            context: TrialContext::Hunt {
                hunt: std::num::NonZeroU32::MIN,
                group: std::num::NonZeroU32::MIN,
            },
            class,
            window: Window::LiveOrCarried {
                since: Some(group_seq),
            },
            needed: j.run().config.needed,
        },
        retry: false,
        cause: vec![group_seq],
    };
    assert_eq!(j.next_trial(), want);
    assert_eq!(j.run().r7_decision(), None);
    j.loc_assert_resumed_next();
}

#[test]
fn a_locate_reads_a_ranking_newer_than_the_last_round_plan() {
    let mut j = Journal::r7_session();
    let (_, failure, _) = j.loc_fail_escalated(&top0());
    assert!(!j.run().ranking_stale());
    let profile = values(j.offsets());
    let round = j.round_start(1, &profile, &[0], 1);
    j.round_end(1, round);
    assert_eq!(j.run().locate_action(), Some(Action::ReadRanking));
    let ids = j.loc_ids();
    j.ranking(&ids);
    let action = j.run().locate_action().expect("the failure is located");
    assert_eq!(hunt_start(&action).failure, failure);
    j.assert_resumes();
}

#[test]
fn a_located_group_failure_naming_a_loaded_core_is_not_held() {
    use crate::later_tests::{pass_cycles, start_cycle, strikes};
    let mut j = Journal::r7_session();
    j.loc_back_off(&top0());
    pass_cycles(&mut j, 3);
    start_cycle(&mut j);
    assert!(
        j.run().later_gate().is_some(),
        "the later-failure gate is closed"
    );
    j.loc_fail_live(&top0());
    let located = j.loc_run(|plan| {
        if at(plan, 2) == 0 {
            Verdict::Pass
        } else {
            Verdict::Named(0)
        }
    });
    let end = hunt_end(&located.action);
    assert_eq!((end.result, end.cores), (HuntOutcome::Loaded, set(&[0, 1])));
    j.decide(decision(&located.action).clone());
    let action = j.loc_act();
    let step = backoff(&action);
    assert_eq!(
        (step.core, step.to_offset, step.failure_point),
        (core(0), off(-29), Some(off(-30))),
        "{step:?}"
    );
    j.decide(decision(&action).clone());
    assert!(
        !matches!(
            j.state.next().ok(),
            Some(Action::Decide(DecisionAction {
                payload: DecisionPayload::TunerDecision(_),
                ..
            }))
        ),
        "the failure moved twice"
    );
    assert_eq!(strikes(&j), 0);
    j.assert_resumes();
}

#[test]
fn a_locate_ends_the_open_round_first() {
    let mut j = Journal::r7_session();
    let (_, failure, _) = j.loc_fail_escalated(&top0());
    let profile = values(j.offsets());
    j.round_start(1, &profile, &[0], 1);
    let action = j.run().locate_action().expect("the failure is located");
    let DecisionPayload::DeepeningRound(round) = &decision(&action).payload else {
        panic!("expected the round to end, got {action:?}");
    };
    assert_eq!(
        (round.round.get(), round.event, round.passed),
        (1, togi_journal::RoundEvent::End, false)
    );
    assert_eq!(decision(&action).cause, [failure]);
    assert_eq!(
        round.reason.as_deref(),
        Some(format!("R7 failure #{failure} needs a located hunt").as_str())
    );
    j.decide(decision(&action).clone());
    assert!(j.run().rounds.active().is_none());
    assert_eq!(j.run().locate_action(), Some(Action::ReadRanking));
    j.assert_resumes();
}

#[test]
fn r7_passed_locate_hunts_the_unloaded_culprit() {
    let mut j = Journal::r7_session();
    let (_, failure, _) = j.loc_fail_escalated(&top0());
    let located = j.loc_run(|plan| fails_as(at(plan, 2) != 0, Verdict::Unnamed));
    let end = hunt_end(&located.action);
    assert_eq!((end.result, end.cores), (HuntOutcome::Culprit, set(&[2])));
    j.decide(decision(&located.action).clone());
    let action = j.loc_act();
    let move_ = backoff(&action);
    assert_eq!(move_.activity, Activity::Hunt);
    assert_eq!((move_.core, move_.to_offset), (core(2), off(0)));
    j.decide(decision(&action).clone());
    let rerun = j
        .loc_drain_without_moves("culprit")
        .expect("the failed load is rerun");
    assert!(matches!(
        rerun.requirement.context,
        TrialContext::Rerun { .. }
    ));
    assert_eq!(rerun.condition, Condition::Together);
    assert_eq!(rerun.load, Load::Many(set(&[0, 1])));
    assert_eq!(rerun.duration.get(), 120);
    assert_eq!(rerun.cause, [failure]);
    assert_eq!(values(j.offsets()), [-30, -30, 0, -30]);
    j.assert_resumes();
}

#[test]
fn r7_failed_locate_keeps_the_voltage_targeted_backoff() {
    let mut j = Journal::r7_session();
    let passes = R7Report::default()
        .requests(&[(0, 1.115), (1, 1.08)])
        .top(&[0]);
    for _ in 0..j.run().config.needed.get() {
        j.r7_fact(true, &[0, 1], &[-26, -30, -30, -30], &passes);
    }
    let (_, failure, _) = j.loc_fail_escalated(&top0().requests(&[(0, 1.1), (1, 1.08)]));
    let located = j.loc_run(|_| Verdict::Unnamed);
    let locate = *located.failures.last().unwrap();
    let end = hunt_end(&located.action);
    assert_eq!((end.result, end.cores), (HuntOutcome::Loaded, set(&[0, 1])));
    let ended = j.decide(decision(&located.action).clone());
    let action = j.loc_act();
    let move_ = backoff(&action).clone();
    assert_eq!((move_.core, move_.to_offset), (core(0), off(-25)));
    for seq in [failure, ended, locate] {
        assert!(
            decision(&action).cause.contains(&seq),
            "{:?} omits #{seq}",
            decision(&action).cause
        );
    }
    let gaps = missing(
        &move_.reason,
        &[
            "hunt 1".to_owned(),
            format!("#{locate}"),
            "loaded cores".to_owned(),
            "unloaded core".to_owned(),
            "CO 0".to_owned(),
        ],
    );
    assert!(gaps.is_empty(), "{} lacks {gaps:?}", move_.reason);
    j.decide(decision(&action).clone());
    assert_eq!(j.run().r7_decision(), None);
    j.assert_resumes();
}

#[test]
fn r7_located_group_failure_naming_a_core() {
    let cases: [(&str, u8, HuntOutcome, &[u8]); 3] = [
        ("loaded core", 0, HuntOutcome::Loaded, &[0, 1]),
        (
            "unloaded core at its failing offset",
            2,
            HuntOutcome::Direct,
            &[2],
        ),
        ("unloaded core parked at 0", 3, HuntOutcome::Culprit, &[2]),
    ];
    for (name, named, result, cores) in cases {
        let mut j = Journal::r7_session();
        j.loc_fail_escalated(&top0());
        let located = j.loc_run(|plan| fails_as(at(plan, 2) != 0, Verdict::Named(named)));
        let end = hunt_end(&located.action);
        assert_eq!((end.result, end.cores), (result, set(cores)), "{name}");
        let direct = decision(&located.action).cause[0];
        let ended = j.decide(decision(&located.action).clone());
        if result == HuntOutcome::Direct {
            let action = j.loc_act();
            let move_ = backoff(&action);
            assert_eq!(move_.activity, Activity::Hunt, "{name}");
            assert_eq!(
                (move_.core, move_.from_offset, move_.to_offset),
                (core(2), off(-30), off(-29)),
                "{name}"
            );
            assert_eq!(decision(&action).cause, [ended], "{name}");
            j.decide(decision(&action).clone());
            assert!(j.run().hunts.active.is_none(), "{name}");
            assert_eq!(j.run().locate_due(), None, "{name}");
            let rerun = j
                .loc_drain_without_moves(name)
                .unwrap_or_else(|| panic!("{name}: no rerun"));
            assert!(
                matches!(rerun.requirement.context, TrialContext::Rerun { .. }),
                "{name}"
            );
            assert_eq!(rerun.cause, [direct], "{name}");
            assert_eq!(rerun.load, Load::Many(set(&[0, 1])), "{name}");
            assert_eq!(rerun.condition, Condition::Together, "{name}");
            j.assert_resumes();
            continue;
        }
        let action = j.loc_act();
        let move_ = backoff(&action);
        assert_eq!(
            (move_.core, move_.to_offset),
            (
                core(cores[0]),
                off(if result == HuntOutcome::Culprit {
                    0
                } else {
                    -29
                }),
            ),
            "{name}"
        );
        j.decide(decision(&action).clone());
        assert_eq!(j.run().r7_decision(), None, "{name}");
        j.assert_resumes();
    }
}

#[test]
fn r7_zero_loaded_ccd_dead_ends_only_after_failed_locate() {
    for locate_fails in [true, false] {
        let mut j = Journal::r7_session();
        j.loc_set_core(0, 0);
        j.loc_set_core(1, 0);
        j.profile_change(&[0, 0, -30, -30]);
        let (_, failure) = j.loc_fail_live(&top0());
        assert_eq!(j.run().r7_decision(), None);
        let located =
            j.loc_run(|plan| fails_as(locate_fails || at(plan, 3) != 0, Verdict::Unnamed));
        let end = hunt_end(&located.action);
        if !locate_fails {
            assert_eq!((end.result, end.cores), (HuntOutcome::Culprit, set(&[3])));
            continue;
        }
        assert_eq!(end.result, HuntOutcome::Loaded, "{end:?}");
        let locate = *located.failures.last().unwrap();
        j.decide(decision(&located.action).clone());
        let action = j.loc_act();
        let dead = dead_end(&action);
        assert_eq!(dead.condition, DeadEndCondition::FailureAtZero);
        let cause = &decision(&action).cause;
        assert!(
            cause.contains(&failure) && cause.contains(&locate),
            "{cause:?}"
        );
        let gaps = missing(
            &dead.detail,
            &["CO 0".to_owned(), "hunt 1".to_owned(), format!("#{locate}")],
        );
        assert!(gaps.is_empty(), "{} lacks {gaps:?}", dead.detail);
        j.assert_resumes();
    }
}

#[test]
fn r7_located_failure_naming_a_loaded_core_charges_that_core() {
    type Fails = fn(&TrialPlan) -> bool;
    let cases: [(&str, Fails, u32); 2] = [
        ("locate", |_| true, 1),
        (
            "narrowing group",
            |plan| at(plan, 2) != 0 || at(plan, 3) != 0,
            2,
        ),
    ];
    for (name, fails, groups) in cases {
        let mut j = Journal::r7_session();
        let (_, failure, _) = j.loc_fail_escalated(&top0());
        let located = j.loc_run(|plan| fails_as(fails(plan), Verdict::Named(1)));
        let named = *located.failures.last().unwrap();
        let end = hunt_end(&located.action);
        assert_eq!(
            (end.result, end.cores, end.groups),
            (HuntOutcome::Loaded, set(&[0, 1]), groups),
            "{name}"
        );
        let ended = j.decide(decision(&located.action).clone());
        let action = j.loc_act();
        let move_ = backoff(&action).clone();
        assert_eq!(
            (
                move_.core,
                move_.from_offset,
                move_.to_offset,
                move_.failure_point
            ),
            (core(1), off(-30), off(-29), Some(off(-30))),
            "{name}"
        );
        let cause = &decision(&action).cause;
        assert_eq!(cause[0], failure, "{name}");
        assert!(
            cause.contains(&ended) && cause.contains(&named),
            "{name}: {cause:?}"
        );
        let gaps = missing(
            &move_.reason,
            &[
                "hunt 1".to_owned(),
                "loaded core 01".to_owned(),
                format!("#{named}"),
                format!("#{failure}"),
            ],
        );
        assert!(gaps.is_empty(), "{name}: {} lacks {gaps:?}", move_.reason);
        j.decide(decision(&action).clone());
        assert_eq!(j.run().r7_decision(), None, "{name}");
        if let Some(next) = j.loc_drain_without_moves(name) {
            assert!(at(&next, 1) > -30, "{name}: {next:?}");
        }
        assert_eq!(values(j.offsets()), [-30, -29, -30, -30], "{name}");
        let constraints = &j.run().constraints;
        assert!(
            constraints.reaches(&j.offsets()).unwrap().is_none(),
            "{name}"
        );
        assert!(
            constraints
                .reaches(&j.profile(&[-30, -30, -30, -30]))
                .unwrap()
                .is_some(),
            "{name}"
        );
        j.assert_resumes();
    }
}

#[test]
fn r7_passed_all_zero_locate_never_charges_loaded_cores_at_zero() {
    type Fails = fn(&TrialPlan) -> Verdict;
    let cases: [(&str, Fails, HuntOutcome, &[u8]); 2] = [
        ("no group fails", never, HuntOutcome::Fallback, &[2, 3]),
        (
            "a group names a loaded core at 0",
            |plan| fails_as(at(plan, 3) != 0, Verdict::Named(0)),
            HuntOutcome::Culprit,
            &[3],
        ),
    ];
    for (name, fails, result, cores) in cases {
        let mut j = Journal::r7_session();
        j.loc_set_core(0, 0);
        j.loc_set_core(1, 0);
        j.profile_change(&[0, 0, -30, -30]);
        j.loc_fail_live(&top0());
        let located = j.loc_run(fails);
        let end = hunt_end(&located.action);
        assert_eq!((end.result, end.cores), (result, set(cores)), "{name}");
        j.decide(decision(&located.action).clone());
        while let Some(Action::Decide(decision)) = j.loc_poll() {
            assert!(
                !matches!(decision.payload, DecisionPayload::DeadEnd(_)),
                "{name}: dead-ended after the all-zero locate passed: {decision:?}"
            );
            j.decide(decision.clone());
        }
        assert_eq!(values(j.offsets())[..2], [0, 0], "{name}");
        j.assert_resumes();
    }
}

#[test]
fn r7_locate_failure_naming_a_loaded_core_at_zero() {
    let cases: [(&str, u8, bool); 3] = [
        ("top requester, rerun passes", 0, false),
        ("top requester, rerun fails", 0, true),
        ("not a top requester", 1, false),
    ];
    for (name, top, rerun_fails) in cases {
        let mut j = Journal::r7_session();
        j.loc_set_core(0, 0);
        j.profile_change(&[0, -30, -30, -30]);
        let report = R7Report::default().top(&[top]);
        let (_, failure, _) = j.loc_fail_escalated(&report);
        let locate = loop {
            let action = j.loc_act();
            match &action {
                Action::Decide(decision)
                    if matches!(
                        decision.payload,
                        DecisionPayload::HuntStart(_) | DecisionPayload::HuntGroup(_)
                    ) =>
                {
                    j.decide(decision.clone());
                }
                Action::Decide(_) | Action::RunTrial(_) | Action::ReadRanking => {
                    break trial(&action).clone();
                }
            }
        };
        assert_eq!(
            locate.requirement.context,
            TrialContext::Hunt {
                hunt: std::num::NonZeroU32::MIN,
                group: std::num::NonZeroU32::MIN,
            },
            "{name}"
        );
        assert_eq!(values(locate.profile), [0, -30, 0, 0], "{name}");
        j.loc_finish(&locate, Outcome::Failure, &report.clone().named(0));
        let attribution = j.next_decision();
        let locate = j.decide(attribution);
        let action = j.loc_act();
        let end = hunt_end(&action);
        assert_eq!(end.result, HuntOutcome::Loaded, "{name}");
        assert!(
            end.reason.contains("named loaded core 00"),
            "{name}: {end:?}"
        );
        j.decide(decision(&action).clone());
        let action = j.loc_act();
        if top != 0 {
            let move_ = backoff(&action).clone();
            assert_eq!((move_.core, move_.to_offset), (core(1), off(-29)), "{name}");
            let cause = &decision(&action).cause;
            assert_eq!(cause[0], failure, "{name}");
            assert!(cause.contains(&locate), "{name}: {cause:?}");
            let gaps = missing(
                &move_.reason,
                &[
                    "core 00".to_owned(),
                    "CO 0".to_owned(),
                    "top requester".to_owned(),
                    "CCD 0".to_owned(),
                ],
            );
            assert!(gaps.is_empty(), "{name}: {} lacks {gaps:?}", move_.reason);
            j.decide(decision(&action).clone());
            assert_eq!(j.run().r7_decision(), None, "{name}");
            j.assert_resumes();
            continue;
        }
        let rerun = trial(&action).clone();
        assert!(
            matches!(rerun.requirement.context, TrialContext::ZeroRerun { .. }),
            "{name}: {rerun:?}"
        );
        assert_eq!(rerun.condition, Condition::Parked, "{name}");
        assert_eq!(values(rerun.profile), [0, 0, 0, 0], "{name}");
        assert_eq!(rerun.load, Load::Many(set(&[0, 1])), "{name}");
        assert_eq!(rerun.cause, [locate], "{name}");
        if rerun_fails {
            let end = j.loc_finish(&rerun, Outcome::Failure, &R7Report::default());
            let attribution = j.next_decision();
            j.decide(attribution);
            let action = j.loc_act();
            let dead = dead_end(&action);
            assert_eq!(dead.condition, DeadEndCondition::FailureAtZero, "{name}");
            let cause = &decision(&action).cause;
            assert!(
                cause.contains(&locate) && cause.contains(&end),
                "{name}: {cause:?}"
            );
            assert!(
                dead.detail.contains(&format!("#{end}")),
                "{name}: {}",
                dead.detail
            );
            j.assert_resumes();
            continue;
        }
        let end = j.loc_finish(&rerun, Outcome::Pass, &R7Report::default());
        let action = j.loc_act();
        let move_ = backoff(&action);
        assert_eq!((move_.core, move_.to_offset), (core(1), off(-29)), "{name}");
        for seq in [failure, locate, end] {
            assert!(
                decision(&action).cause.contains(&seq),
                "{name}: {:?} omits #{seq}",
                decision(&action).cause
            );
        }
        j.decide(decision(&action).clone());
        assert_eq!(j.run().r7_decision(), None, "{name}");
        j.assert_resumes();
    }
}

#[test]
fn r7_passed_zero_rerun_locates_citing_the_rerun() {
    let mut j = Journal::r7_session();
    j.loc_set_core(0, 0);
    j.profile_change(&[0, -30, -30, -30]);
    let (_, failure, _) = j.loc_fail_escalated(&top0().named(0));
    let rerun = j.next_trial();
    assert!(
        matches!(rerun.requirement.context, TrialContext::ZeroRerun { .. }),
        "{rerun:?}"
    );
    assert_eq!(rerun.condition, Condition::Parked);
    assert_eq!(values(rerun.profile), [0, 0, 0, 0]);
    assert_eq!(rerun.load, Load::Many(set(&[0, 1])));
    let rerun = j.loc_finish(&rerun, Outcome::Pass, &R7Report::default());
    let action = j.loc_act();
    let start = hunt_start(&action);
    assert_eq!(start.failure, failure);
    assert_eq!(start.candidates, set(&[2, 3]));
    assert_eq!(decision(&action).cause[..2], [failure, rerun]);
    let reason = start.reason.clone().unwrap();
    let gaps = missing(
        &reason,
        &["CO 0".to_owned(), "passed".to_owned(), format!("#{rerun}")],
    );
    assert!(gaps.is_empty(), "{reason} lacks {gaps:?}");
    j.decide(decision(&action).clone());
    j.assert_resumes();
}

const SESSION: &str = "20261002T004254Z";

fn loc_limit_session(count: u8, offset: i64) -> Journal {
    let ids: Vec<u8> = (0..count).collect();
    let starts = vec![Start::at_limit(offset).fail(offset - 1); usize::from(count)];
    let mut j = Journal::new(&ids, &starts);
    j.load(&loc_config(None));
    j.profile_change(&vec![offset; usize::from(count)]);
    j.ranking(&ids);
    j
}

fn loc_config(backends: Option<(&str, &str)>) -> togi_config::Config {
    let durations = r#""durations":{"checking_idle_s":120}"#;
    match backends {
        None => config(&format!("{{{durations}}}")),
        Some((mprime, ycruncher)) => config(&format!(
            r#"{{{durations},"backends":{{"mprime":"{mprime}","ycruncher":"{ycruncher}"}}}}"#
        )),
    }
}

fn loc_failed_hunt(count: u8, duration: u32) -> Journal {
    let mut j = loc_limit_session(count, -30);
    let ids: Vec<u8> = (0..count).collect();
    let plan = j.manual(
        Regime::R6,
        0,
        Load::Many(set(&ids)),
        Condition::Together,
        &vec![-30; usize::from(count)],
        duration,
    );
    j.trial(&plan, Outcome::Failure);
    let attribution = j.next_decision();
    j.decide(attribution);
    j.start_hunt();
    j
}

fn loc_first_hunt_trial(j: &mut Journal) -> TrialPlan {
    for _ in 0..10 {
        match j.next() {
            Action::RunTrial(plan) => return plan,
            Action::Decide(decision) => {
                j.decide(decision);
            }
            Action::ReadRanking => unreachable!(),
        }
    }
    panic!("the hunt ran no trial");
}

fn loc_hand_start(j: &mut Journal, failure: Seq, failing: &[i64], duration: u32) -> Seq {
    let ids: Vec<u8> = (0..u8::try_from(failing.len()).unwrap()).collect();
    let start = HuntStart {
        hunt: std::num::NonZeroU32::MIN,
        failure,
        trial: None,
        regime: Regime::R6,
        workload: workloads(Regime::R6)[0].id(),
        cores: set(&ids),
        duration_s: duration,
        failing: j.profile(failing).offsets(),
        parked: j.profile(&vec![0; failing.len()]).offsets(),
        parked_seq: None,
        candidates: set(&ids),
        trials: u32::from(j.run().config.needed.get()),
        trial_s: j.run().config.short_trial.get(),
        miss: j.run().config.miss,
        rate: j.run().config.rate,
        ranking: ids.iter().map(|&id| core(id)).collect(),
        reason: None,
    };
    j.append(start.into(), &[failure])
}

#[test]
fn reset_after_hunt_end_drops_commitment() {
    let mut j = super::support::hunt_session(&[-29, -30]);
    j.together_failure(Regime::R6, 0, &[-29, -30]);
    let (start, _) = j.start_hunt();
    let end = HuntEnd {
        hunt: std::num::NonZeroU32::MIN,
        result: HuntOutcome::Culprit,
        cores: set(&[0]),
        members: Vec::new(),
        groups: 2,
        reason: "test".to_owned(),
    };
    j.append(end.into(), &[start]);
    assert!(matches!(
        j.next(),
        Action::Decide(DecisionAction {
            payload: DecisionPayload::TunerDecision(_),
            ..
        })
    ));
    j.reset(0);
    j.loc_end_cycle_for_reset(0);
    let action = j.next();
    let DecisionPayload::CorePhase(phase) = &decision(&action).payload else {
        panic!("expected the reset, got {action:?}");
    };
    assert_eq!((phase.core, phase.to), (core(0), CorePhase::Search));
    j.decide(decision(&action).clone());
    assert!(j.run().hunts.active.is_none());
    if let Ok(Action::Decide(next)) = j.state.next() {
        assert!(
            !matches!(&next.payload, DecisionPayload::TunerDecision(td) if td.activity == Activity::Hunt),
            "the reset core received a hunt commitment: {next:?}"
        );
        assert!(
            !matches!(next.payload, DecisionPayload::Combination(_)),
            "the reset core received a combination: {next:?}"
        );
    }
    j.assert_resumes();
}

#[test]
fn hunt_group_retry_belongs_to_its_group() {
    let mut j = loc_failed_hunt(4, 120);
    let first = loc_first_hunt_trial(&mut j);
    assert!(
        matches!(first.requirement.context, TrialContext::Hunt { .. }),
        "{first:?}"
    );
    j.trial(&first, Outcome::Inconclusive);
    let again = j.next_trial();
    assert!(again.retry, "{again:?}");
    assert_eq!(again.requirement.context, first.requirement.context);
    assert_eq!(fresh(&j.events).next().ok(), Some(Action::RunTrial(again)));
    j.reset(0);
    for _ in 0..10 {
        match j.state.next().ok() {
            Some(Action::RunTrial(plan)) => {
                assert!(!plan.retry, "a reset left the hunt retry: {plan:?}");
                break;
            }
            Some(Action::Decide(decision)) => {
                j.decide(decision);
            }
            Some(Action::ReadRanking) | None => break,
        }
    }
    assert!(j.run().retry.is_none());
    j.assert_resumes();
}

fn loc_retry_session(prior: Option<(Vec<i64>, u32)>) -> Journal {
    let backend = workloads(Regime::R6)[0].backend;
    let old = match backend {
        togi_machine::Backend::Mprime => crate::testkit::MPRIME_OLD,
        togi_machine::Backend::Ycruncher => crate::testkit::YCRUNCHER_OLD,
    };
    let mut j = loc_limit_session(4, -30);
    if let Some((profile, duration)) = prior {
        j.load(&loc_config(Some((
            crate::testkit::MPRIME_OLD,
            crate::testkit::YCRUNCHER_OLD,
        ))));
        for _ in 0..j.run().config.needed.get() {
            j.carry_under(
                SESSION,
                (Regime::R6, 0, &[0, 1, 2, 3], duration),
                Condition::Parked,
                &profile,
                Outcome::Pass,
                Some((backend, old)),
            );
        }
    }
    j.load(&loc_config(Some((
        crate::testkit::MPRIME_NEW,
        crate::testkit::YCRUNCHER_NEW,
    ))));
    let plan = j.manual(
        Regime::R6,
        0,
        Load::Many(set(&[0, 1, 2, 3])),
        Condition::Together,
        &[-30; 4],
        120,
    );
    j.trial(&plan, Outcome::Failure);
    let attribution = j.next_decision();
    j.decide(attribution);
    j.start_hunt();
    j
}

fn loc_group(j: &Journal, number: u32) -> HuntGroup {
    let (group, _) = j.group_plan();
    assert_eq!(group.group.get(), number, "{group:?}");
    assert!(
        matches!(group.stage, HuntStage::Part | HuntStage::Complement)
            && group.inferred.is_none()
            && !group.skipped,
        "group {number} should run as a part or complement: {group:?}"
    );
    group
}

fn loc_values(group: &HuntGroup) -> Vec<i64> {
    (0..4)
        .map(|id| group.profile.get(core(id)).unwrap().get())
        .collect()
}

#[test]
fn hunt_retry_stays_with_its_group() {
    let unrestored = loc_group(&loc_retry_session(None), 1);
    let (p1, duration) = (loc_values(&unrestored), unrestored.duration_s);

    let mut j = loc_retry_session(Some((p1.clone(), duration)));
    let first = loc_group(&j, 1);
    assert_eq!(
        loc_values(&first),
        p1,
        "group 1 plans with the old passes unrestored"
    );
    let decision = j.next_decision();
    j.decide(decision);
    let ran = j.next_trial();
    assert_eq!(values(ran.profile), p1);
    j.trial(&ran, Outcome::Inconclusive);
    let retry = j.next_trial();
    assert!(retry.retry, "{retry:?}");
    assert_eq!(retry.requirement.context, ran.requirement.context);
    assert_eq!(values(retry.profile), p1);

    j.load(&loc_config(Some((
        crate::testkit::MPRIME_OLD,
        crate::testkit::YCRUNCHER_OLD,
    ))));
    let second = loc_group(&j, 2);
    let p2 = loc_values(&second);
    assert_ne!(p1, p2);
    let decision = j.next_decision();
    let second_seq = j.decide(decision);
    assert!(
        j.run().retry.is_none(),
        "group 1's retry outlived its group"
    );
    let want = j.next();
    let plan = trial(&want);
    assert!(!plan.retry, "{plan:?}");
    assert_eq!(
        plan.requirement.context,
        TrialContext::Hunt {
            hunt: std::num::NonZeroU32::MIN,
            group: std::num::NonZeroU32::new(2).unwrap(),
        }
    );
    assert_eq!(values(plan.profile), p2);
    assert_eq!(plan.cause, [second_seq]);
    assert_eq!(fresh(&j.events).next().ok(), Some(want));
    j.assert_resumes();
}

#[test]
fn requirement_checks_hunt_identity() {
    let mut j = loc_failed_hunt(4, 120);
    let plan = loc_first_hunt_trial(&mut j);
    let TrialContext::Hunt { hunt, group } = plan.requirement.context else {
        panic!("not a hunt trial: {:?}", plan.requirement.context);
    };
    let needed = j.run().hunts.active.as_ref().unwrap().needed;
    assert_eq!(plan.requirement.needed, needed);
    assert_eq!(
        needed.get(),
        u16::try_from(j.run().hunts.active.as_ref().unwrap().start.trials).unwrap()
    );
    j.intent(&plan);
    let shown = j.requirement();
    assert_eq!(shown.needed, u32::from(needed.get()));
    j.load(&config(r#"{"evidence":{"miss":0.001}}"#));
    assert_eq!(j.requirement(), shown);
    assert_eq!(j.run().hunts.active.as_ref().unwrap().needed, needed);

    for other in [
        TrialContext::Hunt {
            hunt: std::num::NonZeroU32::new(hunt.get() + 1).unwrap(),
            group,
        },
        TrialContext::Hunt {
            hunt,
            group: std::num::NonZeroU32::new(group.get() + 1).unwrap(),
        },
    ] {
        let mut j = loc_failed_hunt(4, 120);
        let mut plan = loc_first_hunt_trial(&mut j);
        plan.requirement.context = other;
        let intent = plan.intent(togi_journal::TrialId::from("other".to_owned()), 0);
        let error = j.try_append(intent.into(), &[]);
        assert!(
            matches!(error, Err(crate::FoldError::UnscheduledIntent(_))),
            "{other:?}: {error:?}"
        );
    }
}

fn loc_pair_session() -> Journal {
    let starts = [Start::at_limit(-10).fail(-11), Start::has_room(-10)];
    let mut j = Journal::new(&[0, 1], &starts);
    j.profile_change(&[-10, -10]);
    j.ranking(&[0, 1]);
    j
}

#[test]
fn carried_failure_establishes_later_hunt_group() {
    let mut j = loc_pair_session();
    let duration = j.run().config.short_trial.get();
    let failure = j.carry(
        SESSION,
        (Regime::R6, 0, &[0, 1], duration),
        Condition::Together,
        &[-10, 0],
        Outcome::Failure,
    );
    let start = loc_hand_start(&mut j, failure, &[-10, -10], duration);
    let Some(Action::Decide(group)) = j.run().hunt_next() else {
        panic!("the hunt planned no group");
    };
    let action = Action::Decide(group.clone());
    let payload = hunt_group(&action);
    assert_eq!(
        payload.inferred,
        Some(togi_journal::Inference::Failure),
        "{payload:?}"
    );
    assert_eq!(group.cause, [start, failure]);
    let reason = payload.reason.clone().unwrap();
    assert!(reason.contains(SESSION), "{reason}");
    j.decide(group);
    assert_eq!(
        j.run().hunt_groups()[0].outcome,
        Some(super::plan::GroupOutcome::Failed)
    );
    j.assert_resumes();
}

#[test]
fn a_carried_failure_is_admitted_to_a_group_past_a_newer_live_failure_before_the_hunt() {
    for idle in [false, true] {
        let mut j = loc_pair_session();
        let duration = j.run().config.short_trial.get();
        j.load(&config(&format!(
            r#"{{"durations":{{"checking_idle_s":{duration}}}}}"#
        )));
        let carried = if idle {
            j.carry_idle(SESSION, &[-9, 0])
        } else {
            j.carry(
                SESSION,
                (Regime::R6, 0, &[0, 1], duration),
                Condition::Together,
                &[-9, 0],
                Outcome::Failure,
            )
        };
        let live = |j: &mut Journal, profile: &[i64]| {
            let plan = j.manual(
                Regime::R6,
                0,
                Load::Many(set(&[0, 1])),
                Condition::Together,
                profile,
                duration,
            );
            j.trial(&plan, Outcome::Failure);
            let decision = j.next_decision();
            assert!(matches!(decision.payload, DecisionPayload::Failure(_)));
            j.decide(decision)
        };
        live(&mut j, &[-10, 0]);
        let source = live(&mut j, &[-10, -10]);
        loc_hand_start(&mut j, source, &[-10, -10], duration);
        let run = j.run();
        let hunt = run.hunts.active.as_ref().expect("the hunt is open");
        let class = hunt.class_at(Secs::new(std::num::NonZeroU32::new(duration).unwrap()));
        for stage in [HuntStage::Full, HuntStage::Probe] {
            let since = run.hunt_since(stage, hunt.seq);
            let answer = run.evidence_answer(hunt, class, &hunt.failing, since);
            assert_eq!(
                answer.outcome,
                Some(super::plan::GroupOutcome::Failed),
                "idle={idle} {stage:?}"
            );
            assert_eq!(answer.failure, Some(carried), "idle={idle} {stage:?}");
        }
        j.assert_resumes();
    }
}

#[test]
fn carried_passes_establish_a_hunt_group() {
    let mut j = loc_pair_session();
    let duration = j.run().config.short_trial.get();
    let n = j.run().config.needed.get();
    let facts: Vec<Seq> = (0..n)
        .map(|_| {
            j.carry(
                SESSION,
                (Regime::R6, 0, &[0, 1], duration),
                Condition::Together,
                &[-10, 0],
                Outcome::Pass,
            )
        })
        .collect();
    loc_hand_start(&mut j, facts[0], &[-10, -10], duration);
    let Some(Action::Decide(group)) = j.run().hunt_next() else {
        panic!("the hunt planned no group");
    };
    let action = Action::Decide(group.clone());
    let payload = hunt_group(&action);
    assert_eq!(
        payload.inferred,
        Some(togi_journal::Inference::Pass),
        "{payload:?}"
    );
    for fact in &facts {
        assert!(
            group.cause.contains(fact),
            "the group omitted carried cause #{fact}: {:?}",
            group.cause
        );
    }
    j.decide(group);
    let view = &j.run().hunt_groups()[0];
    assert_eq!(
        (view.passed, view.outcome, view.carried),
        (u32::from(n), Some(super::plan::GroupOutcome::Passed), true)
    );
    j.assert_resumes();
}

fn loc_hunt_before_member_probe(carried: &[Outcome]) -> (Journal, Vec<Seq>) {
    let mut j = loc_limit_session(4, -30);
    let duration = j.run().config.short_trial.get();
    let facts: Vec<Seq> = carried
        .iter()
        .map(|&outcome| {
            j.carry(
                SESSION,
                (Regime::R6, 0, &[0, 1, 2, 3], duration),
                Condition::Together,
                &[-29, -30, 0, 0],
                outcome,
            )
        })
        .collect();
    let plan = j.manual(
        Regime::R6,
        0,
        Load::Many(set(&[0, 1, 2, 3])),
        Condition::Together,
        &[-30, -30, -30, -30],
        duration,
    );
    j.trial(&plan, Outcome::Failure);
    let attribution = j.next_decision();
    j.decide(attribution);
    j.start_hunt();
    let mut oracle = |plan: &TrialPlan| {
        if at(plan, 0) <= -30 && at(plan, 1) <= -30 {
            Outcome::Failure
        } else {
            Outcome::Pass
        }
    };
    for _ in 0..200 {
        if hunt_group_of(&j.next()).is_some_and(|group| group.probe.is_some()) {
            return (j, facts);
        }
        j.step(&mut oracle);
    }
    panic!("the hunt never reached its first member probe");
}

fn hunt_group_of(action: &Action) -> Option<&HuntGroup> {
    if let Action::Decide(decision) = action
        && let DecisionPayload::HuntGroup(group) = &decision.payload
    {
        Some(group)
    } else {
        None
    }
}

#[test]
fn carried_evidence_before_the_hunt_establishes_a_member_probe() {
    let first = Member {
        core: core(0),
        offset: off(-29),
    };
    let needed = {
        let j = loc_limit_session(4, -30);
        usize::from(j.run().config.needed.get())
    };
    for passes in [true, false] {
        let carried = vec![
            if passes {
                Outcome::Pass
            } else {
                Outcome::Failure
            };
            if passes { needed } else { 1 }
        ];
        let (mut j, facts) = loc_hunt_before_member_probe(&carried);
        let hunt_seq = j.run().hunts.active.as_ref().expect("the hunt is open").seq;
        assert!(facts.iter().all(|&fact| fact < hunt_seq));
        let planned = j.next();
        let probe = hunt_group(&planned);
        assert_eq!(probe.stage, HuntStage::Probe);
        assert_eq!(probe.probe, Some(first));
        let cause = &decision(&planned).cause;
        assert_eq!(cause[0], hunt_seq);
        for fact in &facts {
            assert!(
                cause.contains(fact),
                "passes={passes}: #{fact} not cited in {cause:?}"
            );
        }
        let reason = probe.reason.clone().unwrap();
        assert!(reason.contains(SESSION), "{reason}");
        let want = if passes {
            togi_journal::Inference::Pass
        } else {
            togi_journal::Inference::Failure
        };
        assert_eq!(probe.inferred, Some(want), "passes={passes}: {probe:?}");
        j.decide(decision(&planned).clone());
        let view = j.run().hunt_groups().last().cloned().unwrap();
        let want = if passes {
            super::plan::GroupOutcome::Passed
        } else {
            super::plan::GroupOutcome::Failed
        };
        assert_eq!(view.probe, Some(first));
        assert_eq!(view.outcome, Some(want), "passes={passes}");
        assert!(view.carried, "passes={passes}: {view:?}");
        if passes {
            assert_eq!(view.passed, u32::try_from(needed).unwrap());
        }
        let plan = j.state.hunt_plan().expect("the hunt is still active");
        let ladder = plan
            .probes
            .iter()
            .find(|probe| probe.member == first.core)
            .expect("the probed member is on the plan");
        if passes {
            assert_eq!(ladder.passed_at, [first.offset], "{ladder:?}");
            assert_eq!(ladder.carried, [first.offset], "{ladder:?}");
        } else {
            assert!(ladder.passed_at.is_empty(), "{ladder:?}");
            assert!(ladder.carried.is_empty(), "{ladder:?}");
            assert!(ladder.failed_at.contains(&first.offset), "{ladder:?}");
        }
        let next = j.next();
        let following = hunt_group(&next);
        assert_eq!(following.inferred, None, "passes={passes}: {following:?}");
        let moved = following.probe.expect("the probes go on");
        let (member, offset) = if passes { (1, -29) } else { (0, -28) };
        assert_eq!((moved.core, moved.offset), (core(member), off(offset)));
        j.assert_resumes();
    }
}

#[test]
fn skipped_together_failure_makes_progress() {
    for attributed in [false, true] {
        let starts = [Start::at_limit(-20); 2];
        let mut j = Journal::new(&[0, 1], &starts);
        j.profile_change(&[-20, -20]);
        j.ranking(&[0, 1]);
        let duration = j.run().config.checking_idle.get();
        let mut fact = j.carried_trial(
            SESSION,
            togi_journal::TrialClass {
                regime: Regime::R6,
                workload: togi_machine::WorkloadId::new(Regime::R6, 0),
                cores: set(&[0, 1]),
                duration_s: duration,
            },
            Condition::Together,
            &[-20, -20],
            Outcome::Failure,
            None,
        );
        fact.core = attributed.then(|| core(0));
        let failure = j.append(fact.into(), &[]);
        j.cycle_start(1, &[Regime::R6]);
        if attributed {
            let retry = j.manual(
                Regime::R6,
                0,
                Load::Many(set(&[0, 1])),
                Condition::Together,
                &[-20, -20],
                duration,
            );
            j.trial(&retry, Outcome::Inconclusive);
            let pending = j
                .run()
                .retry
                .as_ref()
                .expect("the inconclusive trial left a retry");
            assert_eq!(pending.plan.profile, j.profile(&[-20, -20]));
        }
        let mut found = false;
        for _ in 0..100 {
            let action = j.loc_act();
            assert!(
                !matches!(action, Action::RunTrial(_)),
                "together failure was rerun before its decision: {action:?}"
            );
            let Action::Decide(decision) = &action else {
                unreachable!();
            };
            if let DecisionPayload::HuntStart(start) = &decision.payload {
                assert!(!attributed, "{start:?}");
                assert_eq!(start.failure, failure);
                assert!(decision.cause.contains(&failure), "{:?}", decision.cause);
                assert_eq!(start.failing, j.profile(&[-20, -20]).offsets());
                assert_eq!(
                    (start.regime, start.workload, start.duration_s),
                    (Regime::R6, workloads(Regime::R6)[0].id(), duration)
                );
                assert_eq!(start.cores, set(&[0, 1]));
                found = true;
            } else if let DecisionPayload::TunerDecision(moved) = &decision.payload
                && moved.decision == Decision::Backoff
            {
                assert!(attributed, "{moved:?}");
                assert_eq!(
                    (moved.core, moved.to_offset),
                    (core(0), off(-19)),
                    "{moved:?}"
                );
                assert!(decision.cause.contains(&failure), "{:?}", decision.cause);
                found = true;
            }
            j.decide(decision.clone());
            if found {
                break;
            }
        }
        assert!(
            found,
            "the skipped together step reached no hunt or backoff"
        );
        let mut live = false;
        for _ in 0..200 {
            match j.loc_act() {
                Action::RunTrial(_) => {
                    live = true;
                    break;
                }
                Action::Decide(decision) => {
                    j.decide(decision);
                }
                Action::ReadRanking => unreachable!(),
            }
        }
        assert!(live, "the skip loop prevented the next live trial");
        j.assert_resumes();
    }
}

#[test]
fn direct_failure_at_zero_in_a_hunt_group() {
    let starts = [Start::at_limit(-10).fail(-11); 2];
    let mut j = Journal::new(&[0, 1], &starts);
    j.profile_change(&[-10, -10]);
    j.ranking(&[0, 1]);
    let source = j.carry(
        SESSION,
        (Regime::R6, 0, &[0, 1], 120),
        Condition::Together,
        &[-10, -10],
        Outcome::Failure,
    );
    loc_hand_start(&mut j, source, &[-10, -10], 120);
    let planned = j.next_decision();
    let DecisionPayload::HuntGroup(group) = &planned.payload else {
        panic!("expected a group, got {planned:?}");
    };
    assert_eq!(loc_values_of(group, 2), [0, -10]);
    j.decide(planned);
    let plan = j.next_trial();
    let (_, failure) = {
        let end = j.loc_finish(&plan, Outcome::Failure, &R7Report::default().named(0));
        let attribution = j.next_decision();
        let DecisionPayload::Failure(record) = &attribution.payload else {
            panic!("expected the attribution, got {attribution:?}");
        };
        assert_eq!(record.core, Some(core(0)));
        (end, j.decide(attribution))
    };
    let rerun = j.next_trial();
    assert_eq!(
        rerun.requirement.context,
        TrialContext::ZeroRerun { failure }
    );
    assert_eq!(rerun.condition, Condition::Parked);
    assert_eq!(rerun.activity, Activity::Hunt);
    assert_eq!(values(rerun.profile), [0, 0]);
    let rerun_end = j.loc_finish(&rerun, Outcome::Failure, &R7Report::default());
    let attribution = j.next_decision();
    j.decide(attribution);
    let action = j.next();
    let dead = dead_end(&action);
    assert_eq!(dead.condition, DeadEndCondition::FailureAtZero);
    assert_eq!(dead.core, Some(core(0)));
    assert_eq!(decision(&action).cause, [failure, rerun_end]);
    let checking = j.run().checking.profile();
    let offsets = j.offsets();
    j.decide(decision(&action).clone());
    j.assert_resumes();
    for _ in 0..3 {
        let action = j.next();
        assert_eq!(dead_end(&action).condition, DeadEndCondition::FailureAtZero);
        assert_eq!(j.run().checking.profile(), checking);
        assert_eq!(j.offsets(), offsets);
    }
}

fn loc_values_of(group: &HuntGroup, count: u8) -> Vec<i64> {
    (0..count)
        .map(|id| group.profile.get(core(id)).unwrap().get())
        .collect()
}

#[derive(Clone, Copy)]
enum Source {
    Idle,
    Live(Regime, &'static [u8], u32),
    Carried(&'static [u8]),
}

fn loc_class_failure(offsets: &[i64], source: Source) -> (Journal, Seq) {
    let ids: Vec<u8> = (0..u8::try_from(offsets.len()).unwrap()).collect();
    let starts: Vec<Start> = offsets.iter().map(|&o| Start::has_room(o)).collect();
    let mut j = Journal::new(&ids, &starts);
    j.profile_change(offsets);
    j.ranking(&ids);
    let failure = match source {
        Source::Idle => {
            let profile = j.profile(offsets).offsets();
            j.idle_failure(profile, Condition::Together)
        }
        Source::Live(regime, loaded, duration) => {
            let load = match loaded {
                [single] => Load::Single(core(*single)),
                many => Load::Many(set(many)),
            };
            let plan = j.manual(regime, 0, load, Condition::Together, offsets, duration);
            j.trial(&plan, Outcome::Failure);
            let attribution = j.next_decision();
            let DecisionPayload::Failure(record) = &attribution.payload else {
                panic!("expected the attribution, got {attribution:?}");
            };
            assert_eq!(record.attribution, Attribution::Unattributed);
            j.decide(attribution)
        }
        Source::Carried(loaded) => {
            let duration = j.run().config.short_trial.get();
            let fact = j.carry(
                SESSION,
                (Regime::R6, 0, loaded, duration),
                Condition::Together,
                offsets,
                Outcome::Failure,
            );
            let plan = j.manual(
                Regime::R6,
                0,
                Load::Many(set(loaded)),
                Condition::Together,
                offsets,
                duration,
            );
            let skipped = j.run().skip_known_failure(Action::RunTrial(plan));
            let DecisionPayload::Failure(known) = &decision(&skipped).payload else {
                panic!("the carried failure did not skip the trial: {skipped:?}");
            };
            assert_eq!(known.known_failure, Some(fact));
            j.decide(decision(&skipped).clone());
            assert_eq!(j.run().failures.queue().next(), Some(fact));
            fact
        }
    };
    (j, failure)
}

#[test]
fn class_targets_preserve_hunt_and_rerun_lookup() {
    let four: &[i64] = &[-10, -11, -12, -13];
    let idle = 900;
    let cases: [(&str, &[i64], Source, &[u8]); 5] = [
        ("singleton before part in hunt", &[-10], Source::Idle, &[0]),
        (
            "single core",
            four,
            Source::Live(Regime::R1, &[1], 120),
            &[1],
        ),
        ("CCD part", four, Source::Carried(&[0, 1]), &[0, 1]),
        (
            "all cores",
            four,
            Source::Live(Regime::R6, &[0, 1, 2, 3], idle),
            &[0, 1, 2, 3],
        ),
        ("unknown target", four, Source::Carried(&[0, 2]), &[0, 2]),
    ];
    for (name, offsets, source, target) in cases {
        let (j, _) = loc_class_failure(offsets, source);
        let (start, _) = j.hunt_start_payload();
        assert_eq!(start.cores, set(target), "{name}");
        j.assert_resumes();

        let (mut j, failure) = loc_class_failure(offsets, source);
        let backoff = TunerDecision {
            core: core(0),
            activity: Activity::Checking,
            decision: Decision::Backoff,
            from_offset: off(offsets[0]),
            to_offset: off(offsets[0] + 1),
            pass: None,
            failure_point: Some(off(offsets[0])),
            workloads: Vec::new(),
            reason: "test".to_owned(),
        };
        j.append(backoff.into(), &[failure]);
        let mut rerun = None;
        for _ in 0..50 {
            match j.loc_act() {
                Action::RunTrial(plan) => {
                    rerun = Some(plan);
                    break;
                }
                Action::Decide(decision) => {
                    assert!(
                        !matches!(decision.payload, DecisionPayload::HuntStart(_)),
                        "{name}: the answered failure was hunted: {decision:?}"
                    );
                    j.decide(decision);
                }
                Action::ReadRanking => unreachable!(),
            }
        }
        let rerun = rerun.unwrap_or_else(|| panic!("{name}: no rerun"));
        assert_eq!(
            rerun.requirement.context,
            TrialContext::Rerun { failure },
            "{name}"
        );
        assert_eq!(rerun.load.cores(), set(target), "{name}");
        assert_eq!(rerun.duration, j.run().config.short_trial, "{name}");
        assert_eq!(rerun.condition, Condition::Together, "{name}");
        j.assert_resumes();
    }
    let mut empty = loc_limit_session(2, -10);
    let plan = empty.manual(
        Regime::R6,
        0,
        Load::Many(set(&[])),
        Condition::Together,
        &[-10, -10],
        120,
    );
    let intent = plan.intent(togi_journal::TrialId::from("0001".to_owned()), 0);
    assert!(matches!(
        empty.try_append(intent.into(), &[]),
        Err(crate::FoldError::NoLoad(_))
    ));
}

#[test]
fn attribution_at_group_parked_offsets_and_already_shallower() {
    for shallower in [false, true] {
        let mut j = super::support::hunt_session(&[-10, -12]);
        j.together_failure(Regime::R6, 0, &[-10, -12]);
        j.start_hunt();
        let planned = j.next_decision();
        assert!(matches!(planned.payload, DecisionPayload::HuntGroup(_)));
        j.decide(planned);
        let plan = j.next_trial();
        assert_eq!(plan.condition, Condition::Parked);
        let (named, failing) = plan
            .profile
            .iter()
            .find(|(_, offset)| offset.get() != 0)
            .expect("the group holds a candidate at its failing offset");
        let moved = off(failing.get() + 1);
        if shallower {
            j.core_phase(CoreTransition {
                core: named,
                from: Some(CorePhase::HasRoom),
                to: CorePhase::HasRoom,
                offset: moved,
                pass: None,
                failure_point: None,
                check_solo_limit: false,
                workloads: Vec::new(),
                cleared_combination: Vec::new(),
                reason: "test".to_owned(),
            });
        }
        j.loc_finish(
            &plan,
            Outcome::Failure,
            &R7Report::default().named(named.get()),
        );
        let attribution = j.next_decision();
        let DecisionPayload::Failure(record) = &attribution.payload else {
            panic!("expected the attribution, got {attribution:?}");
        };
        assert_eq!(record.attribution, Attribution::Attributed);
        assert_eq!(record.core, Some(named));
        assert_eq!(record.offset, Some(failing));
        assert_eq!(record.condition, Some(Condition::Parked));
        let failure = j.decide(attribution);
        let action = j.next();
        let end = hunt_end(&action);
        assert_eq!(
            (end.result, end.cores),
            (HuntOutcome::Direct, std::iter::once(named).collect())
        );
        assert_eq!(decision(&action).cause, [failure]);
        let ended = j.decide(decision(&action).clone());
        let action = j.next();
        let move_ = backoff(&action).clone();
        assert_eq!(move_.activity, Activity::Hunt);
        assert_eq!(move_.core, named);
        assert_eq!(move_.failure_point, Some(failing));
        assert_eq!(decision(&action).cause, [ended]);
        let from = if shallower { moved } else { failing };
        assert_eq!((move_.from_offset, move_.to_offset), (from, moved));
        assert_eq!(move_.reason.contains("already shallower"), shallower);
        j.decide(decision(&action).clone());
        assert!(j.run().hunts.active.is_none());
        assert_eq!(j.run().reruns.is_empty(), shallower);
        if !shallower {
            assert_eq!(
                j.run().reruns.head().map(|head| head.failure),
                Some(failure)
            );
        }
        j.assert_resumes();
    }
}

const RERUN_OFFSETS: [i64; 4] = [-10, -11, -12, -13];

fn loc_rerun_session() -> Journal {
    let starts: Vec<Start> = RERUN_OFFSETS
        .iter()
        .map(|&offset| Start::at_limit(offset).fail(offset - 1).pass(offset))
        .collect();
    let mut j = Journal::new(&[0, 1, 2, 3], &starts);
    j.load(&config(r#"{"checking":{"cycle":["R6"]}}"#));
    super::support::passed_cycle(&mut j, &[-5; 4], &RERUN_OFFSETS);
    j.ranking(&[0, 1, 2, 3]);
    j
}

fn loc_until_trial(j: &mut Journal) -> TrialPlan {
    for _ in 0..20 {
        match j.loc_act() {
            Action::RunTrial(plan) => return plan,
            Action::Decide(decision) => {
                j.decide(decision);
            }
            Action::ReadRanking => unreachable!(),
        }
    }
    panic!("no trial came");
}

fn loc_hunt_commitment(kind: &str) -> (Journal, TrialPlan, Seq) {
    let mut j = loc_rerun_session();
    let idle = j.manual(
        Regime::R6,
        0,
        Load::Many(set(&[0, 1, 2, 3])),
        Condition::Together,
        &RERUN_OFFSETS,
        900,
    );
    j.loc_finish(&idle, Outcome::Failure, &R7Report::default());
    let attribution = j.next_decision();
    let mut source = j.decide(attribution);
    if kind == "direct" {
        let plan = loc_until_trial(&mut j);
        assert!(
            matches!(plan.requirement.context, TrialContext::Hunt { .. }),
            "{plan:?}"
        );
        let (named, _) = plan
            .profile
            .iter()
            .find(|(_, offset)| offset.get() != 0)
            .unwrap();
        j.loc_finish(
            &plan,
            Outcome::Failure,
            &R7Report::default().named(named.get()),
        );
        let attribution = j.next_decision();
        source = j.decide(attribution);
    }
    let mut oracle = |plan: &TrialPlan| {
        let at = |id: u8| plan.profile.get(core(id)).unwrap().get();
        let fails = match kind {
            "culprit" => at(0) <= -10,
            "combination" => at(0) <= -10 && at(1) <= -11,
            _ => false,
        };
        if fails {
            Outcome::Failure
        } else {
            Outcome::Pass
        }
    };
    for _ in 0..500 {
        if let Action::Decide(decision) = j.step(&mut oracle) {
            if let DecisionPayload::HuntEnd(end) = &decision.payload {
                let result = match kind {
                    "culprit" => HuntOutcome::Culprit,
                    "combination" => HuntOutcome::Combination,
                    _ => HuntOutcome::Direct,
                };
                assert_eq!(end.result, result, "{kind}: {end:?}");
            }
            if matches!(&decision.payload, DecisionPayload::TunerDecision(td) if td.activity == Activity::Hunt)
            {
                assert!(j.run().hunts.active.is_none(), "{kind}");
                return (j, idle, source);
            }
        }
    }
    panic!("{kind}: the hunt never committed");
}

#[test]
fn rerun_derived_hunt_commitments() {
    for kind in ["culprit", "combination", "direct"] {
        let (mut j, idle, source) = loc_hunt_commitment(kind);
        j.assert_resumes();
        let n = usize::from(j.run().config.needed.get());
        let short = j.run().config.short_trial.get();
        let mut durations = vec![short; n];
        if kind != "direct" {
            durations.push(idle.duration.get());
        }
        let mut done = Vec::new();
        for _ in 0..100 {
            match j.loc_poll() {
                Some(Action::Decide(decision))
                    if matches!(
                        decision.payload,
                        DecisionPayload::ProfileChange(_) | DecisionPayload::CorePhase(_)
                    ) =>
                {
                    j.decide(decision);
                }
                Some(Action::RunTrial(plan))
                    if matches!(plan.requirement.context, TrialContext::Rerun { .. }) =>
                {
                    j.loc_finish(&plan, Outcome::Pass, &R7Report::default());
                    done.push(plan);
                }
                Some(Action::Decide(_) | Action::RunTrial(_) | Action::ReadRanking) | None => break,
            }
        }
        let got: Vec<u32> = done.iter().map(|plan| plan.duration.get()).collect();
        assert_eq!(got, durations, "{kind}");
        for plan in &done {
            assert_eq!(plan.condition, Condition::Together, "{kind}");
            assert_eq!(plan.regime, Regime::R6, "{kind}");
            assert_eq!(plan.workload, idle.workload, "{kind}");
            assert_eq!(plan.load, idle.load, "{kind}");
            assert_eq!(plan.cause, [source], "{kind}");
        }
        assert!(j.run().reruns.is_empty(), "{kind}");
        j.assert_resumes();
    }
}

#[test]
fn resetting_a_loaded_core_discards_the_saved_located_group_rejection() {
    for (reset, retained) in [(0, false), (2, true)] {
        let mut j = Journal::r7_session();
        j.loc_fail_escalated(&top0());
        let located = j.loc_run(|_| Verdict::Named(1));
        assert_eq!(hunt_end(&located.action).result, HuntOutcome::Loaded);
        let saved = j.run().hunts.rejecting.clone().unwrap();
        assert_eq!(saved.open.pending.fact.class.loaded(), set(&[0, 1]));
        j.reset(reset);
        assert_eq!(
            j.run().hunts.rejecting.as_deref(),
            retained.then_some(saved.as_ref()),
            "reset core {reset}"
        );
        j.assert_resumes();
    }
}

#[test]
fn a_skipped_or_inferred_locate_has_no_live_rejecting_failure() {
    for inferred in [false, true] {
        let mut j = Journal::r7_session();
        j.loc_fail_escalated(&top0());
        let located = j.loc_run_until(never, |_| true);
        let mut group = hunt_group(&located.action).clone();
        assert_eq!(group.stage, HuntStage::Locate);
        group.skipped = !inferred;
        group.inferred = inferred.then_some(togi_journal::Inference::Failure);
        j.append(group.into(), &decision(&located.action).cause);
        let active = j.run().hunts.active.as_ref().unwrap();
        assert_eq!(j.run().rejecting_failure(active), None);
        let action = j.run().hunt_next().unwrap();
        assert_eq!(hunt_end(&action).result, HuntOutcome::Loaded);
        j.assert_resumes();
    }
}

fn singleton_hunt(origin: i64, duration: u32) -> Journal {
    let mut j = Journal::r7_session();
    j.loc_set_core(2, origin);
    j.profile_change(&[-30, -30, origin, -30]);
    j.loc_back_off(&top0());
    // A two-CCD cycle gives each CCD one quarter of the configured all-core duration.
    j.load(&config(&format!(
        r#"{{"durations":{{"checking_all_core_s":{}}}}}"#,
        duration * 4
    )));
    let plan = j.manual(
        Regime::R7,
        0,
        Load::Many(set(&[0, 1])),
        Condition::Together,
        &[-30, -30, origin, -30],
        duration,
    );
    j.loc_finish(&plan, Outcome::Failure, &top0());
    let attribution = j.next_decision();
    j.decide(attribution);
    let start = j.loc_act();
    assert_eq!(hunt_start(&start).duration_s, duration);
    j.decide(decision(&start).clone());
    j
}

#[test]
fn singleton_probes_bracket_the_last_reproduced_failure_and_resume() {
    for (origin, limit, expected) in [
        (-30, -22, vec![-29, -28, -26, -22, -24, -23]),
        (-30, -29, vec![-29]),
        (-30, 0, vec![-29, -28, -26, -22, -14, -7, -4, -2, -1]),
        (-1, 0, vec![]),
    ] {
        let mut j = singleton_hunt(origin, 120);
        let mut probes = Vec::new();
        let located = j.loc_run_until(
            |plan| fails_as(at(plan, 2) < limit, Verdict::Unnamed),
            |group| {
                if let Some(probe) = group.probe {
                    assert_eq!(probe.core, core(2));
                    assert_eq!(group.set, set(&[2]));
                    assert_eq!(group.held, []);
                    assert!(!probes.contains(&probe.offset.get()));
                    probes.push(probe.offset.get());
                }
                false
            },
        );
        let end = hunt_end(&located.action);
        assert_eq!(end.result, HuntOutcome::Culprit);
        assert_eq!(probes, expected);
        if origin != -1 {
            assert_eq!(
                end.members,
                [Member {
                    core: core(2),
                    offset: off(limit - 1)
                }]
            );
        }
        j.assert_resumes();
        j.decide(decision(&located.action).clone());
        j.assert_resumes();
        let commitment = j.loc_act();
        let moved = backoff(&commitment);
        assert_eq!(
            (moved.core, moved.to_offset, moved.failure_point),
            (core(2), off(limit), Some(off(limit - 1)))
        );
        j.decide(decision(&commitment).clone());
        assert_eq!(j.run().hunt_next(), None);
        j.assert_resumes();
    }
}

#[test]
fn singleton_without_existing_same_class_anchor_tests_it_before_accelerating() {
    let mut j = singleton_hunt(-30, 600);
    let stopped = j.loc_run_until(
        |plan| fails_as(at(plan, 2) < -22, Verdict::Unnamed),
        |group| group.stage == HuntStage::Probe,
    );
    let anchor = hunt_group(&stopped.action);
    assert_eq!(
        anchor.probe,
        Some(Member {
            core: core(2),
            offset: off(0)
        })
    );
    assert_eq!(anchor.duration_s, 120);
    assert_eq!(anchor.inferred, None);
    assert!(
        anchor
            .reason
            .as_deref()
            .unwrap()
            .contains("safe singleton anchor")
    );
    j.decide(decision(&stopped.action).clone());
    for passed in 0..5 {
        let projection = j.run().hunt_plan().unwrap();
        assert_eq!(projection.probes.len(), 1);
        assert_eq!(projection.probes[0].offset, off(0));
        assert!(!projection.probes[0].done);
        j.assert_resumes();
        let plan = j.next_trial();
        assert_eq!(at(&plan, 2), 0);
        assert_eq!(plan.duration.get(), 120);
        j.loc_finish(&plan, Outcome::Pass, &R7Report::default());
        assert_eq!(j.run().hunt_groups().last().unwrap().passed, passed + 1);
    }
    let first = j.loc_act();
    let probe = hunt_group(&first);
    assert_eq!(
        probe.probe,
        Some(Member {
            core: core(2),
            offset: off(-29)
        })
    );
    assert_eq!(probe.duration_s, 120);
    assert_eq!(probe.index, 1);
    j.assert_resumes();
}

#[test]
fn a_failed_singleton_anchor_never_creates_a_safe_bracket() {
    for all_zero in [false, true] {
        let mut j = singleton_hunt(-30, 600);
        if all_zero {
            // Supply an all-zero parked background in the recorded start, not a safe pass.
            let active = j.run().hunts.active.as_ref().unwrap();
            let mut start = active.start.clone();
            start.parked = j.profile(&[0; 4]).offsets();
            let cause = vec![active.seq];
            j.append(start.into(), &cause);
        }
        let stopped = j.loc_run_until(
            |plan| fails_as(at(plan, 2) < -22, Verdict::Unnamed),
            |group| group.stage == HuntStage::Probe,
        );
        let anchor = hunt_group(&stopped.action);
        assert_eq!(anchor.probe.unwrap().offset, off(0));
        j.decide(decision(&stopped.action).clone());
        let plan = j.next_trial();
        j.loc_finish(&plan, Outcome::Failure, &R7Report::default());
        let attribution = j.next_decision();
        j.decide(attribution);
        j.assert_resumes();
        let action = j.loc_act();
        if all_zero {
            assert_eq!(dead_end(&action).condition, DeadEndCondition::FailureAtZero);
        } else {
            let end = hunt_end(&action);
            assert_eq!(end.result, HuntOutcome::Culprit);
            assert_eq!(
                end.members,
                [Member {
                    core: core(2),
                    offset: off(-30)
                }]
            );
            j.decide(decision(&action).clone());
            let commitment = j.loc_act();
            assert_eq!(backoff(&commitment).to_offset, off(-29));
        }
        j.assert_resumes();
    }
}

#[test]
fn singleton_anchor_evidence_keeps_the_exact_class_and_backend() {
    for (workload, loaded, duration, changed_backend, expected_anchor) in [
        (0, &[0, 1][..], 120, false, false),
        (1, &[0, 1][..], 120, false, true),
        (0, &[0, 2][..], 120, false, true),
        (0, &[0, 1][..], 600, false, true),
        (0, &[0, 1][..], 120, true, true),
    ] {
        let mut j = singleton_hunt(-30, 600);
        j.load(&loc_config(Some((
            crate::testkit::MPRIME_OLD,
            crate::testkit::YCRUNCHER_OLD,
        ))));
        let backend = workloads(Regime::R7)[workload].backend;
        let old = match backend {
            togi_machine::Backend::Mprime => crate::testkit::MPRIME_OLD,
            togi_machine::Backend::Ycruncher => crate::testkit::YCRUNCHER_OLD,
        };
        // These class variants need not be schedulable parts of the current cycle.
        for _ in 0..5 {
            j.carry_under(
                SESSION,
                (Regime::R7, workload, loaded, duration),
                Condition::Together,
                &[-30, -30, 0, 0],
                Outcome::Pass,
                Some((backend, old)),
            );
        }
        if changed_backend {
            j.load(&loc_config(Some((
                crate::testkit::MPRIME_NEW,
                crate::testkit::YCRUNCHER_NEW,
            ))));
        }
        let stopped = j.loc_run_until(
            |plan| fails_as(at(plan, 2) < -22, Verdict::Unnamed),
            |group| group.stage == HuntStage::Probe,
        );
        assert_eq!(
            hunt_group(&stopped.action).probe.unwrap().offset,
            off(if expected_anchor { 0 } else { -29 })
        );
        j.assert_resumes();
    }
}

#[test]
fn reset_cancels_singleton_anchor_acquisition_and_interior_probes() {
    for interior in [false, true] {
        let mut j = singleton_hunt(-30, 600);
        let stopped = j.loc_run_until(
            |plan| fails_as(at(plan, 2) < -22, Verdict::Unnamed),
            |group| group.stage == HuntStage::Probe,
        );
        j.decide(decision(&stopped.action).clone());
        for _ in 0..if interior { 5 } else { 2 } {
            let plan = j.next_trial();
            j.loc_finish(&plan, Outcome::Pass, &R7Report::default());
            j.assert_resumes();
        }
        if interior {
            let next = j.loc_act();
            assert_eq!(hunt_group(&next).probe.unwrap().offset, off(-29));
            j.decide(decision(&next).clone());
            let plan = j.next_trial();
            j.loc_finish(&plan, Outcome::Inconclusive, &R7Report::default());
            j.assert_resumes();
        }
        j.reset(2);
        let cancel = j.loc_act();
        assert_eq!(hunt_end(&cancel).result, HuntOutcome::Cancelled);
        j.decide(decision(&cancel).clone());
        assert_eq!(j.run().hunt_next(), None);
        assert_eq!(at(&j.r7_plan(&[0, 1], &values(j.offsets())), 2), -30);
        j.assert_resumes();
    }
}

#[test]
fn an_ordinary_singleton_hunt_acquires_a_nonzero_parked_anchor() {
    let mut j = super::support::hunt_session(&[-30; 4]);
    super::support::passed_cycle(&mut j, &[-10; 4], &[-30; 4]);
    j.together_failure(Regime::R6, 0, &[-30; 4]);
    let (_, start) = j.start_hunt();
    assert_eq!(start.parked, j.profile(&[-10; 4]).offsets());
    let end = j.run_hunt(300, |plan| {
        if at(plan, 0) < -22 {
            Outcome::Failure
        } else {
            Outcome::Pass
        }
    });
    assert_eq!(end.result, HuntOutcome::Culprit);
    assert_eq!(
        end.members,
        [Member {
            core: core(0),
            offset: off(-23)
        }]
    );
    let active = j.run().hunts.active.as_ref().unwrap();
    let probes: Vec<_> = active
        .groups
        .iter()
        .filter_map(|group| group.payload.probe)
        .collect();
    assert_eq!(
        probes.first(),
        Some(&Member {
            core: core(0),
            offset: off(-10)
        })
    );
    assert_eq!(
        probes
            .iter()
            .map(|probe| probe.offset.get())
            .collect::<Vec<_>>(),
        [-10, -29, -28, -26, -22, -24, -23]
    );
    j.assert_resumes();
    let action = j.next();
    let moved = backoff(&action);
    assert_eq!(
        (moved.to_offset, moved.failure_point),
        (off(-22), Some(off(-23)))
    );
    j.decide(decision(&action).clone());
    assert_eq!(j.run().hunt_next(), None);
    j.assert_resumes();
}
