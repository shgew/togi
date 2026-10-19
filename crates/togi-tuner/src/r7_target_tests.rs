use togi_journal::{Outcome, Seq};
use togi_machine::{BankType, Condition, CpuId, Regime, Signal, WorkloadId};

use crate::r7::order::RequestOrder;
use crate::r7::reason::{Basis, Note, R7Reason};
use crate::r7::respond::R7Response;
use crate::testkit::{Journal, MPRIME_NEW, MPRIME_OLD, R7Report, YCRUNCHER_OLD, backends, set};
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
    let response = j.run().r7_decision();
    let Some(R7Response::Backoff { decision, why }) = response else {
        panic!("expected a voltage-targeted backoff, got {response:?}");
    };
    (decision, why)
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
    let Basis::Toward {
        request: r,
        target: t,
    } = why.basis
    else {
        panic!(
            "expected {request} V toward {target} V, got {:?}",
            why.basis
        );
    };
    assert!(
        close(r, request) && close(t, target),
        "expected {request} V toward {target} V, got {:?}",
        why.basis
    );
}

fn assert_no_pass(why: &R7Reason, request: f64) {
    let Basis::NoPass { request: r } = why.basis else {
        panic!(
            "expected no qualifying pass at {request} V, got {:?}",
            why.basis
        );
    };
    assert!(
        close(r, request),
        "expected no qualifying pass at {request} V, got {:?}",
        why.basis
    );
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

#[test_case::test_case(i64::MIN, -49; "negative counts")]
#[test_case::test_case(0, -49; "zero counts")]
#[test_case::test_case(1, -49; "one count")]
#[test_case::test_case(255, 0; "u8 limit")]
#[test_case::test_case(256, 0; "u8 overflow")]
#[test_case::test_case(i64::MAX, 0; "largest count")]
fn backoff_count_conversion_saturates_without_crossing_offset_bounds(counts: i64, want: i64) {
    use crate::r7::target::backoff_move;
    use crate::tests::off;

    assert_eq!(backoff_move(off(-50), None, counts).to, off(want));
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
                    &report(&[(0, 1.136), (1, 1.08)], &[0]).clocks(&[(0, 5000)]),
                );
                let mut failing = report(&[(0, 1.1), (1, 1.08)], &[0]).clocks(&[(0, 5000)]);
                failing.named = named.then_some(0);
                failing.duration_s = Some(duration);
                failing.signal = Some(signal);
                j.r7_failed(&[0, 1], &ALL_30, &failing);
                let (decision, why) = r7_backoff(&j);
                let short = signal == togi_machine::Signal::Crash && duration < 60;
                let capped = named && signal != togi_machine::Signal::Crash;
                let want = if short {
                    -29
                } else if capped {
                    -27
                } else {
                    -20
                };
                assert_eq!(moved(&decision), (0, want));
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
        ("equal clock", &[(0, 5000)], -27, Some(1.136)),
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
fn r7_named_target_transfers_within_ccd_using_latest_load_clock() {
    let mut j = Journal::r7_session();
    let n = needed(&j);
    passes(
        &mut j,
        n,
        &[0, 1],
        &[-30, -20, -30, -30],
        &report(&[(0, 1.09), (1, 1.118)], &[1]).clocks(&[(0, 5000)]),
    );
    let measured = j.r7_fact(
        true,
        &[0, 1, 2, 3],
        &ALL_30,
        &report(&[(0, 1.1), (1, 1.09), (2, 1.3), (3, 1.2)], &[0, 2])
            .clocks(&[(0, 5000), (1, 4900)]),
    );
    j.r7_failed(
        &[0, 1, 2, 3],
        &ALL_30,
        &report(&[(0, 1.1), (1, 1.09), (2, 1.3), (3, 1.2)], &[0, 2]).named(0),
    );
    let (decision, why) = r7_backoff(&j);
    assert_eq!(moved(&decision), (0, -27));
    assert_toward(&why, 1.1, 1.118);
    assert!(decision.cause.contains(&measured));
    j.assert_resumes();
}

#[test]
fn named_missing_clock_uses_latest_same_load_not_another_load_or_later_measurement() {
    for (last_clock, expected) in [(None, None), (Some(5000), Some(1.136)), (Some(5100), None)] {
        let mut j = Journal::r7_session();
        let n = needed(&j);
        passes(
            &mut j,
            n,
            &[0, 1],
            &[-20, -30, -30, -30],
            &report(&[(0, 1.136), (1, 1.08)], &[0]).clocks(&[(0, 5000)]),
        );
        // The failing load differs from the target load. Its newest reading is independent.
        if let Some(clock) = last_clock {
            j.r7_fact(
                true,
                &[0, 1, 2, 3],
                &ALL_30,
                &report(&[(0, 1.1), (1, 1.08), (2, 1.2), (3, 1.1)], &[0, 2])
                    .clocks(&[(0, clock), (1, 4900)]),
            );
        }
        j.r7_failed(
            &[0, 1, 2, 3],
            &ALL_30,
            &report(&[(0, 1.1), (1, 1.08), (2, 1.2), (3, 1.1)], &[0, 2]).named(0),
        );
        j.r7_fact(
            true,
            &[0, 1, 2, 3],
            &ALL_30,
            &report(&[(0, 1.1), (1, 1.08), (2, 1.2), (3, 1.1)], &[0, 2]).clocks(&[(0, 5000)]),
        );
        let (_, why) = r7_backoff(&j);
        match expected {
            Some(target) => assert_toward(&why, 1.1, target),
            None => assert_no_pass(&why, 1.1),
        }
        j.assert_resumes();
    }
}

#[test]
fn named_missing_clock_uses_the_restored_backends_own_latest_load_clock() {
    let wide = [0, 1, 2, 3];
    let lanes = [(0, 1.1), (1, 1.08), (2, 1.2), (3, 1.1)];
    let (a, b) = (
        backends(MPRIME_OLD, YCRUNCHER_OLD),
        backends(MPRIME_NEW, YCRUNCHER_OLD),
    );
    for (a_clock, b_clock, expected) in [
        (Some(5000), 5100, Some(1.136)),
        (Some(5100), 5000, None),
        (None, 5000, None),
    ] {
        let name = format!("A {a_clock:?}, B {b_clock}");
        let mut j = Journal::r7_session();
        j.load(&a);
        for _ in 0..needed(&j) {
            j.r7_trial(
                &[0, 1],
                &[-20, -30, -30, -30],
                Outcome::Pass,
                &report(&[(0, 1.136), (1, 1.08)], &[0]).clocks(&[(0, 5000)]),
            );
        }
        let a_source = a_clock.map(|clock| {
            j.r7_trial(
                &wide,
                &ALL_30,
                Outcome::Pass,
                &report(&lanes, &[0, 2]).clocks(&[(0, clock), (1, 4900)]),
            )
        });
        j.load(&b);
        let b_source = j.r7_trial(
            &wide,
            &ALL_30,
            Outcome::Pass,
            &report(&lanes, &[0, 2]).clocks(&[(0, b_clock), (1, 4900)]),
        );
        j.load(&a);
        j.r7_failed(&wide, &ALL_30, &report(&lanes, &[0, 2]).named(0));
        let (decision, why) = r7_backoff(&j);
        match expected {
            Some(target) => assert_toward(&why, 1.1, target),
            None => assert_no_pass(&why, 1.1),
        }
        assert_eq!(
            a_source.is_some_and(|source| decision.cause.contains(&source)),
            a_clock.is_some(),
            "{name}: A's frozen clock source is cited whenever A supplied one"
        );
        assert!(!decision.cause.contains(&b_source), "{name}: B's clock");
        assert_eq!(j.next_decision(), decision, "{name}");
        j.assert_resumes();
    }
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
    let DecisionPayload::TunerDecision(td) = &decision.payload else {
        unreachable!()
    };
    let clause = td
        .reason
        .strip_prefix(&why.to_string())
        .expect("the reason starts with the backoff's why");
    assert!(
        clause.starts_with("; 1 carried facts from session "),
        "{clause}"
    );
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

fn named_cap_passes(j: &mut Journal, target: f64) -> Vec<Seq> {
    let n = needed(j);
    passes(
        j,
        n,
        &[0, 1],
        &[0, -20, -30, -30],
        &report(&[(0, 1.08), (1, target)], &[1]).clocks(&[(0, 5000)]),
    )
}

fn named_cap_failure(j: &mut Journal, signal: Signal, named: Option<u8>, crashed: bool) {
    let values: Vec<i64> = j.offsets().iter().map(|(_, offset)| offset.get()).collect();
    let plan = j.r7_plan(&[0, 1], &values);
    let (intent, trial) = j.intent(&plan);
    let mut cause = vec![intent];
    if crashed {
        cause.push(
            j.append(
                togi_journal::CrashDetected {
                    previous_boot: togi_machine::BootId::from("boot".to_owned()),
                    in_flight: Some(intent),
                    stray: false,
                    condition: Some(Condition::Together),
                    reset_reason: None,
                    reset_reason_raw: Vec::new(),
                    inconclusive: false,
                    pstore: None,
                }
                .into(),
                &[intent],
            ),
        );
    }
    let mut end = Journal::end_payload(&trial, Outcome::Failure, Some(signal));
    end.duration_s = 120;
    end.core = named.map(core);
    end.top_requesters = set(&[0]);
    end.voltage_requests_v = [(core(0), 1.1), (core(1), 1.08)].into_iter().collect();
    end.ccd_mhz = [(togi_machine::CcdId::new(0).unwrap(), 5000)]
        .into_iter()
        .collect();
    j.append(end.into(), &cause);
    let attribution = j.next_decision();
    assert!(matches!(attribution.payload, DecisionPayload::Failure(_)));
    j.decide(attribution);
}

#[test]
fn named_non_crash_cap_keeps_exact_three_count_boundary_and_target_citations() {
    for (gap, uncapped, want) in [
        (1.0, 1, 1),
        (2.0, 2, 2),
        (3.0 - 0.0001, 3, 3),
        (3.0, 3, 3),
        (3.0 + 0.0001, 4, 3),
        (18.0, 18, 3),
    ] {
        let mut j = Journal::r7_session();
        let target = 1.1 + gap * togi_requests::VOLTS_PER_COUNT;
        let cited = named_cap_passes(&mut j, target);
        named_cap_failure(&mut j, Signal::ComputationError, Some(0), false);
        j.assert_resumes();
        let (decision, why) = r7_backoff(&j);
        assert_eq!(why.counts, want, "gap {gap}");
        assert_eq!(moved(&decision), (0, -30 + want), "gap {gap}");
        assert_toward(&why, 1.1, target);
        assert!(cited.iter().all(|seq| decision.cause.contains(seq)));
        let cap: Vec<_> = why
            .notes
            .iter()
            .filter(|note| matches!(note, Note::NamedNonCrashCap { .. }))
            .collect();
        if uncapped > 3 {
            assert_eq!(cap, [&Note::NamedNonCrashCap { uncapped, limit: 3 }]);
            assert!(why.to_string().contains("capped at 3 counts"));
        } else {
            assert_eq!(cap, Vec::<&Note>::new());
        }
        let DecisionPayload::TunerDecision(td) = &decision.payload else {
            unreachable!()
        };
        assert_eq!(
            td.failure_point.map(togi_machine::TargetOffset::get),
            Some(-30)
        );
        assert_eq!(j.next_decision(), decision);
        j.decide(decision);
        j.assert_resumes();
    }
}

#[test]
fn named_non_crash_cap_qualifies_signals_and_excludes_recovered_backend_failures() {
    for signal in [
        Signal::ComputationError,
        Signal::UnexpectedExit,
        Signal::Stall,
        Signal::CorrectedMce,
        Signal::UncorrectedMce,
        Signal::Crash,
    ] {
        for crashed in [false, true] {
            let mut j = Journal::r7_session();
            named_cap_passes(&mut j, 1.136);
            named_cap_failure(&mut j, signal, Some(0), crashed);
            let (decision, why) = r7_backoff(&j);
            let capped =
                !crashed && matches!(signal, Signal::ComputationError | Signal::UnexpectedExit);
            assert_eq!(
                why.counts,
                if capped { 3 } else { 10 },
                "{signal:?}/{crashed}"
            );
            assert_eq!(moved(&decision), (0, if capped { -27 } else { -20 }));
            j.assert_resumes();
        }
    }
}

#[test]
fn an_unattributed_non_crash_failure_keeps_its_full_target() {
    let mut j = Journal::r7_session();
    let n = needed(&j);
    passes(
        &mut j,
        n,
        &[0, 1],
        &[-20, -30, -30, -30],
        &report(&[(0, 1.136), (1, 1.08)], &[0]),
    );
    named_cap_failure(&mut j, Signal::ComputationError, None, false);
    let (decision, why) = r7_backoff(&j);
    assert_eq!(why.counts, 10);
    assert_eq!(moved(&decision), (0, -20));
    j.assert_resumes();
}

#[test]
fn an_mce_named_crash_keeps_its_full_target() {
    let mut j = Journal::r7_session();
    named_cap_passes(&mut j, 1.136);
    let plan = j.r7_plan(&[0, 1], &ALL_30);
    let (intent, trial) = j.intent(&plan);
    let mce = j.append(
        togi_journal::Mce {
            cpu: CpuId::from(0),
            core: Some(core(0)),
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
    );
    let mut end = Journal::end_payload(&trial, Outcome::Failure, Some(Signal::Crash));
    end.duration_s = 120;
    end.ccd_mhz = [(togi_machine::CcdId::new(0).unwrap(), 5000)]
        .into_iter()
        .collect();
    end.voltage_requests_v = [(core(0), 1.1), (core(1), 1.08)].into_iter().collect();
    j.append(end.into(), &[intent, mce]);
    let attribution = j.next_decision();
    j.decide(attribution);
    let (decision, why) = r7_backoff(&j);
    assert_eq!(why.counts, 10);
    assert_eq!(moved(&decision), (0, -20));
    j.assert_resumes();
}

#[test]
fn named_non_crash_without_a_target_and_carried_failure_still_move_one_count() {
    for carried in [false, true] {
        let mut j = Journal::r7_session();
        if carried {
            named_cap_passes(&mut j, 1.136);
            j.r7_fact(
                false,
                &[0, 1],
                &ALL_30,
                &report(&[(0, 1.1), (1, 1.08)], &[0]).named(0),
            );
        } else {
            named_cap_failure(&mut j, Signal::UnexpectedExit, Some(0), false);
        }
        let (decision, why) = r7_backoff(&j);
        assert_eq!(why.counts, 1);
        assert_eq!(moved(&decision), (0, -29));
        assert_eq!(why.notes, Vec::<Note>::new());
        j.assert_resumes();
    }
}

#[test]
fn named_non_crash_cap_respects_offset_bounds() {
    for (applied, want) in [(-50, -47), (-3, 0), (-2, 0), (-1, 0)] {
        let ids = [0, 1, 2, 3];
        let mut starts = [crate::testkit::Start::has_room(-30); 4];
        starts[0] = crate::testkit::Start::has_room(applied);
        let mut j = Journal::on(&ids, crate::testkit::pairs(&ids), &starts);
        j.profile_change(&[applied, -30, -30, -30]);
        named_cap_passes(&mut j, 1.136);
        named_cap_failure(&mut j, Signal::UnexpectedExit, Some(0), false);
        let (decision, why) = r7_backoff(&j);
        assert_eq!(why.counts, 3);
        assert_eq!(moved(&decision), (0, want));
        j.decide(decision);
        j.assert_resumes();
    }
}

#[test]
fn named_zero_routing_does_not_cap_the_necessary_stepdown() {
    let ids = [0, 1, 2, 3];
    let starts = [0, 0, -30, -30].map(crate::testkit::Start::has_room);
    let mut j = Journal::on(&ids, crate::testkit::quads(&ids), &starts);
    j.profile_change(&[0, 0, -30, -30]);
    j.r7_failed(
        &ids,
        &[0, 0, -30, -30],
        &report(&[(0, 1.1), (1, 1.2), (2, 1.15), (3, 1.14)], &[1]).named(0),
    );
    let (decision, why) = r7_backoff(&j);
    assert_eq!(why.counts, 14);
    assert_eq!(moved(&decision), (2, -16));
    assert!(
        why.notes
            .iter()
            .any(|note| matches!(note, Note::SteppedDown { .. }))
    );
    assert!(
        !why.notes
            .iter()
            .any(|note| matches!(note, Note::NamedNonCrashCap { .. }))
    );
    j.assert_resumes();
}

#[test]
fn named_zero_top_requester_still_requires_an_all_zero_rerun() {
    let mut j = Journal::r7_session();
    j.core_phase(crate::schedule_tests::transition(
        0,
        togi_journal::CorePhase::HasRoom,
        0,
    ));
    j.profile_change(&[0, -30, -30, -30]);
    named_cap_passes(&mut j, 1.136);
    named_cap_failure(&mut j, Signal::ComputationError, Some(0), false);
    assert!(matches!(
        j.run().r7_decision(),
        Some(R7Response::ZeroRerun(_))
    ));
    j.assert_resumes();
}

#[test]
fn single_core_r7_and_other_regimes_keep_ordinary_one_count_backoff() {
    for regime in [Regime::R7, Regime::R2] {
        let mut j = Journal::has_room(&[-30]);
        j.load(&crate::testkit::config(
            r#"{"checking":{"cycle":["R7","R2"]},"durations":{"checking_trial_s":120}}"#,
        ));
        j.profile_change(&[-30]);
        j.ranking(&[0]);
        let n = needed(&j);
        passes(
            &mut j,
            n,
            &[0],
            &[-20],
            &report(&[(0, 1.136)], &[0]).clocks(&[(0, 5000)]),
        );
        let plan = j.manual(
            regime,
            0,
            crate::Load::Single(core(0)),
            Condition::Together,
            &[-30],
            120,
        );
        let (intent, trial) = j.intent(&plan);
        let mut end =
            Journal::end_payload(&trial, Outcome::Failure, Some(Signal::ComputationError));
        end.core = Some(core(0));
        end.voltage_requests_v = [(core(0), 1.1)].into_iter().collect();
        j.append(end.into(), &[intent]);
        let attribution = j.next_decision();
        j.decide(attribution);
        assert_eq!(j.run().r7_decision(), None);
        let decision = j.next_decision();
        assert_eq!(moved(&decision), (0, -29));
        j.decide(decision);
        j.assert_resumes();
    }
}

#[test]
fn a_recorded_failure_point_takes_precedence_over_the_three_count_cap() {
    use crate::r7::target::backoff_move;
    use crate::tests::off;

    for (prior, want) in [
        (None, -27),
        (Some(-29), -27),
        (Some(-24), -23),
        (Some(-1), 0),
    ] {
        let moved = backoff_move(off(-30), prior.map(off), 3);
        assert_eq!(moved.to, off(want));
        assert_eq!(moved.failure_point, off(prior.unwrap_or(-30)));
    }
}
