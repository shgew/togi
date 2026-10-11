use togi_journal::{CorePhase, DeadEndCondition, Decision, Outcome, Seq, TunerDecision};
use togi_machine::{BankType, CcdId, Condition, CpuId, Signal};

use crate::r7::escalation::{ESCALATE_AFTER, Escalation};
use crate::r7::reason::{Basis, Note, R7Reason};
use crate::r7::respond::{R7Response, R7Route};
use crate::schedule_tests::{decision, transition};
use crate::testkit::{Journal, R7Report, Start, pairs, set};
use crate::tests::{core, off};
use crate::{Action, DecisionAction, DecisionPayload, TrialContext};

#[test]
fn r7_attribution_and_tie_backoff() {
    type Case = (
        &'static str,
        &'static [u8],
        &'static [u8],
        Option<u8>,
        Option<u8>,
        &'static [(u8, i64)],
    );
    let cases: [Case; 6] = [
        ("named", &[0, 1], &[0], Some(1), None, &[(1, -29)]),
        ("named idle core", &[0, 1], &[0], Some(3), None, &[(3, -29)]),
        ("whole ccd", &[0, 1], &[0], None, None, &[(0, -29)]),
        (
            "all ccds",
            &[0, 1, 2, 3],
            &[0, 2],
            None,
            None,
            &[(0, -29), (2, -29)],
        ),
        (
            "stalled ccd",
            &[0, 1, 2, 3],
            &[0, 2],
            None,
            Some(3),
            &[(2, -29)],
        ),
        ("tied ccd", &[0, 1], &[0, 1], None, None, &[(1, -29)]),
    ];
    for (name, cores, top, named, stalled, want) in cases {
        let mut j = Journal::r7_session();
        let mut report = R7Report::default().top(top);
        report.named = named;
        report.stalled = stalled;
        let profile = j.offsets();
        let values: Vec<i64> = profile.iter().map(|(_, offset)| offset.get()).collect();
        j.r7_fact(false, cores, &values, &report);
        assert_eq!(j.drain_backoffs(8), want, "{name}");
        assert!(
            !matches!(j.state.decide(), Some(Action::Decide(d)) if matches!(d.payload, DecisionPayload::HuntStart(_))),
            "{name}"
        );
        j.assert_resumes();
    }
}

impl Journal {
    fn at_values(&self) -> Vec<i64> {
        self.offsets()
            .iter()
            .map(|(_, offset)| offset.get())
            .collect()
    }

    fn at_needed(&self) -> usize {
        usize::from(self.run().config.needed.get())
    }

    fn at_set_offset(&mut self, id: u8, offset: i64) -> Seq {
        self.core_phase(transition(id, CorePhase::HasRoom, offset))
    }

    fn at_r7_backoff(&self) -> (DecisionAction, R7Reason) {
        match self.run().r7_decision() {
            Some(R7Response::Backoff { decision, why }) => (decision, why),
            other => panic!("expected an R7 backoff, got {other:?}"),
        }
    }

    fn at_next_backoff(&self) -> (DecisionAction, (u8, i64)) {
        let action = self.next_decision();
        let td = decision(&action);
        assert_eq!(td.decision, Decision::Backoff, "{action:?}");
        let moved = (td.core.get(), td.to_offset.get());
        (action, moved)
    }

    fn at_next_dead_end(&self) -> DecisionAction {
        let action = self.next_decision();
        match &action.payload {
            DecisionPayload::DeadEnd(dead) => {
                assert_eq!(dead.condition, DeadEndCondition::FailureAtZero);
            }
            other => panic!("expected a dead end, got {other:?}"),
        }
        action
    }

    fn at_assert_no_hunt(&self) {
        assert!(
            !matches!(self.state.decide(), Some(Action::Decide(d)) if matches!(d.payload, DecisionPayload::HuntStart(_)))
        );
    }

    fn at_run_zero_rerun(&mut self, cores: &[u8], fail: bool) -> Seq {
        assert!(matches!(
            self.run().r7_decision(),
            Some(R7Response::ZeroRerun(_))
        ));
        let plan = self.next_trial();
        assert!(
            matches!(plan.context, TrialContext::ZeroRerun { .. }),
            "{plan:?}"
        );
        assert_eq!(plan.condition, Condition::Parked);
        assert!(plan.profile.is_zero());
        assert_eq!(plan.load.cores(), set(cores));
        if !fail {
            return self.trial(&plan, Outcome::Pass);
        }
        let end = self.trial(&plan, Outcome::Failure);
        let attribution = self.next_decision();
        assert!(
            matches!(attribution.payload, DecisionPayload::Failure(_)),
            "{attribution:?}"
        );
        self.decide(attribution);
        end
    }
}

