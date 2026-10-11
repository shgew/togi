use togi_journal::{
    Attribution, DeadEnd, DeadEndCondition, Decision, Outcome, Payload, SessionWarning, Shutdown,
    ShutdownReason, TunerDecision,
};
use togi_machine::{CoreInfo, Regime};

use crate::chain_tests::{chain_journal, pass_part};
use crate::forecast::{AllPass, Branch, Forecast, Premise, Uncertainty};
use crate::testkit::{Journal, R7Report, Start, fresh, pairs, quads, set};
use crate::tests::core;
use crate::{Action, DecisionPayload, Load, State, TrialContext, TrialPlan};

fn topology(count: u8) -> Vec<CoreInfo> {
    let ids: Vec<u8> = (0..count).collect();
    match count {
        4 => pairs(&ids),
        _ => quads(&ids),
    }
}

fn room(offsets: &[i64]) -> Journal {
    let ids: Vec<u8> = (0..u8::try_from(offsets.len()).unwrap()).collect();
    let starts: Vec<Start> = offsets.iter().map(|&at| Start::has_room(at)).collect();
    let mut j = Journal::on(
        &ids,
        topology(u8::try_from(offsets.len()).unwrap()),
        &starts,
    );
    j.profile_change(offsets);
    j
}

fn branch(forecast: &Forecast, premise: Premise) -> &Branch {
    forecast
        .branches
        .iter()
        .find(|branch| branch.premise == premise)
        .unwrap_or_else(|| panic!("no {premise:?} branch in {forecast:#?}"))
}

fn named(forecast: &Forecast) -> Vec<&Branch> {
    forecast
        .branches
        .iter()
        .filter(|branch| matches!(branch.premise, Premise::Named(_)))
        .collect()
}

fn backoff(branch: &Branch) -> Option<&TunerDecision> {
    branch.decisions.iter().find_map(|decision| match decision {
        DecisionPayload::TunerDecision(td) if td.decision == Decision::Backoff => Some(td),
        _ => None,
    })
}

fn dead_end(branch: &Branch) -> bool {
    branch
        .decisions
        .iter()
        .any(|decision| matches!(decision, DecisionPayload::DeadEnd(_)))
}

fn rerun(plan: &TrialPlan) -> bool {
    matches!(
        plan.context,
        TrialContext::Rerun { .. } | TrialContext::ZeroRerun { .. }
    )
}

fn zero_rerun(branch: &Branch) -> bool {
    branch.next.as_ref().is_some_and(|next| {
        rerun(next) && next.condition == togi_machine::Condition::Parked && next.profile.is_zero()
    })
}

fn partial_in_flight() -> Journal {
    let mut j = chain_journal();
    pass_part(&mut j, &[0, 1, 2, 3], &[]);
    let chain = j.next_decision();
    j.decide(chain);
    j
}

fn start(j: &mut Journal) -> TrialPlan {
    let plan = j.next_trial();
    j.intent(&plan);
    plan
}

fn end_pass(
    j: &mut Journal,
    intent: togi_journal::Seq,
    trial: &togi_journal::TrialId,
    duration: u32,
) {
    let mut end = Journal::end_payload(trial, Outcome::Pass, None);
    end.duration_s = duration;
    j.append(end.into(), &[intent]);
}

fn trial_pass(j: &mut Journal, plan: &TrialPlan) {
    let (intent, trial) = j.intent(plan);
    end_pass(j, intent, &trial, plan.duration.get());
}

