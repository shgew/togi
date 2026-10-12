use std::num::{NonZeroU16, NonZeroU32};

use togi_journal::{Decision, Outcome, Seq};
use togi_machine::{Backend, Condition, Regime, workloads};

use crate::schedule_tests::decision;
use crate::testkit::{
    Journal, MPRIME_NEW, MPRIME_OLD, Start, YCRUNCHER_NEW, YCRUNCHER_OLD, backends, config, fresh,
    set,
};
use crate::tests::core;
use crate::{
    Action, ClassKey, DecisionAction, DecisionPayload, FoldError, Load, Secs, TrialContext,
    TrialHistory, TrialPlan, Window,
};

type Reload = &'static [(&'static str, &'static str)];

const ANY: Window = Window::LiveOrCarried { since: None };

fn class(regime: Regime, workload: usize, cores: &[u8], duration: u32) -> ClassKey {
    ClassKey::new(
        togi_machine::WorkloadId::new(regime, workload).unwrap(),
        set(cores),
        Secs::new(NonZeroU32::new(duration).unwrap()),
    )
}

fn needed(j: &Journal) -> NonZeroU16 {
    j.run().config.needed
}

fn valid(j: &Journal, class: ClassKey, profile: &[i64]) -> u32 {
    let p = j.profile(profile);
    j.run()
        .evidence
        .count(class, &p, ANY, needed(j))
        .unwrap()
        .valid
}

fn failing(j: &Journal, class: ClassKey, profile: &[i64], since: Option<Seq>) -> Option<Seq> {
    let p = j.profile(profile);
    j.run()
        .evidence
        .failing_seq(class, &p, since, needed(j))
        .unwrap()
}

fn count(j: &Journal) -> usize {
    usize::from(needed(j).get())
}

fn decide_pending(j: &mut Journal) -> Seq {
    let action = j.next_decision();
    j.decide(action)
}

#[test]
fn evidence_validity() {
    let mut j = Journal::new(
        &[0, 1],
        &[
            Start::at_limit(-20).fail(-21),
            Start::at_limit(-10).fail(-11),
        ],
    );
    let k = class(Regime::R6, 0, &[0, 1], 120);
    let rows: [(&[i64], Outcome); 5] = [
        (&[-20, -10], Outcome::Pass),
        (&[-19, -10], Outcome::Pass),
        (&[-20, -10], Outcome::Failure),
        (&[-22, -10], Outcome::Pass),
        (&[-20, -12], Outcome::Pass),
    ];
    let mut ends = Vec::new();
    for (profile, outcome) in rows {
        ends.push(j.carry(
            SESSION,
            (Regime::R6, 0, &[0, 1], 120),
            Condition::Parked,
            profile,
            outcome,
        ));
    }
    assert_eq!(valid(&j, k, &[-19, -10]), 4);
    assert_eq!(valid(&j, k, &[-21, -10]), 1);
    assert_eq!(valid(&j, k, &[-20, -11]), 1);
    assert!(failing(&j, k, &[-20, -10], Some(ends[1])).is_some());

    let idle_class = class(Regime::R6, 0, &[0, 1], 900);
    let profile = j.profile(&[-20, -10]).offsets();
    let idle = j.idle_failure(profile, Condition::Together);
    assert!(failing(&j, idle_class, &[-20, -10], None).is_some());
    let parked = j.idle_failure(profile, Condition::Parked);
    assert!(failing(&j, idle_class, &[-20, -10], Some(idle)).is_some());
    let queued: Vec<Seq> = j.run().failures.queue().collect();
    assert!(queued.ends_with(&[idle, parked]));
    assert_eq!(
        j.run().failures.pending(parked).unwrap().class.regime(),
        Regime::R6
    );
    j.assert_resumes();
}

#[test]
fn monotonicity_warning() {
    let mut j = Journal::new(&[0], &[Start::search(-10).checking()]);
    let plan = j.next_trial();
    let expected: Vec<Seq> = (0..count(&j))
        .map(|_| j.trial(&plan, Outcome::Pass))
        .collect();
    let failed = j.trial(&plan, Outcome::Failure);
    let attribution = j.next_decision();
    let DecisionPayload::Failure(f) = &attribution.payload else {
        panic!("expected a failure");
    };
    assert_eq!(f.attribution, togi_journal::Attribution::Attributed);
    j.decide(attribution);
    let warning = j.next_decision();
    let DecisionPayload::TunerWarning(w) = &warning.payload else {
        panic!("expected a warning, got {warning:?}");
    };
    assert_eq!(w.passes, expected);
    assert_eq!(warning.cause, [failed]);
    j.decide(warning);
    assert!(j.run().warning.is_none());
    assert!(j.warning().is_none());
}

