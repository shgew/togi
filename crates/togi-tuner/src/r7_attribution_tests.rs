use togi_journal::{CorePhase, DeadEndCondition, Decision, Outcome, Seq, TunerDecision};
use togi_machine::{BankType, CcdId, Condition, CpuId, Regime, Signal};

use crate::r7::escalation::{ESCALATE_AFTER, Escalation};
use crate::r7::reason::{Basis, Note, R7Reason};
use crate::r7::respond::{R7Response, R7Route};
use crate::schedule_tests::{decision, transition};
use crate::testkit::{Journal, R7Report, Start, pairs, set};
use crate::tests::{core, off};
use crate::{Action, DecisionAction, DecisionPayload, TrialContext};

#[test_case::test_case("named", &[0, 1], &[0], Some(1), None, &[(1, -29)]; "named")]
#[test_case::test_case("named idle core", &[0, 1], &[0], Some(3), None, &[(3, -29)]; "named idle core")]
#[test_case::test_case("whole ccd", &[0, 1], &[0], None, None, &[(0, -29)]; "whole ccd")]
#[test_case::test_case("all ccds", &[0, 1, 2, 3], &[0, 2], None, None, &[(0, -29), (2, -29)]; "all ccds")]
#[test_case::test_case("stalled ccd", &[0, 1, 2, 3], &[0, 2], None, Some(3), &[(2, -29)]; "stalled ccd")]
#[test_case::test_case("tied ccd", &[0, 1], &[0, 1], None, None, &[(1, -29)]; "tied ccd")]
fn r7_attribution_and_tie_backoff(
    name: &'static str,
    cores: &'static [u8],
    top: &'static [u8],
    named: Option<u8>,
    stalled: Option<u8>,
    want: &'static [(u8, i64)],
) {
    let mut j = Journal::r7_session();
    let mut report = R7Report::default().top(top);
    report.named = named;
    report.stalled = stalled;
    let profile = j.offsets();
    let values: Vec<i64> = profile.iter().map(|(_, offset)| offset.get()).collect();
    j.r7_fact(false, cores, &values, &report);
    assert_eq!(j.drain_backoffs(8), want, "{name}");
    assert!(
        !matches!(j.state.next().ok(), Some(Action::Decide(d)) if matches!(d.payload, DecisionPayload::HuntStart(_))),
        "{name}"
    );
    j.assert_resumes();
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

    fn at_mce(&mut self, trial: &togi_journal::TrialId, id: u8) -> Seq {
        self.append(
            togi_journal::Mce {
                cpu: CpuId::from(u16::from(id)),
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
            .into(),
            &[],
        )
    }

    fn at_set_offset(&mut self, id: u8, offset: i64) -> Seq {
        self.core_phase(transition(id, CorePhase::HasRoom, offset))
    }

    fn at_r7_backoff(&self) -> (DecisionAction, R7Reason) {
        let response = self.run().r7_decision();
        let Some(R7Response::Backoff { decision, why }) = response else {
            panic!("expected an R7 backoff, got {response:?}");
        };
        (decision, why)
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
        let DecisionPayload::DeadEnd(dead) = &action.payload else {
            panic!("expected a dead end, got {:?}", action.payload);
        };
        assert_eq!(dead.condition, DeadEndCondition::FailureAtZero);
        action
    }

    fn at_assert_no_hunt(&self) {
        assert!(
            !matches!(self.state.next().ok(), Some(Action::Decide(d)) if matches!(d.payload, DecisionPayload::HuntStart(_)))
        );
    }

    fn at_run_zero_rerun(&mut self, cores: &[u8], fail: bool) -> Seq {
        assert!(matches!(
            self.run().r7_decision(),
            Some(R7Response::ZeroRerun(_))
        ));
        let plan = self.next_trial();
        assert!(
            matches!(plan.requirement.context, TrialContext::ZeroRerun { .. }),
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

#[test_case::test_case(false; "not local_mce")]
#[test_case::test_case(true; "local_mce")]
fn r7_live_named_and_mce_attribution(local_mce: bool) {
    let mut j = Journal::r7_session();
    let plan = j.r7_plan(&[0, 1], &j.at_values());
    let (intent, trial) = j.intent(&plan);
    let mut cause = vec![intent];
    let mut end = Journal::end_payload(&trial, Outcome::Failure, Some(Signal::ComputationError));
    end.duration_s = 120;
    end.top_requesters = set(&[0]);
    if local_mce {
        let mce = togi_journal::Mce {
            cpu: CpuId::from(1),
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

#[test]
fn r7_all_core_targets_use_each_ccd() {
    let mut j = Journal::r7_session();
    let passes = requests(&[(0, 1.118), (1, 1.09), (2, 1.236), (3, 1.19)]).top(&[0, 2]);
    for _ in 0..j.at_needed() {
        j.r7_fact(true, &[0, 1, 2, 3], &[-25, -30, -20, -30], &passes);
    }
    let failing = requests(&[(0, 1.1), (1, 1.09), (2, 1.2), (3, 1.19)]).top(&[0, 2]);
    let at = j.at_values();
    j.r7_failed(&[0, 1, 2, 3], &at, &failing);
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

#[test_case::test_case("offset fallback", &[], None, &Basis::NoTelemetry { offset: -30 }, 1, false; "offset fallback")]
#[test_case::test_case("no pass", &[(0, 1.09447), (1, 1.08)], None, &Basis::NoPass { request: 1.09447 }, 1, false; "no pass")]
#[test_case::test_case("equal target", &[(0, 1.1539), (1, 1.08)], Some(1.1539), &Basis::Met {
                request: 1.1539,
                target: 1.1539,
            }, 1, false; "equal target")]
#[test_case::test_case("higher than target", &[(0, 1.16), (1, 1.08)], Some(1.1539), &Basis::Met {
                request: 1.16,
                target: 1.1539,
            }, 1, false; "higher than target")]
#[test_case::test_case("one count", &[(0, 1.15), (1, 1.08)], Some(1.153), &Basis::Toward {
                request: 1.15,
                target: 1.153,
            }, 1, false; "one count")]