#[test]
fn forecast_partial_failure_requires_backoff() {
    let mut j = partial_in_flight();
    start(&mut j);
    let forecast = j.state.forecast();
    let mut seen = Vec::new();
    for branch in &forecast.branches {
        seen.push(branch.premise);
        match branch.premise {
            Premise::Named(_) | Premise::Unnamed => {
                assert!(backoff(branch).is_some(), "{branch:?}");
                assert!(branch.next.as_ref().is_some_and(rerun), "{branch:?}");
            }
            Premise::Pass => {
                assert!(
                    branch.decisions.is_empty() && branch.next.is_some(),
                    "{branch:?}"
                );
            }
            Premise::AllPass => {
                assert_eq!(branch.passes, 3, "{branch:?}");
                assert_eq!(branch.all_pass, Some(AllPass::Complete));
                assert_eq!(branch.next.as_ref().unwrap().duration.get(), 300);
            }
            Premise::Inconclusive => {}
        }
    }
    assert!(seen.iter().any(|p| matches!(p, Premise::Named(_))));
    assert!(seen.contains(&Premise::Unnamed));
    assert!(seen.contains(&Premise::AllPass));
}

#[test]
fn forecast_all_pass_counts_partial_passes() {
    let mut j = partial_in_flight();
    let first = j.next_trial();
    trial_pass(&mut j, &first);
    start(&mut j);
    let forecast = j.state.forecast();
    let pass = branch(&forecast, Premise::Pass);
    let all = branch(&forecast, Premise::AllPass);
    assert_eq!(all.passes, 2);
    assert_eq!(pass.next.as_ref().unwrap().duration.get(), 120);
    assert_eq!(all.next.as_ref().unwrap().duration.get(), 300);
}

#[test]
fn forecast_needs_ranking_without_recorded_read() {
    let mut j = room(&[-20; 4]);
    let plan = j.r7_plan(&[0, 1], &[-20; 4]);
    j.intent(&plan);
    let forecast = j.state.forecast();
    let unnamed = branch(&forecast, Premise::Unnamed);
    assert_eq!(unnamed.uncertainty, Some(Uncertainty::NeedsRanking));
    assert!(unnamed.next.is_none());
}

#[test]
fn forecast_single_loaded_together_may_be_unattributed() {
    let mut j = room(&[-20, -20]);
    let plan = j.manual(
        Regime::R1,
        0,
        Load::Single(core(0)),
        togi_machine::Condition::Together,
        &[-20, -20],
        120,
    );
    j.intent(&plan);
    let forecast = j.state.forecast();
    let unnamed = branch(&forecast, Premise::Unnamed);
    assert!(matches!(
        &unnamed.decisions[0],
        DecisionPayload::Failure(failure) if failure.attribution == Attribution::Unattributed
    ));
}

#[test]
fn forecast_inconclusive_retains_trial() {
    for partial in [false, true] {
        let mut j = if partial {
            partial_in_flight()
        } else {
            Journal::search_at(&[-20])
        };
        let plan = start(&mut j);
        let forecast = j.state.forecast();
        let inconclusive = branch(&forecast, Premise::Inconclusive);
        assert!(inconclusive.decisions.is_empty(), "{inconclusive:?}");
        let mut want = plan.clone();
        want.retry = true;
        want.workload = crate::WorkloadChoice::Frozen(plan.workload_at(j.rotation(&plan)));
        let next = inconclusive.next.as_ref().expect("the trial is retained");
        assert_eq!(next.retry, want.retry);
        assert_eq!(next.workload, want.workload);
        assert_eq!(next.requirement, want.requirement);
        assert_eq!(next.context, want.context);
        assert_eq!(next.profile, want.profile);
        assert_eq!(next.duration, want.duration);
    }
}

#[test]
fn forecast_reuses_recorded_ranking() {
    let mut j = room(&[-20, -20, 0, 0]);
    j.ranking(&[1, 0, 2, 3]);
    let plan = j.r7_plan(&[0, 1], &[-20, -20, 0, 0]);
    j.intent(&plan);
    let forecast = j.state.forecast();
    let unnamed = branch(&forecast, Premise::Unnamed);
    assert_eq!(unnamed.uncertainty, None, "{unnamed:?}");
    let next = unnamed.next.as_ref().expect("a settled branch");
    assert!(rerun(next));
    assert_eq!(next.condition, togi_machine::Condition::Together);
    let moved = backoff(unnamed).expect("voltage-targeted backoff");
    assert_eq!((moved.core.get(), moved.to_offset.get()), (0, -19));
}