type IdleCase = (&'static str, [i64; 2], &'static [u32], &'static [u8], bool);

#[test]
fn idle_monotonicity_warning() {
    let cases: [IdleCase; 5] = [
        ("equal profile", [-10, -12], &[120], &[0, 1], true),
        ("deeper passing profile", [-11, -13], &[900], &[0, 1], true),
        (
            "shallower passing profile",
            [-9, -11],
            &[120],
            &[0, 1],
            false,
        ),
        (
            "durations cannot pool",
            [-10, -12],
            &[120, 900],
            &[0, 1],
            false,
        ),
        (
            "partial load cannot establish the class",
            [-10, -12],
            &[120],
            &[0],
            false,
        ),
    ];
    for (name, pass_profile, durations, cores, warns) in cases {
        let mut j = Journal::has_room(&[-10, -12]);
        let seqs: Vec<Seq> = (0..count(&j))
            .map(|i| {
                j.carry(
                    SESSION,
                    (Regime::R6, 0, cores, durations[i % durations.len()]),
                    Condition::Together,
                    &pass_profile,
                    Outcome::Pass,
                )
            })
            .collect();
        let failure = j.idle_failure(j.profile(&[-10, -12]).offsets(), Condition::Together);
        match (j.warning(), warns) {
            (Some(warning), true) => {
                assert_eq!(warning.passes, seqs, "{name}");
                let action = j.next_decision();
                let mut cause = vec![failure];
                cause.extend(seqs.iter().copied());
                assert_eq!(action.cause, cause, "{name}");
                j.decide(action);
            }
            (None, false) => {}
            (found, _) => panic!("{name}: warning {found:?}, expected {warns}"),
        }
        assert_eq!(j.run().failures.queue().last(), Some(failure), "{name}");
    }
}

#[test]
fn idle_monotonicity_chooses_earliest_passing_class() {
    let mut j = Journal::has_room(&[-10, -12]);
    let mut expected = Vec::new();
    for duration in [900, 120] {
        for i in 0..=count(&j) {
            let end = j.carry(
                SESSION,
                (Regime::R6, 0, &[0, 1], duration),
                Condition::Together,
                &[-10, -12],
                Outcome::Pass,
            );
            if duration == 900 && i < count(&j) {
                expected.push(end);
            }
        }
    }
    let failure = j.idle_failure(j.profile(&[-10, -12]).offsets(), Condition::Together);
    let action = j.next_decision();
    let DecisionPayload::TunerWarning(w) = &action.payload else {
        panic!("no warning: {action:?}");
    };
    assert_eq!(w.passes, expected);
    assert_eq!(action.cause[0], failure);
    assert_eq!(fresh(&j.events).next().ok(), Some(Action::Decide(action)));
}

#[test]
fn stored_requirement_survives_crash_and_changed_config() {
    let mut j = Journal::new(&[0], &[Start::search(-20).checking()]);
    let n = u32::from(needed(&j).get());
    for _ in 0..n - 1 {
        let plan = j.next_trial();
        j.trial(&plan, Outcome::Pass);
    }
    let plan = j.next_trial();
    let (intent, trial) = j.intent(&plan);
    assert_eq!(
        j.run().flight.open.as_ref().unwrap().requirement,
        plan.requirement
    );
    j.load(&config(
        r#"{"evidence":{"miss":0.001},"durations":{"search_trial_s":91}}"#,
    ));
    assert_eq!(
        j.run().flight.open.as_ref().unwrap().requirement,
        plan.requirement
    );
    assert_ne!(u32::from(needed(&j).get()), n);
    j.append(
        togi_journal::CrashDetected {
            previous_boot: togi_machine::BootId::from("boot".to_owned()),
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
    j.end(intent, &trial, Outcome::Failure);
    let want = TrialHistory {
        passed: n - 1,
        trial: n,
        needed: n,
        step: None,
    };
    let closed = j.run().flight.last_closed.clone().unwrap();
    assert_eq!((closed.trial, closed.history), (trial, want));
    j.assert_resumes();
    j.reset(0);
    assert_eq!(j.run().flight.last_closed.as_ref().unwrap().history, want);
}

#[test]
fn requirement_trial_number_is_bounded() {
    let mut j = Journal::new(&[0], &[Start::search(-20).checking()]);
    let n = u32::from(needed(&j).get());
    for _ in 0..n - 1 {
        let plan = j.next_trial();
        j.trial(&plan, Outcome::Pass);
    }
    let plan = j.next_trial();
    for _ in 0..3 {
        j.trial(&plan, Outcome::Pass);
    }
    let history = j.run().flight.last_closed.as_ref().unwrap().history;
    assert_eq!((history.passed, history.trial, history.needed), (n, n, n));
}

#[test]
fn a_search_trial_needs_one_pass_and_a_check_needs_n() {
    let search = Journal::search_at(&[-20]);
    let plan = search.next_trial();
    assert_eq!(plan.requirement.needed.get(), 1);
    assert_eq!(plan.requirement.window, Window::Live { since: None });
    assert!(matches!(plan.context, TrialContext::Search { .. }));
    let check = Journal::new(&[0], &[Start::search(-20).checking()]);
    let plan = check.next_trial();
    assert_eq!(plan.requirement.needed, needed(&check));
    assert!(matches!(
        plan.requirement.window,
        Window::LiveOrCarried { since: Some(_) }
    ));
}

#[test]
fn intents_that_match_no_open_context_are_unscheduled() {
    type Change = fn(&mut TrialPlan);
    let cases: [(&str, Change); 7] = [
        ("together trial", |plan| {
            plan.condition = Condition::Together;
            plan.activity = togi_journal::Activity::Checking;
        }),
        ("hunt", |plan| {
            plan.context = TrialContext::Hunt {
                hunt: NonZeroU32::MIN,
                group: NonZeroU32::MIN,
            };
        }),
        ("round", |plan| {
            plan.context = TrialContext::Round {
                round: NonZeroU32::MIN,
            };
        }),
        ("offset the core is not at", |plan| {
            plan.profile = plan.profile.with(core(0), crate::tests::off(-11));
        }),
        ("duration the search does not run", |plan| {
            plan.duration = Secs::new(NonZeroU32::new(91).unwrap());
        }),
        ("workload the rotation does not name", |plan| {
            plan.workload = crate::WorkloadChoice::Frozen(workloads(Regime::R1)[1].id());
        }),
        ("R2 before the step's R1 passed", |plan| {
            plan.regime = Regime::R2;
            plan.workload = crate::WorkloadChoice::Rotate;
        }),
    ];
    for (name, change) in cases {
        let mut j = Journal::search_at(&[-10]);
        let mut plan = j.next_trial();
        change(&mut plan);
        let intent = plan.intent(togi_journal::TrialId::from("0001".to_owned()), 0);
        let error = j.try_append(intent.into(), &[]);
        assert!(
            matches!(error, Err(FoldError::UnscheduledIntent(_))),
            "{name}: {error:?}"
        );
    }
    let mut j = Journal::new(&[0], &[Start::at_limit(-10)]);
    let plan = j.manual(
        Regime::R1,
        0,
        Load::Single(core(0)),
        Condition::Alone,
        &[-10],
        90,
    );
    let intent = plan.intent(togi_journal::TrialId::from("0001".to_owned()), 0);
    assert!(matches!(
        j.try_append(intent.into(), &[]),
        Err(FoldError::UnscheduledIntent(_))
    ));
}

#[test]
fn retry_requirement_matches_fold() {
    let mut j = Journal::search_at(&[-10]);
    let first = j.next_trial();
    j.trial(&first, Outcome::Inconclusive);
    let retry = j.next_trial();
    assert!(retry.retry);
    j.intent(&retry);
    assert_eq!(
        j.run().flight.open.as_ref().unwrap().requirement,
        retry.requirement
    );
}

#[test]
fn resumed_requirement_change_drops_retry() {
    let cases: [(&str, &str, bool, u32); 2] = [
        ("unchanged", "{}", true, 90),
        (
            "search_trial_s",
            r#"{"durations":{"search_trial_s":60}}"#,
            false,
            60,
        ),
    ];
    for (name, json, retry, duration) in cases {
        let mut j = Journal::search_at(&[-10]);
        let first = j.next_trial();
        j.trial(&first, Outcome::Inconclusive);
        j.load(&config(json));
        let plan = j.next_trial();
        assert_eq!(
            (plan.retry, plan.duration.get(), plan.regime),
            (retry, duration, Regime::R1),
            "{name}"
        );
        j.assert_resumes();
    }
    let mut j = Journal::new(&[0], &[Start::search(-20).checking()]);
    let first = j.next_trial();
    j.trial(&first, Outcome::Inconclusive);
    j.load(&config(r#"{"evidence":{"miss":0.001}}"#));
    assert!(!j.next_trial().retry);
}

#[test]
fn search_retry_belongs_to_its_turn() {
    let mut j = Journal::search_at(&[-10, -10]);
    let first = j.next_trial();
    j.trial(&first, Outcome::Inconclusive);
    let retry = j.next_trial();
    assert!(retry.retry);
    assert_eq!((retry.load, retry.regime), (first.load, first.regime));
    let other = match first.load {
        Load::Single(c) if c == core(0) => 1,
        _ => 0,
    };
    j.reset(other);
    let seen = j.run_until(
        10,
        |_| Outcome::Pass,
        |action| matches!(action, Action::RunTrial(_)),
    );
    let Some(Action::RunTrial(plan)) = seen.last() else {
        panic!("no trial after the reset");
    };
    assert!(!plan.retry);
}

const SESSION: &str = "20261002T004254Z";

fn carry_passes(
    j: &mut Journal,
    class: (Regime, usize, &[u8], u32),
    profile: &[i64],
    times: usize,
    outcome: Outcome,
) -> Vec<Seq> {
    let condition = if class.0 == Regime::R6 {
        Condition::Together
    } else {
        Condition::Alone
    };
    (0..times)
        .map(|_| j.carry(SESSION, class, condition, profile, outcome))
        .collect()
}

fn check_decision(j: &mut Journal) -> Seq {
    j.decision(togi_journal::TunerDecision {
        core: core(0),
        activity: togi_journal::Activity::Search,
        decision: Decision::CheckSoloLimit,
        from_offset: crate::tests::off(-20),
        to_offset: crate::tests::off(-20),
        pass: None,
        failure_point: None,
        workloads: vec![workloads(Regime::R1)[0].id(), workloads(Regime::R2)[0].id()],
        reason: "test".to_owned(),
    })
}

#[test]
fn carried_evidence_answers_a_candidate_solo_limit_but_not_an_ordinary_step() {
    let mut j = Journal::search_at(&[-20]);
    let n = count(&j);
    let mut facts = Vec::new();
    for regime in [Regime::R1, Regime::R2] {
        facts.extend(carry_passes(
            &mut j,
            (regime, 0, &[0], 90),
            &[-20],
            n,
            Outcome::Pass,
        ));
    }
    let ordinary = j.next_trial();
    assert_eq!(ordinary.regime, Regime::R1);
    assert!(matches!(ordinary.context, TrialContext::Search { .. }));
    let boundary = check_decision(&mut j);
    let exit = j.next_decision();
    let DecisionPayload::CorePhase(phase) = &exit.payload else {
        panic!("carried candidate solo limit requires a live trial: {exit:?}");
    };
    assert_eq!(phase.to, togi_journal::CorePhase::HasRoom);
    let mut cause = vec![boundary];
    cause.extend(facts);
    assert_eq!(exit.cause, cause);
    assert!(
        phase.reason.contains("carried") && phase.reason.contains(SESSION),
        "{}",
        phase.reason
    );
}

#[test]
fn carried_failure_invalidation() {
    for idle in [false, true] {
        let mut j = Journal::new(&[0], &[Start::search(-20).checking()]);
        let n = count(&j);
        let (regime, duration, condition) = if idle {
            (Regime::R6, 900, Condition::Together)
        } else {
            (Regime::R1, 90, Condition::Alone)
        };
        let k = class(regime, 0, &[0], duration);
        for _ in 0..n {
            j.carry(
                SESSION,
                (regime, 0, &[0], duration),
                condition,
                &[-20],
                Outcome::Pass,
            );
        }
        let failure = if idle {
            j.carry_idle(SESSION, &[-19])
        } else {
            j.carry(
                SESSION,
                (regime, 0, &[0], duration),
                condition,
                &[-19],
                Outcome::Failure,
            )
        };
        let boundary = j.profile_change(&[-20]);
        let since = Window::LiveOrCarried {
            since: Some(boundary),
        };
        let p = j.profile(&[-20]);
        let window = |j: &Journal| {
            j.run()
                .evidence
                .count(k, &p, since, needed(j))
                .unwrap()
                .valid
        };
        assert_eq!(window(&j), 0, "idle={idle}");
        assert_eq!(
            failing(&j, k, &[-20], Some(boundary)),
            Some(failure),
            "idle={idle}"
        );
        let plan = j.manual(
            regime,
            0,
            Load::Single(core(0)),
            Condition::Alone,
            &[-20],
            duration,
        );
        for i in 0..n {
            if idle {
                j.carry(
                    SESSION,
                    (regime, 0, &[0], duration),
                    condition,
                    &[-20],
                    Outcome::Pass,
                );
            } else {
                j.trial(&plan, Outcome::Pass);
            }
            assert_eq!(
                failing(&j, k, &[-20], Some(boundary)).is_some(),
                i + 1 < n,
                "idle={idle} after {}",
                i + 1
            );
        }
        assert_eq!(window(&j), u32::try_from(n).unwrap(), "idle={idle}");
    }
}

#[test]
fn reset_clears_carried_core_evidence() {
    for outcome in [Outcome::Pass, Outcome::Failure] {
        let mut j = Journal::new(&[0, 1], &[Start::at_limit(-20), Start::at_limit(-20)]);
        let duration = j.run().config.short_trial.get();
        let n = count(&j);
        let mut carried = Vec::new();
        for id in [0_u8, 1] {
            let mut p = [0, 0];
            p[usize::from(id)] = -20;
            carried.push(carry_passes(
                &mut j,
                (Regime::R1, 0, &[id], duration),
                &p,
                n,
                outcome,
            ));
        }
        j.reset(0);
        for id in [0_u8, 1] {
            let mut p = [0, 0];
            p[usize::from(id)] = -20;
            let k = class(Regime::R1, 0, &[id], duration);
            let kept = if outcome == Outcome::Pass {
                valid(&j, k, &p) > 0
            } else {
                failing(&j, k, &p, None).is_some()
            };
            assert_eq!(kept, id == 1, "{outcome:?} core {id}");
        }
        for seq in &carried[0] {
            assert!(j.run().evidence.carried_source(*seq).is_none());
        }
        for seq in &carried[1] {
            assert!(j.run().evidence.carried_source(*seq).is_some());
        }
    }
}

#[test]
fn carried_solo_limit_eligibility() {
    let cases: [(&str, i64, u32, usize, bool); 4] = [
        ("shallower profile", -19, 90, 0, false),
        ("different duration", -20, 120, 0, false),
        ("different frozen workload", -20, 90, 1, false),
        ("intervening live failure", -20, 90, 0, true),
    ];
    for (name, profile, duration, workload, live_failure) in cases {
        let mut j = Journal::search_at(&[-20]);
        let n = count(&j);
        for regime in [Regime::R1, Regime::R2] {
            for _ in 0..n {
                j.carry(
                    SESSION,
                    (regime, workload, &[0], duration),
                    Condition::Alone,
                    &[profile],
                    Outcome::Pass,
                );
            }
        }
        if live_failure {
            let plan = j.next_trial();
            j.trial(&plan, Outcome::Failure);
        }
        check_decision(&mut j);
        let action = j.run().search_action().unwrap();
        let Action::RunTrial(plan) = action else {
            panic!("{name}: ineligible carried evidence answered the check: {action:?}");
        };
        assert_eq!(plan.regime, Regime::R1, "{name}");
    }
}

#[test]
fn carried_pass_monotonicity_warning() {
    let mut j = Journal::search_at(&[-19]);
    let n = count(&j);
    let facts = carry_passes(&mut j, (Regime::R1, 0, &[0], 90), &[-20], n, Outcome::Pass);
    let plan = j.next_trial();
    assert_eq!(plan.profile, j.profile(&[-19]));
    assert_eq!(plan.condition, Condition::Alone);
    assert_eq!(plan.requirement.class, class(Regime::R1, 0, &[0], 90));
    let failure = j.trial(&plan, Outcome::Failure);
    decide_pending(&mut j);
    let action = j.next_decision();
    let DecisionPayload::TunerWarning(w) = &action.payload else {
        panic!("failure did not contradict carried trials: {action:?}");
    };
    let mut cause = vec![failure];
    cause.extend(facts.iter().copied());
    assert_eq!(action.cause, cause);
    assert_eq!(w.passes, facts);
    assert!(w.detail.contains(SESSION), "{}", w.detail);
    assert_eq!(j.state.drain(), Some(action.clone()));
    assert_eq!(j.state.next().ok(), Some(Action::Decide(action)));
}

#[test]
fn reset_clears_carried_idle_failure() {
    let mut j = Journal::new(&[0, 1], &[Start::at_limit(-20), Start::at_limit(-20)]);
    let failure = j.carry_idle(SESSION, &[-20, -20]);
    let k = class(Regime::R6, 0, &[0, 1], 900);
    let later = Seq::new(failure.get() + 1);
    assert!(failing(&j, k, &[-20, -20], later).is_some());
    j.reset(0);
    assert!(failing(&j, k, &[-20, -20], None).is_none());
    assert!(j.run().failures.pending(failure).is_none());
    assert!(j.run().failures.witnessed(failure).is_none());
}

#[test]
fn carried_passes_do_not_add_session_exposure() {
    for condition in [Condition::Together, Condition::Parked] {
        let mut j = Journal::new(&[0], &[Start::search(-10).checking()]);
        let workload = workloads(Regime::R1)[0].id();
        j.carry(
            SESSION,
            (Regime::R1, 0, &[0], 90),
            condition,
            &[-10],
            Outcome::Pass,
        );
        let boundary = j.profile_change(&[-10]);
        let k = class(Regime::R1, 0, &[0], 90);
        let p = j.profile(&[-10]);
        let since = Window::LiveOrCarried {
            since: Some(boundary),
        };
        assert_eq!(
            j.run()
                .evidence
                .count(k, &p, since, needed(&j))
                .unwrap()
                .valid,
            1
        );
        assert_eq!(j.run().evidence.exposure(workload), 0);
        let plan = j.manual(
            Regime::R1,
            0,
            Load::Single(core(0)),
            Condition::Alone,
            &[-10],
            90,
        );
        j.trial(&plan, Outcome::Pass);
        assert_eq!(j.run().evidence.exposure(workload), 0);
    }
}

#[test]
fn carried_failure_survives_newer_pre_boundary_live_failure() {
    for idle in [false, true] {
        let mut j = Journal::new(&[0], &[Start::search(-20).checking()]);
        let n = count(&j);
        let (regime, duration) = if idle {
            (Regime::R6, 900)
        } else {
            (Regime::R1, 90)
        };
        let k = class(regime, 0, &[0], duration);
        let plan = j.manual(
            Regime::R1,
            0,
            Load::Single(core(0)),
            Condition::Alone,
            &[-20],
            90,
        );
        let carried = if idle {
            j.carry_idle(SESSION, &[-19])
        } else {
            j.carry(
                SESSION,
                (regime, 0, &[0], duration),
                Condition::Alone,
                &[-19],
                Outcome::Failure,
            )
        };
        if idle {
            j.idle_failure(j.profile(&[-20]).offsets(), Condition::Together);
        } else {
            j.trial(&plan, Outcome::Failure);
            decide_pending(&mut j);
        }
        let boundary = j.profile_change(&[-20]);
        for i in 0..n {
            assert_eq!(
                failing(&j, k, &[-20], Some(boundary)),
                Some(carried),
                "idle={idle} after {i}"
            );
            if idle {
                j.carry(
                    SESSION,
                    (regime, 0, &[0], duration),
                    Condition::Together,
                    &[-20],
                    Outcome::Pass,
                );
            } else {
                j.trial(&plan, Outcome::Pass);
            }
        }
        assert_eq!(failing(&j, k, &[-20], Some(boundary)), None, "idle={idle}");
    }
}

#[test]
fn evidence_is_keyed_by_backend_store_path() {
    assert_eq!(workloads(Regime::R1)[0].backend, Backend::Mprime);
    assert_eq!(workloads(Regime::R1)[1].backend, Backend::Ycruncher);
    let cases: [(&str, Reload, u32, u32, bool); 5] = [
        (
            "resume with the same backends",
            &[(MPRIME_OLD, YCRUNCHER_OLD)],
            10,
            10,
            false,
        ),
        (
            "mprime updated",
            &[(MPRIME_NEW, YCRUNCHER_OLD)],
            0,
            10,
            true,
        ),
        (
            "y-cruncher updated",
            &[(MPRIME_OLD, YCRUNCHER_NEW)],
            10,
            0,
            false,
        ),
        ("both updated", &[(MPRIME_NEW, YCRUNCHER_NEW)], 0, 0, true),
        (
            "mprime rolled back",
            &[(MPRIME_NEW, YCRUNCHER_OLD), (MPRIME_OLD, YCRUNCHER_OLD)],
            10,
            10,
            false,
        ),
    ];
    for (name, reload, mprime, ycruncher, failure_valid) in cases {
        let mut j = Journal::has_room(&[-10, -10]);
        j.load(&backends(MPRIME_OLD, YCRUNCHER_OLD));
        let profile = [-10, -10];
        let on = |j: &mut Journal, workload: usize, backend: Backend, path: &str, outcome| {
            j.carry_under(
                SESSION,
                (Regime::R1, workload, &[0], 90),
                Condition::Together,
                &profile,
                outcome,
                Some((backend, path)),
            )
        };
        on(&mut j, 0, Backend::Mprime, MPRIME_OLD, Outcome::Failure);
        for _ in 0..2 * count(&j) {
            on(&mut j, 0, Backend::Mprime, MPRIME_OLD, Outcome::Pass);
            on(&mut j, 1, Backend::Ycruncher, YCRUNCHER_OLD, Outcome::Pass);
        }
        let km = class(Regime::R1, 0, &[0], 90);
        let ky = class(Regime::R1, 1, &[0], 90);
        assert!(failing(&j, km, &profile, None).is_none(), "{name}");
        for (m, y) in reload {
            j.load(&backends(m, y));
        }
        let counts = |j: &Journal| (valid(j, km, &profile), valid(j, ky, &profile));
        let seen = counts(&j);
        assert_eq!(seen, (mprime, ycruncher), "{name}");
        assert_eq!(counts(&j.resumed()), seen, "{name} replayed");
        assert_eq!(
            failing(&j, km, &profile, None).is_some(),
            failure_valid,
            "{name}"
        );
    }
}

#[test]
fn a_backend_update_restarts_carried_solo_limit_evidence() {
    assert_eq!(workloads(Regime::R1)[0].backend, Backend::Mprime);
    assert_eq!(workloads(Regime::R2)[0].backend, Backend::Mprime);
    let cases: [(&str, &str, &str, bool); 3] = [
        (
            "same backends answer the check",
            MPRIME_OLD,
            YCRUNCHER_OLD,
            true,
        ),
        (
            "other backend updated still answers",
            MPRIME_OLD,
            YCRUNCHER_NEW,
            true,
        ),
        (
            "own backend updated needs live trials",
            MPRIME_NEW,
            YCRUNCHER_OLD,
            false,
        ),
    ];
    for (name, mprime, ycruncher, answered) in cases {
        let mut j = Journal::search_at(&[-20]);
        j.load(&backends(MPRIME_OLD, YCRUNCHER_OLD));
        let n = count(&j);
        for regime in [Regime::R1, Regime::R2] {
            for _ in 0..n {
                j.carry_under(
                    SESSION,
                    (regime, 0, &[0], 90),
                    Condition::Alone,
                    &[-20],
                    Outcome::Pass,
                    Some((Backend::Mprime, MPRIME_OLD)),
                );
            }
        }
        j.load(&backends(mprime, ycruncher));
        check_decision(&mut j);
        let action = j.run().search_action().unwrap();
        if answered {
            let Action::Decide(DecisionAction {
                payload: DecisionPayload::CorePhase(p),
                ..
            }) = action
            else {
                panic!(
                    "{name}: carried passes under the recorded backends did not answer the check: {action:?}"
                );
            };
            assert_eq!(p.to, togi_journal::CorePhase::HasRoom, "{name}");
        } else {
            let Action::RunTrial(plan) = action else {
                panic!("{name}: the check was answered by passes from the previous mprime");
            };
            assert_eq!(plan.regime, Regime::R1, "{name}");
            assert_eq!(
                plan.workload,
                crate::WorkloadChoice::Frozen(workloads(Regime::R1)[0].id()),
                "{name}"
            );
        }
    }
}

#[test]
fn a_backend_update_restarts_an_unfinished_search_step() {
    let cases: [(&str, Reload, bool, Reload); 5] = [
        (
            "same backends keep the R1 pass",
            &[(MPRIME_OLD, YCRUNCHER_OLD)],
            false,
            &[],
        ),
        (
            "other backend updated keeps the R1 pass",
            &[(MPRIME_OLD, YCRUNCHER_NEW)],
            false,
            &[],
        ),
        (
            "own backend updated reruns R1",
            &[(MPRIME_NEW, YCRUNCHER_OLD)],
            true,
            &[],
        ),
        (
            "own backend rolled back restores the R1 pass",
            &[(MPRIME_NEW, YCRUNCHER_OLD), (MPRIME_OLD, YCRUNCHER_OLD)],
            false,
            &[],
        ),
        (
            "rollback after the replacement R1 still needs R2",
            &[(MPRIME_NEW, YCRUNCHER_OLD)],
            true,
            &[(MPRIME_OLD, YCRUNCHER_OLD)],
        ),
    ];
    for (name, reload, rerun, rollback) in cases {
        let mut j = Journal::search_at(&[-10]);
        j.load(&backends(MPRIME_OLD, YCRUNCHER_OLD));
        let first = j.next_trial();
        assert_eq!(first.regime, Regime::R1, "{name}");
        let r1 = j.trial(&first, Outcome::Pass);
        for (m, y) in reload {
            j.load(&backends(m, y));
        }
        let assert_replays = |j: &Journal| {
            let action = j.next();
            assert_eq!(fresh(&j.events).next().ok(), Some(action.clone()), "{name}");
            action
        };
        let mut action = assert_replays(&j);
        let mut want = vec![r1];
        if rerun {
            let Action::RunTrial(again) = &action else {
                panic!("{name}: the step after the update continues with {action:?}");
            };
            assert_eq!(again.regime, Regime::R1, "{name}");
            let replacement = j.trial(&again.clone(), Outcome::Pass);
            want = vec![replacement];
            if !rollback.is_empty() {
                for (m, y) in rollback {
                    j.load(&backends(m, y));
                }
                want = vec![r1];
            }
            action = assert_replays(&j);
        }
        let Action::RunTrial(second) = &action else {
            panic!("{name}: step continues with {action:?}, want R2");
        };
        assert_eq!(second.regime, Regime::R2, "{name}");
        let r2 = j.trial(&second.clone(), Outcome::Pass);
        let step = j.next_decision();
        assert_eq!(decision(&step).decision, Decision::StepDeeper, "{name}");
        want.push(r2);
        assert_eq!(step.cause, want, "{name}");
        j.assert_resumes();
    }
}

#[test]
fn a_known_alone_failure_backs_off_from_its_recorded_offset() {
    let mut j = Journal::search_at(&[-20]);
    let fact = j.carry(
        SESSION,
        (Regime::R1, 0, &[0], 90),
        Condition::Alone,
        &[-19],
        Outcome::Failure,
    );
    let skip = j.next_decision();
    assert_eq!(skip.cause, [fact]);
    j.decide(skip);
    let back = j.next_decision();
    assert_eq!(back.cause, [fact]);
    let d = crate::schedule_tests::decision(&back);
    assert_eq!(d.decision, Decision::Backoff);
    assert_eq!(d.from_offset.get(), -20);
    assert_eq!(
        d.failure_point.map(togi_machine::TargetOffset::get),
        Some(-19)
    );
    assert_eq!(d.to_offset.get(), -14);
    j.decide(back);
    assert_eq!(
        j.run()
            .failure_point(core(0))
            .map(togi_machine::TargetOffset::get),
        Some(-19)
    );
    assert!(j.run().failures.pending(fact).is_none());
    j.assert_resumes();
}

#[test]
fn a_carried_all_zero_idle_failure_stops_before_any_trial() {
    let mut j = Journal::new(&[0, 1], &[Start::at_limit(0), Start::at_limit(0)]);
    let fact = j.carry_idle(SESSION, &[0, 0]);
    assert_eq!(j.run().failures.queue().collect::<Vec<_>>(), [fact]);
    let stop = j.next_decision();
    let DecisionPayload::DeadEnd(dead_end) = &stop.payload else {
        panic!("expected a dead end, got {stop:?}");
    };
    assert_eq!(
        dead_end.condition,
        togi_journal::DeadEndCondition::FailureAtZero
    );
    assert_eq!(dead_end.core, None);
    assert_eq!(stop.cause, [fact]);
    j.decide(stop);
    assert!(j.run().failures.queue_is_empty());

    let mut j = Journal::new(&[0, 1], &[Start::at_limit(-10), Start::at_limit(-10)]);
    let fact = j.carry_idle(SESSION, &[-10, -10]);
    assert_eq!(j.run().failures.queue().collect::<Vec<_>>(), [fact]);
    assert!(!matches!(
        j.next(),
        Action::Decide(DecisionAction {
            payload: DecisionPayload::DeadEnd(_),
            ..
        })
    ));
}

#[test]
fn a_pass_counts_under_the_backend_its_trial_began_with() {
    let k = class(Regime::R1, 0, &[0], 90);
    let mut j = Journal::new(&[0], &[Start::search(-20).checking()]);
    j.load(&backends(MPRIME_OLD, YCRUNCHER_OLD));
    let plan = j.next_trial();
    let (intent, trial) = j.intent(&plan);
    j.load(&backends(MPRIME_NEW, YCRUNCHER_OLD));
    j.end(intent, &trial, Outcome::Pass);
    assert_eq!(valid(&j, k, &[-20]), 0);
    j.load(&backends(MPRIME_OLD, YCRUNCHER_OLD));
    assert_eq!(valid(&j, k, &[-20]), 1);
    j.assert_resumes();
}

#[test]
fn a_step_pass_belongs_to_the_backend_its_trial_began_with() {
    let mut j = Journal::search_at(&[-10]);
    j.load(&backends(MPRIME_OLD, YCRUNCHER_OLD));
    let plan = j.next_trial();
    let (intent, trial) = j.intent(&plan);
    j.load(&backends(MPRIME_NEW, YCRUNCHER_OLD));
    let r1 = j.end(intent, &trial, Outcome::Pass);
    let step = |j: &Journal| j.run().search.current(core(0), &j.run().evidence);
    assert_eq!(step(&j), (None, None));
    assert_eq!(j.next_trial().regime, Regime::R1);
    j.load(&backends(MPRIME_OLD, YCRUNCHER_OLD));
    assert_eq!(step(&j), (Some(r1), None));
    assert_eq!(j.next_trial().regime, Regime::R2);
}

#[test]
fn failures_hold_unresolved_records_only() {
    let mut j = Journal::search_at(&[-30]);
    let mut high_water = (0, 0);
    for _ in 0..8 {
        j.run_until(
            400,
            |plan| {
                let Load::Single(c) = plan.load else {
                    unreachable!()
                };
                if plan
                    .profile
                    .get(c)
                    .is_some_and(|offset| offset.get() >= -12)
                {
                    Outcome::Pass
                } else {
                    Outcome::Failure
                }
            },
            |action| {
                matches!(action, Action::Decide(DecisionAction {
                    payload: DecisionPayload::CorePhase(phase), ..
                }) if phase.to != togi_journal::CorePhase::Search)
            },
        );
        let failures = &j.run().failures;
        assert_eq!(failures.unresolved(), 0);
        high_water = (
            high_water.0.max(failures.unresolved()),
            high_water.1.max(failures.witnesses()),
        );
        j.reset(0);
        let reset = j.next_decision();
        j.decide(reset);
        assert_eq!(
            (j.run().failures.unresolved(), j.run().failures.witnesses()),
            (0, 0)
        );
        j.assert_resumes();
    }
    assert!(high_water.1 <= 6, "witnesses {high_water:?}");
}

#[test]
fn repeated_known_failures_keep_one_witness_per_cell() {
    let mut j = Journal::search_at(&[-30]);
    let fact = j.carry(
        SESSION,
        (Regime::R1, 0, &[0], 90),
        Condition::Alone,
        &[-29],
        Outcome::Failure,
    );
    for _ in 0..20 {
        let skip = j.next_decision();
        assert_eq!(skip.cause, [fact]);
        j.decide(skip);
        assert_eq!(j.run().failures.unresolved(), 1);
        let back = j.next_decision();
        j.decide(back);
        assert_eq!(j.run().failures.unresolved(), 0);
        j.core_phase(crate::schedule_tests::transition(
            0,
            togi_journal::CorePhase::Search,
            -30,
        ));
    }
    assert_eq!(j.run().failures.witnesses(), 1);
}

#[test]
fn a_carried_all_zero_idle_failure_behind_another_still_stops() {
    let mut j = Journal::search_at(&[-10, -10]);
    let first = j.carry_idle(SESSION, &[-10, -10]);
    let zero = j.carry_idle(SESSION, &[0, 0]);
    assert_eq!(j.run().failures.queue().collect::<Vec<_>>(), [first, zero]);
    let stop = j.next_decision();
    let DecisionPayload::DeadEnd(dead_end) = &stop.payload else {
        panic!("expected a dead end, got {stop:?}");
    };
    assert_eq!(
        dead_end.condition,
        togi_journal::DeadEndCondition::FailureAtZero
    );
    assert_eq!(dead_end.core, None);
    assert_eq!(stop.cause, [zero]);
}

const OTHER_SESSION: &str = "20261003T004254Z";

#[test]
fn a_carried_failure_warning_names_the_failure_session() {
    for idle in [false, true] {
        let mut j = Journal::new(&[0], &[Start::search(-20).checking()]);
        let n = count(&j);
        let (regime, duration, condition) = if idle {
            (Regime::R6, 900, Condition::Together)
        } else {
            (Regime::R1, 90, Condition::Alone)
        };
        let passes = carry_passes(
            &mut j,
            (regime, 0, &[0], duration),
            &[-20],
            n,
            Outcome::Pass,
        );
        let failure = if idle {
            j.carry_idle(OTHER_SESSION, &[-19])
        } else {
            j.carry(
                OTHER_SESSION,
                (regime, 0, &[0], duration),
                condition,
                &[-19],
                Outcome::Failure,
            )
        };
        let action = j.next_decision();
        let DecisionPayload::TunerWarning(w) = &action.payload else {
            panic!("idle={idle}: failure did not contradict carried passes: {action:?}");
        };
        let mut cause = vec![failure];
        cause.extend(passes.iter().copied());
        assert_eq!(action.cause, cause, "idle={idle}");
        let clause = format!(
            "; {} carried facts from session {SESSION}, {OTHER_SESSION}",
            n + 1
        );
        assert!(w.detail.ends_with(&clause), "idle={idle}: {}", w.detail);
        j.assert_resumes();
    }
}

fn poisoned(j: &Journal, state: &mut crate::State) {
    let next = j.event(
        togi_journal::SessionNotice {
            notice: togi_journal::Notice::NonzeroBaseline,
            cores: togi_machine::CoreSet::default(),
        }
        .into(),
        &[],
    );
    assert_eq!(state.fold(&next), Err(FoldError::Poisoned));
    assert_eq!(state.next().ok(), None);
}

fn carried_naming_outside(change: fn(&mut togi_journal::TrialCarried)) {
    let outside = crate::tests::core(9);
    for outcome in [Outcome::Pass, Outcome::Failure] {
        let mut j = Journal::search_at(&[-10, -10]);
        let mut carried = j.carried_trial(
            SESSION,
            togi_journal::TrialClass {
                regime: Regime::R1,
                workload: togi_machine::WorkloadId::new(Regime::R1, 0),
                cores: set(&[0]),
                duration_s: 90,
            },
            Condition::Alone,
            &[-10, -10],
            outcome,
            None,
        );
        change(&mut carried);
        assert_eq!(
            j.try_append(carried.into(), &[]),
            Err(FoldError::UnknownCore(outside)),
            "{outcome:?}"
        );
        let mut state = j.state.clone();
        poisoned(&j, &mut state);
    }
}

#[test]
fn a_carried_trial_core_outside_the_topology_poisons() {
    carried_naming_outside(|carried| carried.core = Some(core(9)));
}

#[test]
fn a_carried_trial_stalled_core_outside_the_topology_poisons() {
    carried_naming_outside(|carried| carried.stalled_core = Some(core(9)));
}

#[test]
fn a_carried_trial_request_lane_outside_the_topology_poisons() {
    carried_naming_outside(|carried| {
        carried.voltage_requests_v = [(core(9), 1.0)].into_iter().collect();
    });
}