fn requests(lanes: &[(u8, f64)]) -> R7Report {
    R7Report::default().requests(lanes)
}

#[test]
fn r7_failures_move_after_long_clean_ledger() {
    let mut j = Journal::r7_session();
    let report = requests(&[(0, 1.1), (1, 1.08)]).top(&[0]);
    let profile = j.at_values();
    for _ in 0..100 {
        j.r7_fact(true, &[0, 1], &profile, &report);
    }
    for want in [-29, -28] {
        let at = j.at_values();
        j.r7_fact(false, &[0, 1], &at, &report);
        let (action, moved) = j.at_next_backoff();
        assert_eq!(moved, (0, want));
        j.decide(action);
    }
    j.at_assert_no_hunt();
    let plan = j.r7_plan(&[0, 1], &j.at_values());
    assert!(matches!(
        j.run().skip_known_failure(Action::RunTrial(plan)),
        Action::RunTrial(_)
    ));
    j.assert_resumes();
}

#[test]
fn r7_live_named_and_mce_attribution() {
    for local_mce in [false, true] {
        let mut j = Journal::r7_session();
        let plan = j.r7_plan(&[0, 1], &j.at_values());
        let (intent, trial) = j.intent(&plan);
        let mut cause = vec![intent];
        let mut end =
            Journal::end_payload(&trial, Outcome::Failure, Some(Signal::ComputationError));
        end.duration_s = 120;
        end.top_requesters = set(&[0]);
        if local_mce {
            let mce = togi_journal::Mce {
                cpu: CpuId(1),
                core: Some(core(1)),
                bank: 0,
                bank_type: BankType::LoadStore,
                corrected: false,
                monotonic_ns: None,
                from_boot: None,
                trial: Some(trial.clone()),
                between_trials: false,
                lines: Vec::new(),
            };
            cause.push(j.append(mce.into(), &[]));
        } else {
            end.core = Some(core(1));
        }
        j.append(end.into(), &cause);
        let attribution = j.next_decision();
        assert!(
            matches!(attribution.payload, DecisionPayload::Failure(_)),
            "{local_mce}"
        );
        j.decide(attribution);
        let (_, moved) = j.at_next_backoff();
        assert_eq!(moved, (1, -29), "{local_mce}");
    }
}

#[test]
fn r7_all_core_targets_use_each_ccd() {
    let mut j = Journal::r7_session();
    let passes = requests(&[(0, 1.118), (1, 1.09), (2, 1.236), (3, 1.19)]).top(&[0, 2]);
    for _ in 0..j.at_needed() {
        j.r7_fact(true, &[0, 1, 2, 3], &[-25, -30, -20, -30], &passes);
    }
    let failing = requests(&[(0, 1.1), (1, 1.09), (2, 1.2), (3, 1.19)]).top(&[0, 2]);
    let at = j.at_values();
    j.r7_fact(false, &[0, 1, 2, 3], &at, &failing);
    assert_eq!(j.drain_backoffs(8), [(0, -25), (2, -20)]);
}

#[test]
fn r7_zero_failure_drains_without_reading_ranking() {
    let ids = [0, 1, 2, 3];
    let mut j = Journal::on(&ids, pairs(&ids), &[Start::has_room(0); 4]);
    j.profile_change(&[0, 0, 0, 0]);
    j.r7_fact(
        false,
        &[0, 1],
        &[0, 0, 0, 0],
        &R7Report::default().top(&[0, 1]),
    );
    assert!(matches!(
        j.run().r7_decision(),
        Some(R7Response::DeadEnd(_))
    ));
    j.at_next_dead_end();
}