#[test]
fn forecast_waits_for_initial_core_phases() {
    let j = Journal::on(&[0, 1], topology(2), &[Start::search(-20)]);
    for n in 1..=j.events.len() {
        let state = fresh(&j.events[..n]);
        assert_eq!(state.forecast(), Forecast::default(), "prefix {n}");
        assert!(
            state
                .phase_plan()
                .is_none_or(|plan| plan.candidates.is_empty()),
            "prefix {n}"
        );
    }
}

#[test]
fn forecast_all_pass_stays_within_its_requirement() {
    let starts = [Start::search(-20).checking(), Start::search(-10)];
    let mut j = Journal::on(&[0, 1], infos_two(), &starts);
    for _ in 0..20 {
        let action = j.next();
        let plan = match action {
            Action::Decide(decision) => {
                j.decide(decision);
                continue;
            }
            Action::RunTrial(plan) => plan,
            Action::ReadRanking => panic!("ranking read"),
        };
        let (_, trial) = j.intent(&plan);
        let forecast = j.state.forecast();
        let pass = forecast
            .branches
            .iter()
            .find(|branch| branch.premise == Premise::Pass)
            .and_then(|branch| branch.next.clone());
        if pass
            .as_ref()
            .is_some_and(|next| next.load != plan.load || next.regime != plan.regime)
        {
            assert!(
                !forecast
                    .branches
                    .iter()
                    .any(|branch| branch.premise == Premise::AllPass)
            );
        }
        if let (Load::Single(core0), Some(pass)) = (&plan.load, &pass) {
            if *core0 == core(0) && pass.load == Load::Single(core(1)) {
                return;
            }
        }
        let seq = j.events.last().unwrap().seq;
        end_pass(&mut j, seq, &trial, plan.duration.get());
    }
    panic!("search never interleaved the confirming core with the searching one");
}

fn infos_two() -> Vec<CoreInfo> {
    crate::testkit::infos(&[0, 1])
}

#[test]
fn forecast_names_core_at_zero_separately() {
    let mut j = room(&[-20, 0]);
    let plan = j.manual(
        Regime::R6,
        0,
        Load::Many(set(&[0, 1])),
        togi_machine::Condition::Together,
        &[-20, 0],
        j.run().config.checking_idle.get(),
    );
    j.intent(&plan);
    let forecast = j.state.forecast();
    let named = named(&forecast);
    let cores: Vec<Premise> = named.iter().map(|b| b.premise).collect();
    assert_eq!(cores, [Premise::Named(core(0)), Premise::Named(core(1))]);
    assert!(named.iter().all(|b| !dead_end(b)));
    assert!(zero_rerun(named[1]), "{:?}", named[1]);
}

