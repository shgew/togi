use togi_journal::{Outcome, Seq};
use togi_machine::{Regime, WorkloadId};

use crate::r7::order::RequestOrder;
use crate::r7::reason::{Basis, Note, R7Reason};
use crate::r7::respond::R7Response;
use crate::testkit::{Journal, R7Report, set};
use crate::tests::core;
use crate::{DecisionAction, DecisionPayload, LoadKey};

const ALL_30: [i64; 4] = [-30, -30, -30, -30];

fn needed(j: &Journal) -> usize {
    usize::from(j.run().config.needed.get())
}

fn report(requests: &[(u8, f64)], top: &[u8]) -> R7Report {
    R7Report::default().requests(requests).top(top)
}

fn passes(
    j: &mut Journal,
    count: usize,
    cores: &[u8],
    profile: &[i64],
    report: &R7Report,
) -> Vec<Seq> {
    (0..count)
        .map(|_| j.r7_fact(true, cores, profile, report))
        .collect()
}

fn r7_backoff(j: &Journal) -> (DecisionAction, R7Reason) {
    match j.run().r7_decision() {
        Some(R7Response::Backoff { decision, why }) => (decision, why),
        other => panic!("expected a voltage-targeted backoff, got {other:?}"),
    }
}

fn moved(decision: &DecisionAction) -> (u8, i64) {
    let DecisionPayload::TunerDecision(td) = &decision.payload else {
        panic!("expected a tuner decision, got {decision:?}");
    };
    (td.core.get(), td.to_offset.get())
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

fn assert_toward(why: &R7Reason, request: f64, target: f64) {
    match why.basis {
        Basis::Toward {
            request: r,
            target: t,
        } if close(r, request) && close(t, target) => {}
        ref other => panic!("expected {request} V toward {target} V, got {other:?}"),
    }
}

fn assert_no_pass(why: &R7Reason, request: f64) {
    match why.basis {
        Basis::NoPass { request: r } if close(r, request) => {}
        ref other => panic!("expected no qualifying pass at {request} V, got {other:?}"),
    }
}

#[test]
fn short_crash_move_still_escapes_a_shallower_failure_point_and_clamps_at_zero() {
    use crate::r7::target::backoff_move;
    use crate::tests::off as target;

    assert_eq!(
        backoff_move(target(-30), Some(target(-20)), 1).to,
        target(-19)
    );
    assert_eq!(backoff_move(target(-1), None, 1).to, target(0));
}

#[test]
fn detected_short_crash_with_a_preserved_backend_signal_takes_no_target() {
    let mut j = Journal::r7_session();
    let n = needed(&j);
    passes(
        &mut j,
        n,
        &[0, 1],
        &[-20, -30, -30, -30],
        &report(&[(0, 1.136), (1, 1.08)], &[0]),
    );
    let plan = j.r7_plan(&[0, 1], &ALL_30);
    let (intent, trial) = j.intent(&plan);
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
    let mut end = Journal::end_payload(
        &trial,
        Outcome::Failure,
        Some(togi_machine::Signal::ComputationError),
    );
    end.duration_s = 15;
    end.core = Some(core(0));
    end.voltage_requests_v = [(core(0), 1.1), (core(1), 1.08)].into_iter().collect();
    end.top_requesters = set(&[0]);
    j.append(end.into(), &[intent]);
    let attribution = j.next_decision();
    j.decide(attribution);
    let (decision, why) = r7_backoff(&j);
    assert_eq!(moved(&decision), (0, -29));
    assert_eq!(
        why.basis,
        Basis::ShortCrash {
            request: 1.1,
            duration_s: 15,
        }
    );
    assert_eq!(j.resumed().next_decision(), decision);
}

#[test]
fn short_crash_duration_floor_applies_to_named_and_unattributed_failures() {
    for named in [false, true] {
        for duration in [0, 15, 32, 47, 59, 60, 65, 99, 117] {
            for signal in [
                togi_machine::Signal::ComputationError,
                togi_machine::Signal::UnexpectedExit,
                togi_machine::Signal::Crash,
            ] {
                let mut j = Journal::r7_session();
                let n = needed(&j);
                let passed = passes(
                    &mut j,
                    n,
                    &[0, 1],
                    &[-20, -30, -30, -30],
                    &report(&[(0, 1.136), (1, 1.08)], &[0]),
                );
                let mut failing = report(&[(0, 1.1), (1, 1.08)], &[0]);
                failing.named = named.then_some(0);
                failing.duration_s = Some(duration);
                failing.signal = Some(signal);
                j.r7_failed(&[0, 1], &ALL_30, &failing);
                let (decision, why) = r7_backoff(&j);
                let short = signal == togi_machine::Signal::Crash && duration < 60;
                assert_eq!(moved(&decision), (0, if short { -29 } else { -20 }));
                if short {
                    assert_eq!(
                        why.basis,
                        Basis::ShortCrash {
                            request: 1.1,
                            duration_s: duration,
                        }
                    );
                    assert_eq!(why.counts, 1);
                    assert!(passed.iter().all(|seq| !decision.cause.contains(seq)));
                    let DecisionPayload::TunerDecision(td) = &decision.payload else {
                        unreachable!()
                    };
                    assert!(
                        td.reason
                            .contains("below the 60 s floor, so no voltage target")
                    );
                } else {
                    assert_toward(&why, 1.1, 1.136);
                }
                assert_eq!(j.next_decision(), decision);
                assert_eq!(j.resumed().next_decision(), decision);
            }
        }
    }
}

#[test]
fn short_crash_preserves_stepdown_minimum_without_a_target() {
    let mut j = Journal::r7_session();
    let n = needed(&j);
    passes(
        &mut j,
        n,
        &[0, 1],
        &[0, -10, -30, -30],
        &report(&[(0, 1.236), (1, 1.15)], &[0]),
    );
    let profile = [0, -30, -30, -30];
    j.profile_change(&profile);
    let mut failing = report(&[(0, 1.2), (1, 1.15)], &[0]);
    failing.signal = Some(togi_machine::Signal::Crash);
    failing.duration_s = Some(15);
    j.r7_failed(&[0, 1], &profile, &failing);
    let (decision, why) = r7_backoff(&j);
    assert_eq!(moved(&decision), (1, -16));
    assert_eq!(why.counts, 14);
    assert_eq!(why.rail, Some(1.2));
    assert_eq!(
        why.basis,
        Basis::ShortCrash {
            request: 1.15,
            duration_s: 15,
        }
    );
    assert_eq!(j.resumed().next_decision(), decision);
}

#[test]
fn short_crash_without_requests_keeps_the_offset_proxy_fallback() {
    let mut j = Journal::r7_session();
    let mut failing = R7Report::default().top(&[0]);
    failing.signal = Some(togi_machine::Signal::Crash);
    failing.duration_s = Some(0);
    j.r7_failed(&[0, 1], &ALL_30, &failing);
    let (decision, why) = r7_backoff(&j);
    assert_eq!(moved(&decision), (0, -29));
    assert_eq!(why.basis, Basis::NoTelemetry { offset: -30 });
    assert_eq!(j.resumed().next_decision(), decision);
}

#[test]
fn a_carried_failure_takes_no_voltage_target() {
    for carried in [false, true] {
        let mut j = Journal::r7_session();
        let n = needed(&j);
        let pass = report(&[(0, 1.1539), (1, 1.08)], &[0]);
        passes(&mut j, n, &[0, 1], &[-20, -30, -30, -30], &pass);
        let failing = report(&[(0, 1.09447), (1, 1.08)], &[0]);
        if carried {
            j.r7_fact(false, &[0, 1], &ALL_30, &failing);
        } else {
            j.r7_failed(&[0, 1], &ALL_30, &failing);
        }
        let (decision, why) = r7_backoff(&j);
        if carried {
            assert_eq!(why.basis, Basis::Carried { request: 1.09447 });
            assert_eq!(moved(&decision), (0, -29));
        } else {
            assert_toward(&why, 1.09447, 1.1539);
            assert_eq!(moved(&decision), (0, -13));
        }
        assert_eq!(j.next_decision(), decision, "carried {carried}");
    }
}

#[test]
fn r7_voltage_target_clock_filter() {
    type Case<'a> = (&'a str, &'a [(u8, u32)], i64, Option<f64>);
    let cases: [Case<'_>; 3] = [
        ("equal clock", &[(0, 5000)], -20, Some(1.136)),
        ("lower clock", &[(0, 4900)], -29, None),
        ("missing pass clock", &[], -29, None),
    ];
    for (name, clocks, want, target) in cases {
        let mut j = Journal::r7_session();
        let pass = report(&[(0, 1.136), (1, 1.08)], &[0]).clocks(clocks);
        let n = needed(&j);
        passes(&mut j, n, &[0, 1], &[-20, -30, -30, -30], &pass);
        let failing = report(&[(0, 1.1), (1, 1.08)], &[0])
            .named(0)
            .clocks(&[(0, 5000)]);
        j.r7_failed(&[0, 1], &ALL_30, &failing);
        let (decision, why) = r7_backoff(&j);
        assert_eq!(moved(&decision), (0, want), "{name}");
        match target {
            Some(target) => assert_toward(&why, 1.1, target),
            None => assert_no_pass(&why, 1.1),
        }
        assert_eq!(j.next_decision(), decision, "{name}");
    }
}

#[test]
fn r7_request_order_shifts_and_falls_back() {
    let mut j = Journal::r7_session();
    let source = j.r7_fact(
        true,
        &[0, 1],
        &ALL_30,
        &report(&[(0, 1.08), (1, 1.1)], &[1]),
    );
    let run = j.run();
    let w0 = WorkloadId::new(Regime::R7, 0).unwrap();
    let shifted = RequestOrder::derive(
        &run.r7.measurements,
        &run.topo,
        LoadKey::new(w0, set(&[0])),
        &j.profile(&[-20, -30, -30, -30]),
    );
    assert!(close(shifted.volts(core(0)).unwrap(), 1.116));
    assert_eq!(shifted.sources(), [source]);
    let w1 = WorkloadId::new(Regime::R7, 1).unwrap();
    let fallback = RequestOrder::derive(
        &run.r7.measurements,
        &run.topo,
        LoadKey::new(w1, set(&[0, 1])),
        &j.profile(&ALL_30),
    );
    assert_eq!(fallback.top(), set(&[0, 1]));
    assert!(fallback.is_offset_only());
}

#[test]
fn r7_unattributed_voltage_target() {
    let mut j = Journal::r7_session();
    let n = needed(&j);
    passes(
        &mut j,
        n,
        &[0, 1],
        &[-26, -30, -30, -30],
        &report(&[(0, 1.115), (1, 1.08)], &[0]),
    );
    j.r7_failed(&[0, 1], &ALL_30, &report(&[(0, 1.1), (1, 1.08)], &[0]));
    let (decision, why) = r7_backoff(&j);
    assert_eq!(moved(&decision), (0, -25));
    assert_toward(&why, 1.1, 1.115);
    assert_eq!(why.counts, 5);
}

#[test]
fn r7_tied_top_target_must_beat_the_ccd_maximum() {
    for (pass_top, admitted) in [(1.1, false), (1.104, true)] {
        let mut j = Journal::r7_session();
        let n = needed(&j);
        passes(
            &mut j,
            n,
            &[0, 1],
            &[-26, -30, -30, -30],
            &report(&[(0, pass_top), (1, 1.08)], &[0]),
        );
        j.r7_failed(&[0, 1], &ALL_30, &report(&[(0, 1.1), (1, 1.0991)], &[0, 1]));
        let (decision, why) = r7_backoff(&j);
        assert_eq!(why.core, core(1), "{pass_top}");
        if admitted {
            assert_toward(&why, 1.0991, pass_top);
            assert_eq!(moved(&decision), (1, -28));
        } else {
            assert_no_pass(&why, 1.0991);
            assert_eq!(moved(&decision), (1, -29));
        }
    }
}

#[test]
fn r7_named_target_transfers_across_loads_without_fail_clock() {
    let mut j = Journal::r7_session();
    let n = needed(&j);
    passes(
        &mut j,
        n,
        &[0, 1, 2, 3],
        &[-30, -20, -30, -30],
        &report(&[(0, 1.09), (1, 1.118), (2, 1.1), (3, 1.09)], &[1, 2]),
    );
    j.r7_failed(
        &[0, 1],
        &ALL_30,
        &report(&[(0, 1.1), (1, 1.09)], &[0]).named(0),
    );
    let (decision, why) = r7_backoff(&j);
    assert_eq!(moved(&decision), (0, -25));
    assert_toward(&why, 1.1, 1.118);
}

#[test]
fn r7_step_down_target_exceeds_original_top() {
    let mut j = Journal::r7_session();
    let n = needed(&j);
    for _ in 0..n {
        j.r7_fact(
            true,
            &[0, 1],
            &[0, -20, -30, -30],
            &report(&[(0, 1.19), (1, 1.15)], &[0]),
        );
        j.r7_fact(
            true,
            &[0, 1],
            &[0, -10, -30, -30],
            &report(&[(0, 1.236), (1, 1.15)], &[0]),
        );
    }
    let profile = [0, -30, -30, -30];
    j.profile_change(&profile);
    j.r7_failed(&[0, 1], &profile, &report(&[(0, 1.2), (1, 1.15)], &[0]));
    let (decision, why) = r7_backoff(&j);
    assert_eq!(moved(&decision), (1, -6));
    assert_toward(&why, 1.15, 1.236);
    assert!(why.rail.is_some_and(|rail| close(rail, 1.2)));
    assert!(
        why.notes
            .iter()
            .any(|note| matches!(note, Note::SteppedDown { .. }))
    );
}

#[test]
fn r7_failure_uses_requests_as_of_its_start() {
    let mut j = Journal::r7_session();
    let before = j.r7_fact(true, &[0, 1], &ALL_30, &report(&[(0, 1.2), (1, 1.1)], &[0]));
    j.r7_failed(&[0, 1], &ALL_30, &R7Report::default());
    let after = j.r7_fact(true, &[0, 1], &ALL_30, &report(&[(0, 1.1), (1, 1.2)], &[1]));
    let (decision, why) = r7_backoff(&j);
    assert_eq!(moved(&decision).0, 0);
    assert_no_pass(&why, 1.2);
    assert!(decision.cause.contains(&before));
    assert!(!decision.cause.contains(&after));
    j.decide(decision);
    assert_eq!(j.run().r7_decision(), None);
    j.assert_resumes();
}

#[test]
fn r7_voltage_target_cites_its_lowest_pass() {
    let mut j = Journal::r7_session();
    let passed = [-20, -30, -30, -30];
    let n = needed(&j);
    passes(
        &mut j,
        n,
        &[0, 1],
        &passed,
        &report(&[(0, 1.136), (1, 1.08)], &[0]),
    );
    let lowest = j.r7_fact(
        true,
        &[0, 1],
        &passed,
        &report(&[(0, 1.118), (1, 1.08)], &[0]),
    );
    j.r7_failed(&[0, 1], &ALL_30, &report(&[(0, 1.1), (1, 1.08)], &[0]));
    let (decision, why) = r7_backoff(&j);
    assert_eq!(moved(&decision), (0, -25));
    assert_toward(&why, 1.1, 1.118);
    assert!(decision.cause.contains(&lowest));
}

#[test]
fn r7_backoff_counts_each_carried_fact_once() {
    let mut j = Journal::r7_session();
    let failure = j.r7_fact(
        false,
        &[0, 1],
        &ALL_30,
        &report(&[(0, 1.1), (1, 1.08)], &[0]),
    );
    let (decision, why) = r7_backoff(&j);
    assert_eq!(why.failure, failure);
    assert_eq!(
        decision.cause.iter().filter(|&&seq| seq == failure).count(),
        1
    );
    let clause = crate::ledger::carried_clause(&j.run().evidence, &decision.cause);
    assert!(
        clause.starts_with("; 1 carried facts from session "),
        "{clause}"
    );
    let DecisionPayload::TunerDecision(td) = &decision.payload else {
        unreachable!()
    };
    assert_eq!(td.reason, format!("{why}{clause}"));
}

#[test]
fn r7_offset_stand_in_moves_one_count() {
    struct Case {
        name: &'static str,
        earlier: Option<&'static [(u8, f64)]>,
        cores: &'static [u8],
        top: &'static [u8],
        stalled: Option<u8>,
        pass_profile: [i64; 4],
        passes: &'static [(u8, f64)],
        core: u8,
    }
    let cases = [
        Case {
            name: "no measurement as of the failure",
            earlier: None,
            cores: &[0, 1],
            top: &[0],
            stalled: None,
            pass_profile: [-26, -30, -30, -30],
            passes: &[(0, 1.115), (1, 1.08)],
            core: 0,
        },
        Case {
            name: "measured CCD beside an unmeasured one",
            earlier: Some(&[(0, 1.2), (1, 1.15)]),
            cores: &[0, 1, 2, 3],
            top: &[],
            stalled: Some(3),
            pass_profile: [-30, -30, -26, -26],
            passes: &[(0, 1.1), (1, 1.08), (2, 1.2), (3, 1.19)],
            core: 3,
        },
    ];
    for case in cases {
        let mut j = Journal::r7_session();
        let earlier = case
            .earlier
            .map(|requests| j.r7_fact(true, &[0, 1], &ALL_30, &report(requests, &[0])));
        let mut failing = R7Report::default().top(case.top);
        failing.stalled = case.stalled;
        j.r7_fact(false, case.cores, &ALL_30, &failing);
        let n = needed(&j);
        let passed = passes(
            &mut j,
            n,
            case.cores,
            &case.pass_profile,
            &report(case.passes, &[]),
        );
        let (decision, why) = r7_backoff(&j);
        assert_eq!(moved(&decision), (case.core, -29), "{}", case.name);
        assert_eq!(why.counts, 1, "{}", case.name);
        let want = match earlier {
            None => Basis::NoTelemetry { offset: -30 },
            Some(source) => Basis::Uncovered {
                offset: -30,
                sources: vec![source],
            },
        };
        assert_eq!(why.basis, want, "{}", case.name);
        assert!(
            !decision.cause.iter().any(|seq| passed.contains(seq)),
            "{}",
            case.name
        );
    }
}

#[test]
fn r7_unloaded_named_reset_keeps_measurement() {
    for (reset, kept) in [(3, true), (0, false)] {
        let mut j = Journal::r7_session();
        let source = j.r7_fact(
            true,
            &[0, 1],
            &ALL_30,
            &report(&[(0, 1.08), (1, 1.1)], &[1]),
        );
        j.r7_failed(&[0, 1], &ALL_30, &R7Report::default().named(3));
        let (decision, why) = r7_backoff(&j);
        assert_eq!(why.core, core(3));
        j.decide(decision);
        j.reset(reset);
        let failure = j.r7_failed(&[0, 1], &ALL_30, &R7Report::default());
        let order = &j.run().r7.open.get(&failure).unwrap().r7.order;
        let (decision, why) = r7_backoff(&j);
        assert_eq!(moved(&decision), (1, -29), "reset {reset}");
        if kept {
            assert_eq!(order.sources(), [source]);
            assert_no_pass(&why, 1.1);
            assert!(decision.cause.contains(&source));
        } else {
            assert!(order.is_offset_only());
            assert_eq!(why.basis, Basis::NoTelemetry { offset: -30 });
            assert!(!decision.cause.contains(&source));
        }
        j.assert_resumes();
    }
}

#[test]
fn r7_target_tie_earliest_profile() {
    let first = [-26, -30, -30, -30];
    let second = [-30, -26, -30, -30];
    let at_first = report(&[(0, 1.115), (1, 1.08)], &[0]);
    let at_second = report(&[(0, 1.08), (1, 1.115)], &[1]);
    for second_leads in [false, true] {
        let mut j = Journal::r7_session();
        let n = needed(&j);
        let (lead, lead_report, follow, follow_report) = if second_leads {
            (second, &at_second, first, &at_first)
        } else {
            (first, &at_first, second, &at_second)
        };
        let mut leading = vec![j.r7_fact(true, &[0, 1], &lead, lead_report)];
        let followed = passes(&mut j, n, &[0, 1], &follow, follow_report);
        leading.extend(passes(&mut j, n - 1, &[0, 1], &lead, lead_report));
        j.r7_failed(&[0, 1], &ALL_30, &report(&[(0, 1.1), (1, 1.08)], &[0]));
        let (decision, why) = r7_backoff(&j);
        assert_eq!(moved(&decision), (0, -25), "second leads {second_leads}");
        assert_toward(&why, 1.1, 1.115);
        let cited: Vec<Seq> = decision
            .cause
            .iter()
            .copied()
            .filter(|seq| leading.contains(seq) || followed.contains(seq))
            .collect();
        assert_eq!(cited, leading, "second leads {second_leads}");
    }
}

#[test]
fn r7_target_cites_late_minimum() {
    let mut j = Journal::r7_session();
    let passed = [-20, -30, -30, -30];
    let n = needed(&j);
    let first = passes(
        &mut j,
        n,
        &[0, 1],
        &passed,
        &report(&[(0, 1.136), (1, 1.08)], &[0]),
    );
    let middle = j.r7_fact(
        true,
        &[0, 1],
        &passed,
        &report(&[(0, 1.13), (1, 1.08)], &[0]),
    );
    let lowest = j.r7_fact(
        true,
        &[0, 1],
        &passed,
        &report(&[(0, 1.118), (1, 1.08)], &[0]),
    );
    let last = j.r7_fact(
        true,
        &[0, 1],
        &passed,
        &report(&[(0, 1.12), (1, 1.08)], &[0]),
    );
    j.r7_failed(&[0, 1], &ALL_30, &report(&[(0, 1.1), (1, 1.08)], &[0]));
    let (decision, why) = r7_backoff(&j);
    assert_toward(&why, 1.1, 1.118);
    let cited: Vec<Seq> = decision
        .cause
        .iter()
        .copied()
        .filter(|seq| first.contains(seq) || [middle, lowest, last].contains(seq))
        .collect();
    let mut want = first[..n - 1].to_vec();
    want.push(lowest);
    assert_eq!(cited, want);
    assert!(!cited.contains(&middle) && !cited.contains(&last));
}

#[test]
fn r7_missing_lane_is_not_zero() {
    let mut j = Journal::r7_session();
    j.r7_fact(true, &[0, 1], &ALL_30, &R7Report::default().top(&[0]));
    assert_eq!(j.run().r7.targets.groups(), 0);
    let source = j.r7_fact(true, &[0, 1], &ALL_30, &report(&[(1, 1.1)], &[1]));
    j.r7_fact(false, &[0, 1], &ALL_30, &R7Report::default().top(&[0]));
    let (decision, why) = r7_backoff(&j);
    assert_eq!(moved(&decision), (0, -29));
    assert_eq!(
        why.basis,
        Basis::Uncovered {
            offset: -30,
            sources: vec![source],
        }
    );
    assert_eq!(why.counts, 1);
}

#[test]
fn v_star_ignores_an_invalidated_pass() {
    let q = [-26, -30, -30, -30];
    let at_q = report(&[(0, 1.115), (1, 1.08)], &[0]);
    let invalidated = |newer: usize| {
        let mut j = Journal::r7_session();
        let n = needed(&j);
        passes(&mut j, n, &[0, 1], &q, &at_q);
        j.r7_failed(&[0, 1], &q, &report(&[(0, 1.1), (1, 1.08)], &[0]));
        let (decision, why) = r7_backoff(&j);
        assert_eq!(moved(&decision), (0, -25));
        assert_no_pass(&why, 1.1);
        assert_eq!(why.counts, 1);
        j.decide(decision);
        let fresh = passes(&mut j, newer, &[0, 1], &q, &at_q);
        j.r7_failed(&[0, 1], &ALL_30, &report(&[(0, 1.08), (1, 1.1)], &[1]));
        let (decision, why) = r7_backoff(&j);
        assert_eq!(moved(&decision).0, 1);
        (j, decision, why, fresh)
    };
    let n = needed(&Journal::r7_session());
    let (_, _, why, _) = invalidated(n - 1);
    assert_no_pass(&why, 1.1);
    let (j, decision, why, fresh) = invalidated(n);
    assert_toward(&why, 1.1, 1.115);
    assert_eq!(moved(&decision), (1, -25));
    assert!(fresh.iter().all(|seq| decision.cause.contains(seq)));
    j.assert_resumes();
}

#[test]
fn v_star_returns_with_a_failure_the_reset_of_its_named_core_drops() {
    let q = [-26, -30, -30, -30];
    for (named, restored) in [(Some(3), true), (None, false)] {
        let mut j = Journal::r7_session();
        let n = needed(&j);
        let at_q = report(&[(0, 1.115), (1, 1.08)], &[0]);
        let old = passes(&mut j, n, &[0, 1], &q, &at_q);
        let failing = report(&[(0, 1.1), (1, 1.08)], &[0]);
        let failing = named.map_or(failing.clone(), |core| failing.named(core));
        j.r7_failed(&[0, 1], &q, &failing);
        j.reset(3);
        j.r7_failed(&[0, 1], &ALL_30, &report(&[(0, 1.1), (1, 1.08)], &[0]));
        let (decision, why) = r7_backoff(&j);
        assert_eq!(moved(&decision).0, 0, "{named:?}");
        if restored {
            assert_toward(&why, 1.1, 1.115);
            assert!(old.iter().all(|seq| decision.cause.contains(seq)));
        } else {
            assert_no_pass(&why, 1.1);
        }
        j.assert_resumes();
    }
}

#[test]
fn as_of_order_is_frozen() {
    let mut j = Journal::r7_session();
    let before = j.r7_fact(true, &[0, 1], &ALL_30, &report(&[(0, 1.2), (1, 1.1)], &[0]));
    j.r7_trial(&[0, 1], &ALL_30, Outcome::Failure, &R7Report::default());
    let between = j.r7_fact(true, &[0, 1], &ALL_30, &report(&[(0, 1.1), (1, 1.2)], &[1]));
    let attribution = j.next_decision();
    assert!(matches!(attribution.payload, DecisionPayload::Failure(_)));
    let failure = j.decide(attribution);
    let later = j.r7_fact(
        true,
        &[0, 1],
        &ALL_30,
        &report(&[(0, 1.05), (1, 1.25)], &[1]),
    );
    let order = &j.run().r7.open.get(&failure).unwrap().r7.order;
    assert_eq!(order.sources(), [before]);
    assert_eq!(order.top(), set(&[0]));
    let (decision, why) = r7_backoff(&j);
    assert_eq!(why.failure, failure);
    assert_eq!(why.core, core(0));
    assert_no_pass(&why, 1.2);
    assert!(decision.cause.contains(&before));
    assert!(!decision.cause.contains(&between) && !decision.cause.contains(&later));
    j.assert_resumes();
}

fn varied(i: usize) -> R7Report {
    let i = u32::try_from(i).unwrap();
    let wobble = 1.0 + f64::from(i) * 0.000_001;
    let other_ccd_mhz = 4000 + i;
    report(&[(0, 1.115), (1, wobble)], &[0]).clocks(&[(0, 5000), (1, other_ccd_mhz)])
}

fn pass_volume(count: usize) -> (usize, usize, usize, u64, u64, (u8, i64), usize) {
    let mut j = Journal::r7_session();
    let q = [-26, -30, -30, -30];
    for i in 0..count {
        j.r7_fact(true, &[0, 1], &q, &varied(i));
    }
    j.state.visits();
    j.r7_fact(true, &[0, 1], &q, &varied(count));
    let folding = j.state.visits();
    j.r7_failed(&[0, 1], &ALL_30, &report(&[(0, 1.1), (1, 1.08)], &[0]));
    j.state.visits();
    let decision = j.next_decision();
    let deciding = j.state.visits();
    let targets = &j.run().r7.targets;
    (
        targets.entries(),
        targets.segments(),
        needed(&j),
        folding,
        deciding,
        moved(&decision),
        decision.cause.len(),
    )
}

#[test]
fn r7_cost_pass_volume() {
    let small = pass_volume(100);
    let large = pass_volume(10_000);
    assert_eq!(small.0, 2);
    assert_eq!(small.1, 2);
    assert_eq!(small, large);
    assert_eq!(large.5, (0, -25));
}

#[test]
fn r7_target_cites_more_earliest_passes_after_the_needed_count_increases() {
    let mut j = Journal::r7_session();
    let q = [-26, -30, -30, -30];
    let passed = passes(
        &mut j,
        20,
        &[0, 1],
        &q,
        &report(&[(0, 1.115), (1, 1.08)], &[0]),
    );
    let before = needed(&j);
    j.load(&crate::testkit::config(
        r#"{"evidence":{"miss":0.01,"rate":0.5}}"#,
    ));
    let after = needed(&j);
    assert!(after > before && after <= passed.len());
    j.r7_failed(&[0, 1], &ALL_30, &report(&[(0, 1.1), (1, 1.08)], &[0]));
    let (decision, why) = r7_backoff(&j);
    assert_toward(&why, 1.1, 1.115);
    let cited: Vec<Seq> = decision
        .cause
        .iter()
        .copied()
        .filter(|seq| passed.contains(seq))
        .collect();
    assert_eq!(cited, passed[..after]);
    j.assert_resumes();
}