#[test_case::test_case("several counts", &[(0, 1.145), (1, 1.08)], Some(1.1539), &Basis::Toward {
                request: 1.145,
                target: 1.1539,
            }, 3, false; "several counts")]
#[test_case::test_case("uncapped machine check", &[(0, 1.09447), (1, 1.08)], Some(1.1539), &Basis::Toward {
                request: 1.09447,
                target: 1.1539,
            }, 17, true; "uncapped machine check")]
fn r7_backoff_reason(
    name: &'static str,
    lanes: &'static [(u8, f64)],
    target: Option<f64>,
    basis: &Basis,
    counts: i64,
    mce: bool,
) {
    let mut j = Journal::r7_session();
    let profile = j.at_values();
    if let Some(target) = target {
        for _ in 0..j.at_needed() {
            j.r7_fact(
                true,
                &[0, 1],
                &[-20, -30, -30, -30],
                &requests(&[(0, target), (1, 1.08)])
                    .top(&[0])
                    .clocks(&[(0, 5000)]),
            );
        }
    }
    let report = requests(lanes).top(&[0]).named(0).clocks(&[(0, 5000)]);
    let failure = if mce {
        let plan = j.r7_plan(&[0, 1], &profile);
        let (intent, trial) = j.intent(&plan);
        let mce = j.at_mce(&trial, 0);
        let mut end = Journal::end_payload(&trial, Outcome::Failure, Some(Signal::UncorrectedMce));
        end.duration_s = 120;
        end.voltage_requests_v = lanes.iter().map(|&(id, volts)| (core(id), volts)).collect();
        end.top_requesters = set(&[0]);
        end.ccd_mhz.insert(CcdId::new(0).unwrap(), 5000);
        j.append(end.into(), &[intent, mce]);
        let attribution = j.next_decision();
        let DecisionPayload::Failure(failure) = &attribution.payload else {
            panic!("expected MCE attribution, got {attribution:?}");
        };
        assert_eq!(
            (failure.signal, failure.core),
            (Signal::UncorrectedMce, Some(core(0)))
        );
        j.decide(attribution)
    } else {
        j.r7_failed(&[0, 1], &profile, &report)
    };
    let (action, why) = j.at_r7_backoff();
    assert_eq!(why.failure, failure, "{name}");
    assert_eq!(why.core, core(0), "{name}");
    assert_eq!(&why.basis, basis, "{name}");
    assert_eq!(why.counts, counts, "{name}");
    assert!(why.notes.is_empty(), "{name}: {:?}", why.notes);
    assert_eq!(why.rail, None, "{name}");
    assert!(action.cause.contains(&failure), "{name}");
    assert_eq!(
        decision(&action).reason.contains("20230101T000000Z"),
        target.is_some(),
        "{name}"
    );
    assert_eq!(j.next_decision(), action, "{name}");
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

#[test_case::test_case("measured order", &[(0, 1.2), (1, 1.15)], &[0], -16, true; "measured order")]
#[test_case::test_case("offset fallback", &[], &[0], -29, true; "offset fallback")]
#[test_case::test_case("partly zero tie", &[(0, 1.2), (1, 1.1995)], &[0, 1], -29, false; "partly zero tie")]
fn r7_zero_top_steps_down_within_ccd(
    name: &'static str,
    lanes: &'static [(u8, f64)],
    top: &'static [u8],
    want: i64,
    stepped: bool,
) {
    let profile = [0, -30, -30, -30];

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
                ccd: CcdId::new(0).unwrap(),
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

#[test_case::test_case(false; "not rerun_fails")]
#[test_case::test_case(true; "rerun_fails")]
fn r7_zero_affected_ccd_dead_ends_after_its_all_zero_rerun(rerun_fails: bool) {
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
        return;
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

#[test_case::test_case(&[0], &[(0, 1.2), (1, 1.1)], &[], None, true; "measured top")]
#[test_case::test_case(&[0, 1], &[(0, 1.2), (1, 1.2005)], &[], None, true; "measured tied top")]
#[test_case::test_case(&[1], &[(0, 1.1), (1, 1.2)], &[], None, false; "measured not top")]
#[test_case::test_case(
    &[], &[], &[(0, 1.2), (1, 1.1)], Some(&[(0, 1.1), (1, 1.2)][..]), true; "derived top"
)]
#[test_case::test_case(&[], &[], &[(0, 1.1), (1, 1.2)], None, false; "derived not top")]
#[test_case::test_case(&[], &[], &[], None, true; "offset fallback top")]
fn r7_named_zero_attribution_uses_start_top(
    top: &[u8],
    measured: &[(u8, f64)],
    previous: &[(u8, f64)],
    later_requests: Option<&[(u8, f64)]>,
    dead: bool,
) {
    let profile = [0, -30, -30, -30];
    let mut j = Journal::r7_session();
    j.profile_change(&profile);
    let previous =
        (!previous.is_empty()).then(|| j.r7_fact(true, &[0, 1], &profile, &requests(previous)));
    j.r7_fact(
        false,
        &[0, 1],
        &profile,
        &requests(measured).top(top).named(0),
    );
    if let Some(later_requests) = later_requests {
        j.r7_fact(true, &[0, 1], &profile, &requests(later_requests).top(&[1]));
    }
    if dead {
        let plan = j.next_trial();
        assert!(
            matches!(plan.requirement.context, TrialContext::ZeroRerun { .. }),
            "{plan:?}"
        );
        assert_eq!(plan.condition, Condition::Parked);
        assert!(plan.profile.is_zero(), "{plan:?}");
        assert_eq!(plan.load.cores(), set(&[0, 1]));
        let rerun = j.trial(&plan, Outcome::Failure);
        let attribution = j.next_decision();
        assert!(
            matches!(attribution.payload, DecisionPayload::Failure(_)),
            "{attribution:?}"
        );
        j.decide(attribution);
        let dead = j.at_next_dead_end();
        assert!(dead.cause.contains(&rerun), "{:?}", dead.cause);
        if let Some(previous) = previous {
            assert!(dead.cause.contains(&previous), "{:?}", dead.cause);
        }
        let DecisionPayload::DeadEnd(end) = &dead.payload else {
            unreachable!()
        };
        assert_eq!(end.core, Some(core(0)));
    } else {
        let (_, why) = j.at_r7_backoff();
        assert_eq!(
            why.notes,
            [Note::NamedZeroNotTop {
                core: core(0),
                ccd: CcdId::new(0).unwrap(),
            }]
        );
        let (action, moved) = j.at_next_backoff();
        assert_eq!(moved, (1, -29));
        j.decide(action);
        assert_eq!(j.run().r7_decision(), None);
    }
    j.assert_resumes();
}

#[test_case::test_case("other CCD movable", [0, 0, -30, -30], false; "other CCD movable")]
#[test_case::test_case("every CCD at zero", [0, 0, 0, 0], true; "every CCD at zero")]
fn r7_all_core_failure_skips_ccd_at_zero(name: &str, profile: [i64; 4], dead: bool) {
    let mut j = Journal::r7_session();
    j.r7_fact(
        false,
        &[0, 1, 2, 3],
        &profile,
        &R7Report::default().top(&[1, 2]),
    );
    if dead {
        j.at_next_dead_end();
        return;
    }
    let (action, moved) = j.at_next_backoff();
    assert_eq!(moved, (2, -29), "{name}");
    j.decide(action);
    assert_eq!(j.run().r7_decision(), None, "{name}");
    j.assert_resumes();
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
            named_ccd: CcdId::new(1).unwrap(),
            loaded_ccd: CcdId::new(0).unwrap(),
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

#[test_case::test_case("named core shallower", [-30, -30, -30, -30], [-35, -30, -30, -30], Some(0); "named core shallower")]
#[test_case::test_case("top requester shallower", [-30, -30, -30, -30], [-35, -35, -30, -30], None; "top requester shallower")]
#[test_case::test_case("top requester now at CO 0", [0, -30, -30, -30], [-30, -30, -30, -30], None; "top requester now at CO 0")]
fn r7_failure_needs_no_move_while_moved_core_is_shallower(
    name: &str,
    current: [i64; 4],
    failed: [i64; 4],
    named: Option<u8>,
) {
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

#[test]
fn r7_failure_needs_no_move_during_a_deepening_round_while_the_named_core_is_shallower() {
    let mut j = Journal::new(&[0, 1], &[Start::solo(-10, -11), Start::solo(-10, -11)]);
    j.profile_change(&[-10, -10]);
    j.cycle_start(1, &[Regime::R1]);
    j.cycle_end(1, true, true);
    j.ranking(&[0, 1]);
    let start = j.round_start(1, &[-10, -11], &[1], 5);
    let failed = [-12, -10];
    let failure = j.r7_fact(
        false,
        &[0, 1],
        &failed,
        &R7Report::default().top(&[0]).named(0),
    );
    let round_open = |j: &Journal| j.run().rounds.active().map(crate::rounds::Round::start);
    assert_eq!(j.run().r7_decision(), None);
    let held = loop {
        match j.state.next().ok() {
            Some(Action::Decide(step))
                if !matches!(&step.payload, DecisionPayload::DeepeningRound(_))
                    && !matches!(&step.payload, DecisionPayload::TunerDecision(td) if td.decision == Decision::Backoff) =>
            {
                j.decide(step);
            }
            other
            @ (Some(Action::Decide(_) | Action::RunTrial(_) | Action::ReadRanking) | None) => {
                break other;
            }
        }
    };
    assert!(
        matches!(held, Some(Action::RunTrial(_))),
        "core 00 already shallower than in the failed trial, yet {held:?}"
    );
    assert_eq!(round_open(&j), Some(start));
    j.assert_resumes();

    j.at_set_offset(0, failed[0]);
    assert_eq!(round_open(&j), Some(start));
    let action = j.next_decision();
    let DecisionPayload::DeepeningRound(end) = &action.payload else {
        panic!("expected the round to end before the backoff, got {action:?}");
    };
    assert_eq!(end.event, togi_journal::RoundEvent::End);
    assert!(!end.passed);
    assert_eq!(action.cause, [failure]);
    j.decide(action);
    assert_eq!(round_open(&j), None);
    let (_, moved) = j.at_next_backoff();
    assert_eq!(moved, (0, failed[0] + 1));
    j.assert_resumes();
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

#[test_case::test_case(false, None; "not local_mce; None")]
#[test_case::test_case(false, Some(5000); "not local_mce; Some(5000)")]
#[test_case::test_case(true, None; "local_mce; None")]
#[test_case::test_case(true, Some(5000); "local_mce; Some(5000)")]
fn r7_mce_named_unloaded_core_uses_its_lane(local_mce: bool, clock: Option<u32>) {
    let ids = [0, 1, 2, 3];
    let infos = ids
        .iter()
        .map(|&id| togi_machine::CoreInfo {
            core: core(id),
            ccd: CcdId::new(0).unwrap(),
            cpus: Vec::new(),
        })
        .collect();
    let mut j = Journal::on(&ids, infos, &[Start::has_room(-30); 4]);
    let all = [-30; 4];
    j.profile_change(&all);
    j.ranking(&ids);
    j.load(&crate::testkit::config(r#"{"checking":{"cycle":["R7"]}}"#));
    let target = if local_mce { 1.11 } else { 1.1 };
    let passes: Vec<_> = (0..j.at_needed())
        .map(|_| {
            j.r7_fact(
                true,
                &[2, 3],
                &[-30, -30, -20, -30],
                &requests(&[(2, target), (3, 1.09)])
                    .top(&[2])
                    .clocks(&[(0, 5000)]),
            )
        })
        .collect();
    j.open_default_cycle();
    // Complete the full load so its real chain idles core 3. Its request lane remains
    // available, but this different load cannot supply the partial's missing clock.
    let mut measured = None;
    for _ in 0..4 {
        let plan = j.next_trial();
        assert_eq!(plan.load.cores(), set(&ids));
        let (intent, trial) = j.intent(&plan);
        let mut end = Journal::end_payload(&trial, Outcome::Pass, None);
        end.duration_s = plan.duration.get();
        end.voltage_requests_v = [
            (core(0), 1.08),
            (core(1), 1.07),
            (core(2), target - 10.0 * togi_requests::VOLTS_PER_COUNT),
            (core(3), 1.09),
        ]
        .into_iter()
        .collect();
        measured = Some(j.append(end.into(), &[intent]));
    }
    let chain = j.next_decision();
    let DecisionPayload::CheckingChain(partial) = &chain.payload else {
        panic!("expected the partial idling core 3, got {chain:?}");
    };
    assert_eq!(partial.cores, set(&[0, 1, 2]));
    j.decide(chain);
    let plan = j.next_trial();
    assert_eq!(plan.load.cores(), set(&[0, 1, 2]));
    let (intent, trial) = j.intent(&plan);
    let mut cause = vec![intent];
    let signal = if local_mce {
        cause.push(j.at_mce(&trial, 3));
        Signal::UncorrectedMce
    } else {
        Signal::ComputationError
    };
    let mut end = Journal::end_payload(&trial, Outcome::Failure, Some(signal));
    end.duration_s = 120;
    end.core = (!local_mce).then_some(core(3));
    if let Some(mhz) = clock {
        end.ccd_mhz.insert(CcdId::new(0).unwrap(), mhz);
    }
    j.append(end.into(), &cause);
    let attribution = j.next_decision();
    let DecisionPayload::Failure(failure) = &attribution.payload else {
        panic!("expected attribution, mce={local_mce}, clock={clock:?}: {attribution:?}");
    };
    assert_eq!((failure.signal, failure.core), (signal, Some(core(3))));
    j.decide(attribution);
    let (action, why) = j.at_r7_backoff();
    let counts = if clock.is_none() {
        1
    } else if local_mce {
        6
    } else {
        3
    };
    assert_eq!(why.counts, counts, "mce={local_mce}, clock={clock:?}");
    assert_eq!(
        why.basis,
        if clock.is_some() {
            Basis::Toward {
                request: 1.09,
                target,
            }
        } else {
            Basis::NoPass { request: 1.09 }
        }
    );
    assert_eq!(j.next_decision(), action);
    let moved = decision(&action);
    assert_eq!((moved.core, moved.to_offset.get()), (core(3), -30 + counts));
    assert!(action.cause.contains(&measured.unwrap()));
    for pass in passes {
        assert_eq!(action.cause.contains(&pass), clock.is_some());
    }
    j.assert_resumes();
}

#[test]
fn r7_tied_charge_requests_ranking_before_selecting_a_core() {
    let ids = [0, 1, 2, 3];
    let mut j = Journal::on(&ids, pairs(&ids), &[Start::has_room(-30); 4]);
    j.profile_change(&[-30; 4]);
    j.r7_fact(false, &[0, 1], &[-30; 4], &R7Report::default().top(&[0, 1]));
    assert_eq!(j.run().r7_decision(), Some(R7Response::ReadRanking));
    assert_eq!(
        j.run().r7_decision().unwrap().into_action(),
        Action::ReadRanking
    );
    j.ranking(&[0, 1, 2, 3]);
    let (action, moved) = j.at_next_backoff();
    assert_eq!(moved, (1, -29));
    j.decide(action);
    assert_eq!(j.run().r7_decision(), None);
    j.assert_resumes();
}

#[test_case::test_case(2, 0, "R7 failure naming core 02 at CO 0 on CCD 1, which had no loaded core"; "(named, top, subject) scenario 1")]
#[test_case::test_case(0, 1, "core 00 failed at CO 0"; "(0, 1, core 00 failed at CO 0)")]
fn r7_zero_dead_end_explains_the_named_cores_ccd_route(named: u8, top: u8, subject: &str) {
    let mut j = Journal::r7_session();
    j.profile_change(&[0; 4]);
    let failure = j.r7_fact(
        false,
        &[0, 1],
        &[0; 4],
        &R7Report::default()
            .requests(&[(0, 1.1), (1, 1.2)])
            .top(&[top])
            .named(named),
    );
    let dead = j.at_next_dead_end();
    let DecisionPayload::DeadEnd(end) = &dead.payload else {
        unreachable!()
    };
    assert!(end.detail.contains(subject), "{}", end.detail);
    assert!(dead.cause.contains(&failure));
    j.assert_resumes();
}

#[test]
fn r7_carried_failures_move_before_any_trial() {
    let mut j = Journal::r7_session();
    let at = j.at_values();
    for _ in 0..4 {
        j.r7_fact(false, &[0, 1], &at, &R7Report::default().top(&[0]).named(1));
    }
    let Action::Decide(action) = j.next() else {
        panic!("carried failures ran a trial before moving");
    };
    let td = decision(&action);
    assert_eq!(
        (td.decision, td.core.get(), td.to_offset.get()),
        (Decision::Backoff, 1, -29)
    );
    j.assert_resumes();
}