#[test]
fn r7_backoff_reason() {
    struct Case {
        name: &'static str,
        requests: &'static [(u8, f64)],
        target: Option<f64>,
        basis: Basis,
        counts: i64,
    }
    let cases = [
        Case {
            name: "offset fallback",
            requests: &[],
            target: None,
            basis: Basis::NoTelemetry { offset: -30 },
            counts: 1,
        },
        Case {
            name: "no pass",
            requests: &[(0, 1.09447), (1, 1.08)],
            target: None,
            basis: Basis::NoPass { request: 1.09447 },
            counts: 1,
        },
        Case {
            name: "equal target",
            requests: &[(0, 1.1539), (1, 1.08)],
            target: Some(1.1539),
            basis: Basis::Met {
                request: 1.1539,
                target: 1.1539,
            },
            counts: 1,
        },
        Case {
            name: "higher than target",
            requests: &[(0, 1.16), (1, 1.08)],
            target: Some(1.1539),
            basis: Basis::Met {
                request: 1.16,
                target: 1.1539,
            },
            counts: 1,
        },
        Case {
            name: "one count",
            requests: &[(0, 1.15), (1, 1.08)],
            target: Some(1.153),
            basis: Basis::Toward {
                request: 1.15,
                target: 1.153,
            },
            counts: 1,
        },
        Case {
            name: "several counts",
            requests: &[(0, 1.09447), (1, 1.08)],
            target: Some(1.1539),
            basis: Basis::Toward {
                request: 1.09447,
                target: 1.1539,
            },
            counts: 17,
        },
    ];
    for case in cases {
        let name = case.name;
        let mut j = Journal::r7_session();
        let profile = j.at_values();
        if let Some(target) = case.target {
            for _ in 0..j.at_needed() {
                j.r7_fact(
                    true,
                    &[0, 1],
                    &[-20, -30, -30, -30],
                    &requests(&[(0, target), (1, 1.08)]).top(&[0]),
                );
            }
        }
        let failure = j.r7_fact(
            false,
            &[0, 1],
            &profile,
            &requests(case.requests).top(&[0]).named(0),
        );
        let (action, why) = j.at_r7_backoff();
        assert_eq!(why.failure, failure, "{name}");
        assert_eq!(why.core, core(0), "{name}");
        assert_eq!(why.basis, case.basis, "{name}");
        assert_eq!(why.counts, case.counts, "{name}");
        assert!(why.notes.is_empty(), "{name}: {:?}", why.notes);
        assert_eq!(why.rail, None, "{name}");
        assert!(action.cause.contains(&failure), "{name}");
        assert!(
            decision(&action).reason.contains("20230101T000000Z"),
            "{name}"
        );
        assert_eq!(j.next_decision(), action, "{name}");
    }
}

#[test]
fn r7_backoff_reason_counts_the_loads_live_backoffs() {
    let mut j = Journal::r7_session();
    for n in 1..=ESCALATE_AFTER {
        let at = j.at_values();
        let failure = j.r7_failed(&[0, 1], &at, &R7Report::default().top(&[0]));
        let (action, why) = j.at_r7_backoff();
        assert_eq!(why.failure, failure);
        assert_eq!(
            why.notes,
            [Note::Ordinal {
                n,
                after: ESCALATE_AFTER
            }]
        );
        assert_eq!(j.next_decision(), action);
        j.decide(action);
    }
}