#[test]
fn forecast_r7_premises_fold_distinct_outcomes_without_invented_telemetry() {
    let offsets = [-30, -30, 0, 0, -30, -30, -30, -30];
    let mut j = room(&offsets);
    j.ranking(&[0, 1, 2, 3, 4, 5, 6, 7]);
    j.r7_fact(
        true,
        &[0, 1, 2, 3],
        &offsets,
        &R7Report::default()
            .requests(&[(0, 1.1), (1, 1.15), (2, 1.09), (3, 1.2)])
            .top(&[3]),
    );
    j.cycle_start(1, &[Regime::R7]);
    let decision = j.next_decision();
    j.decide(decision);
    let plan = start(&mut j);
    let forecast = j.state.forecast();
    let want = [
        (Premise::Pass, None, None),
        (Premise::AllPass, None, None),
        (Premise::Named(core(0)), Some(0), Some(-29)),
        (Premise::Named(core(2)), Some(1), Some(-16)),
        (Premise::Named(core(3)), None, None),
        (Premise::Unnamed, Some(1), Some(-16)),
        (Premise::Inconclusive, None, None),
    ];
    let got: Vec<_> = forecast
        .branches
        .iter()
        .map(|b| {
            (
                b.premise,
                backoff(b).map(|td| td.core.get()),
                backoff(b).map(|td| td.to_offset.get()),
            )
        })
        .collect();
    assert_eq!(got, want);
    for branch in &forecast.branches {
        assert_eq!(
            branch.without_telemetry,
            branch.premise != Premise::Inconclusive,
            "{branch:?}"
        );
        assert!(!branch.offset_order, "{branch:?}");
        if let Premise::Named(named) = branch.premise {
            assert_eq!(branch.at_zero, named.get() != 0, "{branch:?}");
            assert_eq!(branch.top_requester, named.get() == 3, "{branch:?}");
        }
        assert!(
            !branch.decisions.iter().any(|d| matches!(
                d,
                DecisionPayload::HuntStart(_)
                    | DecisionPayload::HuntGroup(_)
                    | DecisionPayload::Combination(_)
            )),
            "{branch:?}"
        );
        if dead_end(branch) {
            assert!(branch.next.is_none());
        } else {
            assert!(
                branch.next.is_some() && branch.uncertainty.is_none(),
                "{branch:?}"
            );
        }
        if backoff(branch).is_some() {
            let next = branch.next.as_ref().unwrap();
            assert!(rerun(next) && next.load == plan.load, "{branch:?}");
        }
        if branch.premise == Premise::AllPass {
            assert_eq!(branch.passes, 3);
            assert_eq!(branch.next.as_ref().unwrap().duration.get(), 300);
            continue;
        }
        let mut independent = j.resumed();
        let trial = plan_trial(&j);
        let mut end = Journal::end_payload(
            &trial,
            match branch.premise {
                Premise::Pass | Premise::AllPass => Outcome::Pass,
                Premise::Inconclusive => Outcome::Inconclusive,
                _ => Outcome::Failure,
            },
            matches!(branch.premise, Premise::Named(_) | Premise::Unnamed)
                .then_some(togi_machine::Signal::ComputationError),
        );
        end.duration_s = if branch.premise == Premise::Inconclusive {
            0
        } else {
            plan.duration.get()
        };
        if let Premise::Named(named) = branch.premise {
            end.core = Some(named);
        }
        independent.append(end.into(), &[]);
        let mut decisions = Vec::new();
        let mut next = None;
        for _ in 0..100 {
            match independent.next() {
                Action::ReadRanking => panic!("unexpected ranking read"),
                Action::RunTrial(mut trial) => {
                    trial.workload = crate::WorkloadChoice::Frozen(
                        trial.workload_at(independent.rotation(&trial)),
                    );
                    next = Some(trial);
                    break;
                }
                Action::Decide(decision) => {
                    decisions.push(decision.payload.clone());
                    let stop = matches!(decision.payload, DecisionPayload::DeadEnd(_));
                    independent.decide(decision);
                    if stop {
                        break;
                    }
                }
            }
        }
        assert_eq!(decisions, branch.decisions, "{:?}", branch.premise);
        assert_eq!(next, branch.next, "{:?}", branch.premise);
    }
}

fn plan_trial(j: &Journal) -> togi_journal::TrialId {
    j.state
        .run()
        .unwrap()
        .flight
        .open
        .as_ref()
        .unwrap()
        .intent
        .trial
        .clone()
}

#[test]
fn forecast_r7_unnamed_uses_offset_fallback_without_requests() {
    let mut j = room(&[0, -30, 0, 0]);
    j.ranking(&[0, 1, 2, 3]);
    let plan = j.r7_plan(&[0, 1], &[0, -30, 0, 0]);
    j.intent(&plan);
    let forecast = j.state.forecast();
    let unnamed = branch(&forecast, Premise::Unnamed);
    assert!(!dead_end(unnamed));
    assert!(unnamed.next.as_ref().is_some_and(rerun), "{unnamed:?}");
    assert!(unnamed.offset_order);
    let moved = backoff(unnamed).expect("offset-fallback backoff");
    assert_eq!((moved.core.get(), moved.to_offset.get()), (1, -29));
}