#[test]
fn r7_zero_top_steps_down_within_ccd() {
    type Case = (&'static str, &'static [(u8, f64)], &'static [u8], i64, bool);
    let cases: [Case; 3] = [
        ("measured order", &[(0, 1.2), (1, 1.15)], &[0], -16, true),
        ("offset fallback", &[], &[0], -29, true),
        (
            "partly zero tie",
            &[(0, 1.2), (1, 1.1995)],
            &[0, 1],
            -29,
            false,
        ),
    ];
    let profile = [0, -30, -30, -30];
    for (name, lanes, top, want, stepped) in cases {
        let mut j = Journal::r7_session();
        j.profile_change(&profile);
        let failure = j.r7_fact(false, &[0, 1], &profile, &requests(lanes).top(top));
        let (_, why) = j.at_r7_backoff();
        let (action, moved) = j.at_next_backoff();
        assert_eq!(moved, (1, want), "{name}");
        let steps: Vec<&Note> = why
            .notes
            .iter()
            .filter(|note| matches!(note, Note::SteppedDown { .. }))
            .collect();
        if stepped {
            assert_eq!(
                steps,
                [&Note::SteppedDown {
                    ccd: CcdId(0),
                    skipped: vec![set(&[0])],
                    group: set(&[1]),
                }],
                "{name}"
            );
        } else {
            assert!(steps.is_empty(), "{name}");
        }
        assert!(action.cause.contains(&failure), "{name}");
        j.decide(action);
        assert_eq!(j.run().r7_decision(), None, "{name}");
        j.assert_resumes();
    }
}

#[test]
fn r7_zero_top_uses_shifted_full_measurement() {
    let mut j = Journal::r7_session();
    let source = j.r7_fact(
        true,
        &[0, 1],
        &[-5, -30, -30, -30],
        &requests(&[(0, 1.2), (1, 1.15)]).top(&[0]),
    );
    let profile = [0, -30, -30, -30];
    j.profile_change(&profile);
    j.r7_fact(false, &[0, 1], &profile, &R7Report::default().top(&[0]));
    let (action, moved) = j.at_next_backoff();
    assert_eq!(moved, (1, -11));
    assert!(action.cause.contains(&source), "{:?}", action.cause);
}

#[test]
fn r7_zero_affected_ccd_dead_ends_after_its_all_zero_rerun() {
    for rerun_fails in [true, false] {
        let mut j = Journal::r7_session();
        let profile = [0, 0, -30, -30];
        j.profile_change(&profile);
        let report = requests(&[(0, 1.2), (1, 1.15), (2, 1.3), (3, 1.25)])
            .top(&[0, 2])
            .stalled(1);
        let failure = j.r7_fact(false, &[0, 1, 2, 3], &profile, &report);
        let rerun = j.at_run_zero_rerun(&[0, 1, 2, 3], rerun_fails);
        if rerun_fails {
            let dead = j.at_next_dead_end();
            assert!(
                dead.cause.contains(&failure) && dead.cause.contains(&rerun),
                "{:?}",
                dead.cause
            );
            let DecisionPayload::DeadEnd(end) = &dead.payload else {
                unreachable!()
            };
            assert_eq!(end.core, Some(core(0)));
            continue;
        }
        let (_, why) = j.at_r7_backoff();
        assert_eq!(
            why.notes,
            [Note::RerunPassed {
                failure,
                end: rerun
            }]
        );
        let (action, moved) = j.at_next_backoff();
        assert_eq!(moved, (2, -29));
        assert!(
            action.cause.contains(&failure) && action.cause.contains(&rerun),
            "{:?}",
            action.cause
        );
        j.decide(action);
        assert_eq!(j.run().r7_decision(), None);
        j.assert_resumes();
    }
}

#[test]
fn r7_named_zero_attribution_uses_start_top() {
    struct Case {
        name: &'static str,
        top: &'static [u8],
        requests: &'static [(u8, f64)],
        previous: &'static [(u8, f64)],
        dead: bool,
    }
    let cases = [
        Case {
            name: "measured top",
            top: &[0],
            requests: &[(0, 1.2), (1, 1.1)],
            previous: &[],
            dead: true,
        },
        Case {
            name: "measured tied top",
            top: &[0, 1],
            requests: &[(0, 1.2), (1, 1.2005)],
            previous: &[],
            dead: true,
        },
        Case {
            name: "measured not top",
            top: &[1],
            requests: &[(0, 1.1), (1, 1.2)],
            previous: &[],
            dead: false,
        },
        Case {
            name: "derived top",
            top: &[],
            requests: &[],
            previous: &[(0, 1.2), (1, 1.1)],
            dead: true,
        },
        Case {
            name: "derived not top",
            top: &[],
            requests: &[],
            previous: &[(0, 1.1), (1, 1.2)],
            dead: false,
        },
        Case {
            name: "offset fallback top",
            top: &[],
            requests: &[],
            previous: &[],
            dead: true,
        },
    ];
    let profile = [0, -30, -30, -30];
    for case in cases {
        let name = case.name;
        let mut j = Journal::r7_session();
        j.profile_change(&profile);
        let previous = (!case.previous.is_empty())
            .then(|| j.r7_fact(true, &[0, 1], &profile, &requests(case.previous)));
        j.r7_fact(
            false,
            &[0, 1],
            &profile,
            &requests(case.requests).top(case.top).named(0),
        );
        if name == "derived top" {
            j.r7_fact(
                true,
                &[0, 1],
                &profile,
                &requests(&[(0, 1.1), (1, 1.2)]).top(&[1]),
            );
        }
        if case.dead {
            let rerun = j.at_run_zero_rerun(&[0, 1], true);
            let dead = j.at_next_dead_end();
            assert!(dead.cause.contains(&rerun), "{name}: {:?}", dead.cause);
            if let Some(previous) = previous {
                assert!(dead.cause.contains(&previous), "{name}: {:?}", dead.cause);
            }
            let DecisionPayload::DeadEnd(end) = &dead.payload else {
                unreachable!()
            };
            assert_eq!(end.core, Some(core(0)), "{name}");
        } else {
            let (_, why) = j.at_r7_backoff();
            assert_eq!(
                why.notes,
                [Note::NamedZeroNotTop {
                    core: core(0),
                    ccd: CcdId(0)
                }],
                "{name}"
            );
            let (action, moved) = j.at_next_backoff();
            assert_eq!(moved, (1, -29), "{name}");
            j.decide(action);
            assert_eq!(j.run().r7_decision(), None, "{name}");
        }
        j.assert_resumes();
    }
}

#[test]
fn r7_all_core_failure_skips_ccd_at_zero() {
    for (name, profile, dead) in [
        ("other CCD movable", [0, 0, -30, -30], false),
        ("every CCD at zero", [0, 0, 0, 0], true),
    ] {
        let mut j = Journal::r7_session();
        j.r7_fact(
            false,
            &[0, 1, 2, 3],
            &profile,
            &R7Report::default().top(&[1, 2]),
        );
        if dead {
            j.at_next_dead_end();
            continue;
        }
        let (action, moved) = j.at_next_backoff();
        assert_eq!(moved, (2, -29), "{name}");
        j.decide(action);
        assert_eq!(j.run().r7_decision(), None, "{name}");
        j.assert_resumes();
    }
}

#[test]
fn r7_named_idle_zero_core_counts_against_loaded_ccd() {
    let mut j = Journal::r7_session();
    j.r7_fact(
        false,
        &[0, 1],
        &[-30, -30, 0, -30],
        &R7Report::default().top(&[0]).named(2),
    );
    let (_, why) = j.at_r7_backoff();
    assert_eq!(
        why.notes,
        [Note::NamedOnEmptyCcd {
            named: core(2),
            named_ccd: CcdId(1),
            loaded_ccd: CcdId(0),
        }]
    );
    let (action, moved) = j.at_next_backoff();
    assert_eq!(moved, (0, -29));
    j.decide(action);
    assert_eq!(j.run().r7_decision(), None);
    j.assert_resumes();
}

#[test]
fn r7_named_idle_zero_core_on_loaded_ccd_at_zero_waits_to_be_located() {
    let mut j = Journal::r7_session();
    let failure = j.r7_fact(
        false,
        &[0, 1],
        &[0, 0, 0, -30],
        &R7Report::default().top(&[0]).named(2),
    );
    assert_eq!(j.run().r7_decision(), None);
    assert!(
        j.run()
            .eligible_for_locate(j.run().r7.open.get(&failure).unwrap())
    );
    let Some(R7Route::Locate {
        failure: located,
        reason,
    }) = j.run().locate_due()
    else {
        panic!("the failure is not kept for a located hunt");
    };
    assert_eq!(located, failure);
    assert_eq!(reason.rerun_passed, None);
    assert_eq!(reason.escalation, Some(Escalation::LoadedAtZero));
}

#[test]
fn r7_named_unloaded_core_off_zero_is_not_located() {
    let mut j = Journal::r7_session();
    j.at_set_offset(0, 0);
    j.at_set_offset(1, 0);
    let profile = [0, 0, -30, -30];
    j.profile_change(&profile);
    let failure = j.r7_failed(&[0, 1], &profile, &R7Report::default().top(&[0]).named(2));
    assert!(
        !j.run()
            .eligible_for_locate(j.run().r7.open.get(&failure).unwrap())
    );
    let (action, moved) = j.at_next_backoff();
    assert_eq!(moved, (2, -29));
    assert_eq!(action.cause[0], failure);
    j.decide(action);
    assert_eq!(j.run().r7_decision(), None);
    assert_eq!(j.run().locate_due(), None);
    j.at_assert_no_hunt();
    j.assert_resumes();
}

#[test]
fn r7_failure_needs_no_move_while_moved_core_is_shallower() {
    type Case = (&'static str, [i64; 4], [i64; 4], Option<u8>);
    let cases: [Case; 3] = [
        (
            "named core shallower",
            [-30, -30, -30, -30],
            [-35, -30, -30, -30],
            Some(0),
        ),
        (
            "top requester shallower",
            [-30, -30, -30, -30],
            [-35, -35, -30, -30],
            None,
        ),
        (
            "top requester now at CO 0",
            [0, -30, -30, -30],
            [-30, -30, -30, -30],
            None,
        ),
    ];
    for (name, current, failed, named) in cases {
        let mut j = Journal::r7_session();
        for (id, &offset) in (0_u8..).zip(&current) {
            if offset != -30 {
                j.at_set_offset(id, offset);
            }
        }
        let mut report = R7Report::default().top(&[0]);
        report.named = named;
        j.r7_fact(false, &[0, 1], &failed, &report);
        assert_eq!(j.run().r7_decision(), None, "{name}");
        j.at_set_offset(0, failed[0]);
        let (_, moved) = j.at_next_backoff();
        assert_eq!(moved, (0, failed[0] + 1), "{name}");
        j.assert_resumes();
    }
}

#[test]
fn r7_pending_revives_on_offset_return() {
    let mut j = Journal::r7_session();
    let failure = j.r7_fact(
        false,
        &[0, 1],
        &[-35, -30, -30, -30],
        &R7Report::default().top(&[0]).named(0),
    );
    assert_eq!(j.run().r7_decision(), None);
    j.decision(TunerDecision {
        core: core(0),
        activity: togi_journal::Activity::Checking,
        decision: Decision::StepDeeper,
        from_offset: off(-30),
        to_offset: off(-33),
        pass: None,
        failure_point: None,
        workloads: Vec::new(),
        reason: "test".to_owned(),
    });
    assert_eq!(j.run().r7_decision(), None);
    j.at_set_offset(0, -35);
    let (_, at_failing) = j.at_r7_backoff();
    let (_, moved) = j.at_next_backoff();
    assert_eq!(moved, (0, -34));
    assert_eq!(at_failing.failure, failure);
    j.at_set_offset(0, -36);
    let (_, deeper) = j.at_r7_backoff();
    assert_eq!(deeper, at_failing);
    assert_eq!(j.at_next_backoff().1, moved);
    j.decision(TunerDecision {
        core: core(0),
        activity: togi_journal::Activity::Checking,
        decision: Decision::Backoff,
        from_offset: off(-36),
        to_offset: off(-32),
        pass: None,
        failure_point: None,
        workloads: Vec::new(),
        reason: "test".to_owned(),
    });
    assert_eq!(j.run().r7_decision(), None);
    j.assert_resumes();
}

#[test]
fn r7_mce_named_unloaded_core_uses_its_lane() {
    for local_mce in [false, true] {
        let mut j = Journal::r7_session();
        let n = j.at_needed();
        let all = [-30, -30, -30, -30];
        for _ in 0..n {
            j.r7_fact(
                true,
                &[2, 3],
                &all,
                &requests(&[(2, 1.1), (3, 1.09)]).top(&[2]),
            );
        }
        let plan = j.r7_plan(&[0, 1], &all);
        let (intent, trial) = j.intent(&plan);
        let mut cause = vec![intent];
        let mut end =
            Journal::end_payload(&trial, Outcome::Failure, Some(Signal::ComputationError));
        end.duration_s = 120;
        if local_mce {
            let mce = togi_journal::Mce {
                cpu: CpuId(3),
                core: Some(core(3)),
                bank: 0,
                bank_type: BankType::LoadStore,
                corrected: false,
                monotonic_ns: None,
                from_boot: None,
                trial: Some(trial.clone()),
                between_trials: false,
                lines: Vec::new(),
            };
            cause.push(j.append(mce.into(), &[]));
        } else {
            end.core = Some(core(3));
        }
        j.append(end.into(), &cause);
        let attribution = j.next_decision();
        assert!(
            matches!(attribution.payload, DecisionPayload::Failure(_)),
            "{local_mce}"
        );
        j.decide(attribution);
        let (decision, moved) = j.at_next_backoff();
        assert_eq!(moved, (3, -27), "{local_mce}");
        assert!(decision.cause.len() > n, "{local_mce}");
    }
}