#[test]
fn forecast_r7_names_a_core_at_zero_on_each_ccd() {
    let mut j = room(&[0, 0, 0, -30]);
    j.ranking(&[0, 1, 2, 3]);
    let all = [0, 1, 2, 3];
    j.r7_fact(
        true,
        &all,
        &[0, 0, 0, -30],
        &R7Report::default().requests(&[(0, 1.2), (1, 1.1), (2, 1.1), (3, 1.2)]),
    );
    let plan = j.r7_plan(&all, &[0, 0, 0, -30]);
    j.intent(&plan);
    let forecast = j.state.forecast();
    let got: Vec<_> = named(&forecast)
        .into_iter()
        .map(|b| {
            let Premise::Named(core) = b.premise else {
                unreachable!()
            };
            (
                core.get(),
                b.at_zero,
                b.top_requester,
                dead_end(b),
                zero_rerun(b),
                backoff(b).map(|td| td.core.get()),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (3, false, true, false, false, Some(3)),
            (1, true, false, false, true, None),
            (2, true, false, false, false, Some(3)),
            (0, true, true, false, true, None),
        ]
    );
}

#[test]
fn forecast_r7_basis_follows_the_running_loads_measurement() {
    let mut j = chain_journal();
    pass_part(&mut j, &[0, 1, 2, 3], &[(0, 1.19), (1, 1.11), (2, 1.2)]);
    let chain = j.next_decision();
    let DecisionPayload::CheckingChain(record) = &chain.payload else {
        panic!("expected a checking.chain, got {chain:?}");
    };
    let partial = record.cores;
    assert!(partial.contains(core(3)), "{record:?}");
    j.decide(chain);
    let plan = j.next_trial();
    assert_eq!(plan.load, Load::Many(partial));
    let (intent, trial) = j.intent(&plan);
    let unmeasured = j.state.forecast();
    let unnamed = branch(&unmeasured, Premise::Unnamed);
    assert!(
        unnamed.offset_order,
        "fixture: the full part has no lane for core 3: {unnamed:?}"
    );
    let mut end = Journal::end_payload(&trial, Outcome::Pass, None);
    end.duration_s = plan.duration.get();
    end.voltage_requests_v = partial.iter().zip([1.19, 1.25, 1.1]).collect();
    j.append(end.into(), &[intent]);
    let plan = j.next_trial();
    assert_eq!(plan.load, Load::Many(partial));
    j.intent(&plan);
    let measured = j.state.forecast();
    let unnamed = branch(&measured, Premise::Unnamed);
    assert!(!unnamed.offset_order, "{unnamed:?}");
}

#[test]
fn forecast_unmeasured_chain_ending_follows_offset_order() {
    let mut j = room(&[-20; 4]);
    j.ranking(&[0, 1, 2, 3]);
    j.cycle_start(1, &[Regime::R7]);
    for _ in 0..20 {
        let plan = loop {
            match j.next() {
                Action::Decide(decision) => {
                    j.decide(decision);
                }
                Action::RunTrial(plan) => break plan,
                Action::ReadRanking => panic!("ranking read"),
            }
        };
        let (intent, trial) = j.intent(&plan);
        let forecast = j.state.forecast();
        let pass = branch(&forecast, Premise::Pass);
        for decision in &pass.decisions {
            if let DecisionPayload::CheckingChain(chain) = decision {
                if chain.source_seqs.is_empty() {
                    assert!(chain.cores.is_empty() && pass.offset_order, "{pass:?}");
                    return;
                }
            }
        }
        end_pass(&mut j, intent, &trial, plan.duration.get());
    }
    panic!("no pass forecast derived the CCD's chain");
}

#[test]
fn forecast_resume_names_no_step_of_a_later_cycle() {
    let config = crate::testkit::config(r#"{"checking":{"cycle":["R1","R6"]}}"#);
    let mut j = Journal::on(
        &[0, 1],
        crate::testkit::infos(&[0, 1]),
        &[Start::has_room(-20), Start::has_room(-20)],
    );
    j.load(&config);
    let mut failed_r6 = false;
    let (mut reruns, mut between, mut ended, mut later) = (Vec::new(), 0, 0, 0);
    let cycle_of = |j: &Journal| j.run().checking.last_cycle().map(std::num::NonZero::get);
    for _ in 0..200 {
        if failed_r6 {
            let forecast = j.state.forecast();
            let between_trials = forecast.between.clone().unwrap_or_default();
            if cycle_of(&j) == Some(1) {
                assert_eq!(between_trials.resume, None, "{between_trials:?}");
                between += 1;
                if matches!(
                    &j.events.last().unwrap().payload,
                    Payload::CheckingCycle(c) if c.event == togi_journal::CycleEvent::End
                ) {
                    ended += 1;
                    let next = between_trials.next.as_ref().expect("cycle 2's first trial");
                    assert!(
                        matches!(next.context, TrialContext::Cycle { cycle, .. } if cycle.get() == 2),
                        "{next:?}"
                    );
                }
            } else if between_trials
                .next
                .as_ref()
                .is_some_and(|next| !rerun(next))
            {
                later += 1;
                assert_eq!(between_trials.next_step, between_trials.resume);
                assert_eq!(between_trials.resume.map(|s| s.get().get()), Some(1));
            }
        }
        let plan = match j.next() {
            Action::Decide(decision) => {
                j.decide(decision);
                continue;
            }
            Action::RunTrial(plan) => plan,
            Action::ReadRanking => {
                j.ranking(&[0, 1]);
                continue;
            }
        };
        if rerun(&plan) {
            let (intent, trial) = j.intent(&plan);
            let forecast = j.state.forecast();
            let pass = branch(&forecast, Premise::Pass);
            assert_eq!(pass.resume, None);
            reruns.push(forecast.clone());
            end_pass(&mut j, intent, &trial, plan.duration.get());
            continue;
        }
        let in_cycle_two =
            matches!(plan.context, TrialContext::Cycle { cycle, .. } if cycle.get() == 2);
        if in_cycle_two {
            break;
        }
        if plan.regime == Regime::R6 && !failed_r6 {
            failed_r6 = true;
            let (intent, trial) = j.intent(&plan);
            let mut end = Journal::end_payload(
                &trial,
                Outcome::Failure,
                Some(togi_machine::Signal::ComputationError),
            );
            end.core = Some(core(0));
            end.duration_s = 41;
            j.append(end.into(), &[intent]);
            continue;
        }
        assert!(!failed_r6, "cycle 1 ran {plan:?} after its R6 failure");
        trial_pass(&mut j, &plan);
    }
    assert!(
        !reruns.is_empty() && between > 0 && ended > 0 && later > 0,
        "{} {between} {ended} {later}",
        reruns.len()
    );
    let last = reruns.last().unwrap();
    let pass = branch(last, Premise::Pass);
    let next = pass.next.as_ref().expect("cycle 2's first trial");
    assert!(
        matches!(next.context, TrialContext::Cycle { cycle, .. } if cycle.get() == 2)
            && !rerun(next)
            && pass.next_step.map(|s| s.get().get()) == Some(1)
    );
    let closes = pass.decisions.iter().any(|d| {
        matches!(d, DecisionPayload::CheckingCycle(c) if c.event == togi_journal::CycleEvent::End && c.cycle.get() == 1)
    });
    let opens = pass.decisions.iter().any(|d| {
        matches!(d, DecisionPayload::CheckingCycle(c) if c.event == togi_journal::CycleEvent::Start && c.cycle.get() == 2)
    });
    assert!(closes && opens, "{:?}", pass.decisions);
}

fn stopped_journal(stop: Payload) -> Journal {
    let mut j = room(&[-20; 4]);
    let plan = j.r7_plan(&[0, 1], &[-20; 4]);
    j.intent(&plan);
    assert!(!j.state.forecast().branches.is_empty());
    j.append(stop, &[]);
    j
}

fn warning() -> Payload {
    SessionWarning {
        operation: "smu".to_owned(),
        trial: None,
        error: "late".to_owned(),
    }
    .into()
}

#[test]
fn forecast_is_empty_after_shutdown_and_deadend_despite_trailing_warning() {
    let stops: [Payload; 2] = [
        Shutdown {
            reason: ShutdownReason::Signal,
            kernel_cursor: None,
            kernel_error: None,
            cycles: None,
        }
        .into(),
        DeadEnd {
            condition: DeadEndCondition::FailureAtZero,
            core: None,
            detail: String::new(),
            action: None,
        }
        .into(),
    ];
    for stop in stops {
        let mut j = stopped_journal(stop);
        assert_eq!(j.state.forecast(), Forecast::default());
        j.append(warning(), &[]);
        assert_eq!(j.state.forecast(), Forecast::default());
        assert_eq!(fresh(&j.events).forecast(), Forecast::default());
    }
}

#[test]
fn forecast_resumes_after_a_new_config_following_shutdown() {
    let mut j = stopped_journal(
        Shutdown {
            reason: ShutdownReason::Signal,
            kernel_cursor: None,
            kernel_error: None,
            cycles: None,
        }
        .into(),
    );
    j.load(&togi_config::Config::default());
    assert!(j.state.forecast().between.is_some() || !j.state.forecast().branches.is_empty());
}

#[test]
fn hunt_probe_without_recorded_checking_profile_forecast() {
    let j = crate::dashboard_tests::probe_without_checking_profile();
    let forecast = j.state.forecast();
    let all = branch(&forecast, Premise::AllPass);
    assert_eq!(all.passes, 5);
    assert_eq!(all.all_pass, Some(AllPass::Complete));
    assert_eq!(all.uncertainty, Some(Uncertainty::NeedsHistory));
    assert!(all.next.is_none());
}

#[test]
fn forecast_all_pass_that_stops_early_keeps_the_passes_it_folded() {
    let mut j = room(&[-20; 4]);
    j.cycle_start(1, &[Regime::R7]);
    let decision = j.next_decision();
    j.decide(decision);
    let plan = j.r7_plan(&[0, 1], &[-20; 4]);
    j.intent(&plan);
    let forecast = j.state.forecast();
    let all = branch(&forecast, Premise::AllPass);
    assert_eq!(all.passes, 1);
    assert_eq!(
        all.all_pass,
        Some(AllPass::Stopped(Uncertainty::NeedsRanking))
    );
    assert_eq!(all.uncertainty, Some(Uncertainty::NeedsRanking));
    assert!(all.next.is_none());
}

#[test]
fn forecast_leaves_state_unchanged() {
    let mut j = partial_in_flight();
    start(&mut j);
    let before: State = j.state.clone();
    let events = j.events.len();
    let first = j.state.forecast();
    assert_eq!(j.state.forecast(), first);
    assert_eq!(j.events.len(), events);
    assert_eq!(j.state.next().ok(), before.next().ok());
    assert_eq!(j.state, before);
    assert_eq!(j.state.project(), before.project());
}

#[test]
fn forecast_detects_recurring_hunt_plan() {
    use crate::tests::off;
    use std::num::NonZeroU32;
    use togi_journal::{Activity, HuntGroup, HuntStage, HuntStart, Member, Seq};
    use togi_machine::{Condition, workloads};

    let mut j = Journal::new(
        &[0, 1, 2],
        &[
            Start::has_room(-20),
            Start::at_limit(-30),
            Start::has_room(-10),
        ],
    );
    j.profile_change(&[-20, -30, -10]);
    j.append(
        HuntStart {
            hunt: NonZeroU32::MIN,
            failure: Seq::new(1).unwrap(),
            trial: None,
            regime: Regime::R6,
            workload: workloads(Regime::R6)[0].id(),
            cores: set(&[0]),
            failing: j.profile(&[-20, -30, -10]).offsets(),
            parked: j.profile(&[-10, -30, -5]).offsets(),
            parked_seq: None,
            candidates: set(&[0, 2]),
            trials: 5,
            trial_s: 120,
            duration_s: 120,
            miss: 0.0,
            rate: 0.0,
            ranking: vec![core(0), core(1), core(2)],
            reason: None,
        }
        .into(),
        &[],
    );
    j.append(
        HuntGroup {
            hunt: NonZeroU32::MIN,
            group: NonZeroU32::MIN,
            cores: set(&[2]),
            profile: j.profile(&[-15, -30, -5]).offsets(),
            set: togi_machine::CoreSet::default(),
            granularity: 0,
            stage: HuntStage::Probe,
            index: 0,
            duration_s: 120,
            escalated: false,
            full_checked: false,
            any_failed: false,
            probe: Some(Member {
                core: core(0),
                offset: off(-15),
            }),
            held: vec![Member {
                core: core(2),
                offset: off(-5),
            }],
            inferred: None,
            skipped: false,
            reason: None,
        }
        .into(),
        &[],
    );
    let mut plan = j.manual(
        Regime::R6,
        0,
        Load::Many(set(&[0])),
        Condition::Parked,
        &[-15, -30, -5],
        120,
    );
    plan.context = TrialContext::Hunt {
        hunt: NonZeroU32::MIN,
        group: NonZeroU32::MIN,
    };
    plan.activity = Activity::Hunt;
    j.intent(&plan);
    let before = j.state.clone();
    let forecast = j.state.forecast();
    assert!(
        forecast.branches.iter().any(|branch| {
            matches!(
                branch.uncertainty,
                Some(Uncertainty::NeedsHistory | Uncertainty::RecurringPlan)
            ) && branch.next.is_none()
        }),
        "{forecast:#?}"
    );
    assert_eq!(j.state, before);
}

#[test]
fn stored_requirement_survives_crash_and_changed_config_forecast() {
    let mut j = Journal::new(&[0], &[Start::search(-20).checking()]);
    let needed = u32::from(j.run().config.needed.get());
    for _ in 0..needed - 1 {
        let plan = j.next_trial();
        trial_pass(&mut j, &plan);
    }
    let plan = j.next_trial();
    let (intent, _) = j.intent(&plan);
    let before = j.state.requirement().unwrap();
    j.load(&crate::testkit::config(
        r#"{"evidence":{"miss":0.001},"durations":{"search_trial_s":61}}"#,
    ));
    assert_eq!(j.state.requirement().unwrap(), before);
    let forecast = j.state.forecast();
    assert!(!forecast.branches.is_empty());
    assert!(
        !forecast
            .branches
            .iter()
            .any(|branch| branch.premise == Premise::AllPass)
    );
    j.append(
        togi_journal::CrashDetected {
            previous_boot: togi_machine::BootId::from("previous".to_owned()),
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
    let crashed = j.state.forecast();
    assert!(crashed.branches.is_empty());
    assert!(crashed.between.is_some());
    assert_eq!(fresh(&j.events).forecast(), crashed);
}

#[test]
fn forecast_folds_the_recorded_ranking_again_when_a_later_plan_left_it_stale() {
    let ids = [0, 1, 2, 3];
    let starts = vec![Start::at_limit(-30).fail(-31); 4];
    let mut j = Journal::on(&ids, topology(4), &starts);
    j.profile_change(&[-30; 4]);
    j.ranking(&ids);
    let round = j.round_start(1, &[-30; 4], &[0], 1);
    j.round_end(1, round);
    let seconds = j.run().config.checking_idle.get();
    let plan = j.manual(
        Regime::R6,
        0,
        Load::Many(set(&ids)),
        togi_machine::Condition::Together,
        &[-30; 4],
        seconds,
    );
    j.intent(&plan);
    assert!(j.run().ranking_stale());
    let forecast = j.state.forecast();
    let unnamed = branch(&forecast, Premise::Unnamed);
    assert_eq!(unnamed.uncertainty, None, "{unnamed:?}");
    assert!(matches!(
        unnamed.decisions.as_slice(),
        [
            DecisionPayload::Failure(_),
            DecisionPayload::HuntStart(_),
            DecisionPayload::HuntGroup(_)
        ]
    ));
    assert_eq!(fresh(&j.events).forecast(), forecast);
}
