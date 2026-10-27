use std::time::Duration;
use test_case::test_case;
use togi_config::DeadEndThreshold;

use togi_journal::{Outcome, Seq};
use togi_machine::{
    BootId, CcdId, CoreId, CoreSet, CpuId, PerCore, PmTable, ProcessFaultKind, Regime, Signal,
    TrialConditions, TrialFailure, TrialResult,
};

use super::{
    Adjudication, FailureSignal, Ladder, Placement, ProcessFaults, RecordedMce,
    RecordedProcessFault, ResetRuling, ResetVerdict, TrialEvidence, Verdict, Window, adjudicate,
    backend_exhausted, backend_retry, crash_owns, join_diagnostic, kernel_retry, mce_in_window,
    mce_signal, place_kernel_mce, summarize_samples, trial_end,
};

const MINUTE: Duration = Duration::from_secs(60);

fn core(n: i64) -> CoreId {
    CoreId::new(n).unwrap()
}

#[derive(Clone)]
struct Case {
    result: TrialResult,
    mces: Vec<RecordedMce>,
    reset: Option<ResetVerdict>,
    missing: Option<String>,
    containment: Option<String>,
}

impl Case {
    fn new() -> Self {
        Self {
            result: TrialResult {
                ran: MINUTE,
                ..TrialResult::default()
            },
            mces: Vec::new(),
            reset: None,
            missing: None,
            containment: None,
        }
    }

    fn adjudicate(&self) -> Adjudication {
        adjudicate(&TrialEvidence {
            result: &self.result,
            mces: &self.mces,
            reset: self.reset.as_ref(),
            missing: self.missing.as_deref(),
            duration: MINUTE,
            containment: self.containment.as_deref(),
            process: None,
        })
        .unwrap()
    }
}

fn mce(corrected: bool) -> RecordedMce {
    RecordedMce {
        seq: Seq::new(7).unwrap(),
        corrected,
    }
}

struct Row {
    name: &'static str,
    apply: fn(&mut Case),
    outcome: Outcome,
    signal: Option<Signal>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            name: "escape",
            apply: |case| case.result.escaped = vec![CpuId::from(99)],
            outcome: Outcome::Inconclusive,
            signal: None,
        },
        Row {
            name: "containment",
            apply: |case| case.containment = Some("cleanup could not be confirmed".into()),
            outcome: Outcome::Inconclusive,
            signal: None,
        },
        Row {
            name: "backend",
            apply: |case| {
                case.result.failure = Some(TrialFailure {
                    signal: Signal::ComputationError,
                    core: core(3),
                    pid: None,
                });
            },
            outcome: Outcome::Failure,
            signal: Some(Signal::ComputationError),
        },
        Row {
            name: "mce",
            apply: |case| case.mces = vec![mce(true)],
            outcome: Outcome::Failure,
            signal: Some(Signal::CorrectedMce),
        },
        Row {
            name: "reset",
            apply: |case| {
                case.reset = Some(ResetVerdict {
                    outcome: Outcome::Failure,
                    signal: Some(Signal::Crash),
                    reason: Some("watchdog".into()),
                });
            },
            outcome: Outcome::Failure,
            signal: Some(Signal::Crash),
        },
        Row {
            name: "missing",
            apply: |case| case.missing = Some("sampling unavailable".into()),
            outcome: Outcome::Inconclusive,
            signal: None,
        },
        Row {
            name: "short",
            apply: |case| case.result.ran = MINUTE / 2,
            outcome: Outcome::Inconclusive,
            signal: None,
        },
        Row {
            name: "pass",
            apply: |_| {},
            outcome: Outcome::Pass,
            signal: None,
        },
    ]
}

#[test]
fn the_stronger_evidence_decides_in_every_pairing() {
    let rows = rows();
    for (i, upper) in rows.iter().enumerate() {
        for lower in &rows[i..] {
            let mut case = Case::new();
            (lower.apply)(&mut case);
            (upper.apply)(&mut case);
            let end = case.adjudicate();
            assert_eq!(
                (end.outcome, end.signal),
                (upper.outcome, upper.signal),
                "{} over {}",
                upper.name,
                lower.name
            );
        }
    }
}

#[test_case(Signal::ComputationError; "Signal::ComputationError")]
#[test_case(Signal::Stall; "Signal::Stall")]
#[test_case(Signal::UnexpectedExit; "Signal::UnexpectedExit")]
fn a_backend_failure_survives_every_weaker_diagnostic_and_keeps_its_core(signal: Signal) {
    let mut case = Case::new();
    case.result.failure = Some(TrialFailure {
        signal,
        core: core(5),
        pid: None,
    });
    case.missing = Some("canceled; cleanup failed".into());
    case.reset = Some(ResetVerdict {
        outcome: Outcome::Inconclusive,
        signal: None,
        reason: None,
    });
    case.mces = vec![mce(true)];
    let end = case.adjudicate();
    assert_eq!(end.signal, Some(signal));
    assert_eq!(end.outcome, Outcome::Failure);
    assert_eq!(end.core, Some(core(5)));
    assert_eq!(end.reason.as_deref(), Some("canceled; cleanup failed"));
}

#[test_case(MINUTE.saturating_sub(Duration::from_nanos(1)), Outcome::Inconclusive; "MINUTE.saturating_sub(Duration::from_nanos(1)), Outcome::Inconclusive")]
#[test_case(MINUTE, Outcome::Pass; "MINUTE, Outcome::Pass")]
#[test_case(MINUTE + Duration::from_nanos(1), Outcome::Pass; "MINUTE + Duration::from_nanos(1), Outcome::Pass")]
fn a_trial_passes_only_when_it_ran_its_full_duration(ran: Duration, outcome: Outcome) {
    let mut case = Case::new();
    case.result.ran = ran;
    let end = case.adjudicate();
    assert_eq!(end.outcome, outcome, "{ran:?}");
    assert_eq!(
        end.reason.as_deref(),
        (outcome == Outcome::Inconclusive).then_some("trial ended before its full duration")
    );
}

#[test]
fn an_uncorrected_machine_check_rejects_a_full_duration_pass() {
    let mut case = Case::new();
    case.mces = vec![mce(false)];
    let end = case.adjudicate();
    assert_eq!(
        (end.outcome, end.signal),
        (Outcome::Failure, Some(Signal::UncorrectedMce))
    );
    assert_eq!(end.duration_s, 60);
}

#[test]
fn any_corrected_machine_check_makes_the_signal_corrected() {
    assert_eq!(mce_signal(&[mce(false), mce(true)]), Signal::CorrectedMce);
    assert_eq!(mce_signal(&[mce(false)]), Signal::UncorrectedMce);
}

#[test]
fn an_escape_names_the_cpus_and_keeps_the_diagnostic() {
    let mut case = Case::new();
    case.result.escaped = vec![CpuId::from(99)];
    case.missing = Some("sampling unavailable".into());
    let end = case.adjudicate();
    assert_eq!(end.escaped, [CpuId::from(99)]);
    assert_eq!(
        end.reason.as_deref(),
        Some("backend thread outside allowed cpus; sampling unavailable")
    );
}

#[test]
fn unconfirmed_containment_is_carried_into_the_ending() {
    let mut case = Case::new();
    case.containment = Some("backend still running after teardown".into());
    let end = case.adjudicate();
    assert_eq!(end.outcome, Outcome::Inconclusive);
    assert_eq!(
        end.containment_error.as_deref(),
        Some("backend still running after teardown")
    );
}

#[test]
fn a_reset_verdict_joins_the_missing_diagnostic() {
    let mut case = Case::new();
    case.reset = Some(ResetVerdict {
        outcome: Outcome::Inconclusive,
        signal: None,
        reason: Some("power button".into()),
    });
    case.missing = Some("kernel log unreadable".into());
    let end = case.adjudicate();
    assert_eq!(end.outcome, Outcome::Inconclusive);
    assert_eq!(
        end.reason.as_deref(),
        Some("power button; kernel log unreadable")
    );
}

#[test]
fn diagnostics_join_with_a_semicolon_and_empty_ones_vanish() {
    assert_eq!(join_diagnostic(None, None), None);
    assert_eq!(join_diagnostic(Some(""), Some("")), None);
    assert_eq!(join_diagnostic(Some("a"), None).as_deref(), Some("a"));
    assert_eq!(join_diagnostic(None, Some("b")).as_deref(), Some("b"));
    assert_eq!(
        join_diagnostic(Some("a"), Some("b")).as_deref(),
        Some("a; b")
    );
}

#[test_case("zero", Duration::ZERO, Placement::BetweenTrials; "zero")]
#[test_case("before", Duration::from_secs(10).saturating_sub(Duration::from_nanos(1)), Placement::BetweenTrials; "before")]
#[test_case("start", Duration::from_secs(10), Placement::Trial; "start")]
#[test_case("inside", Duration::from_secs(15), Placement::Trial; "inside")]
#[test_case("teardown", Duration::from_secs(20), Placement::Trial; "teardown")]
#[test_case("future", Duration::from_secs(20) + Duration::from_nanos(1), Placement::BetweenTrials; "future")]
fn a_kernel_machine_check_belongs_to_a_trial_inside_its_window_both_ends_inclusive(
    name: &str,
    mono: Duration,
    want: Placement,
) {
    let window = Some(Window {
        since: Duration::from_secs(10),
        until: Duration::from_secs(20),
    });

    assert_eq!(
        place_kernel_mce(mono, true, window, false, false),
        want,
        "{name}"
    );
}

#[test]
fn an_uncorrected_machine_check_found_while_recovering_a_crash_belongs_to_that_crash() {
    let at = Duration::from_secs(1);
    let window = Some(Window {
        since: Duration::from_secs(10),
        until: Duration::from_secs(20),
    });
    assert_eq!(
        place_kernel_mce(at, false, None, true, true),
        Placement::FromBoot
    );
    assert_eq!(
        place_kernel_mce(at, false, window, true, true),
        Placement::FromBoot
    );
    assert_eq!(
        place_kernel_mce(at, true, None, true, true),
        Placement::BetweenTrials
    );
    assert_eq!(
        place_kernel_mce(at, false, None, true, false),
        Placement::BetweenTrials
    );
    assert_eq!(
        place_kernel_mce(at, false, None, false, true),
        Placement::BetweenTrials
    );
}

#[test]
fn a_machine_check_without_a_time_counts_and_one_with_a_time_counts_from_the_window_start() {
    let since = Duration::from_secs(10);
    assert!(mce_in_window(None, since));
    assert!(!mce_in_window(
        Some(since.saturating_sub(Duration::from_nanos(1))),
        since
    ));
    assert!(mce_in_window(Some(since), since));
    assert!(mce_in_window(Some(Duration::from_secs(12)), since));
}

#[test]
fn a_crash_owns_its_boots_machine_checks_and_the_uncorrected_ones_its_successor_retained() {
    let old = BootId::from("old".to_owned());
    let new = BootId::from("new".to_owned());
    let other = BootId::from("other".to_owned());
    assert!(crash_owns(&old, true, &old, &new));
    assert!(crash_owns(&old, false, &old, &new));
    assert!(crash_owns(&new, false, &old, &new));
    assert!(!crash_owns(&new, true, &old, &new));
    assert!(!crash_owns(&other, false, &old, &new));
}

fn threshold(k: u32) -> DeadEndThreshold {
    DeadEndThreshold::new(i64::from(k)).unwrap()
}

#[test]
fn a_backend_waits_one_five_then_thirty_minutes_after_the_threshold() {
    let waits: Vec<_> = (0..=4)
        .map(|streak| backend_retry(streak, threshold(1)).map(|due| (due.attempt, due.wait)))
        .collect();
    assert_eq!(
        waits,
        [
            None,
            Some((1, MINUTE)),
            Some((2, 5 * MINUTE)),
            Some((3, 30 * MINUTE)),
            None
        ]
    );
    assert_eq!(backend_retry(3, threshold(4)), None);
    assert_eq!(
        backend_retry(4, threshold(4)).map(|due| due.attempt),
        Some(1)
    );
    assert_eq!(
        backend_retry(6, threshold(4)).map(|due| due.attempt),
        Some(3)
    );
}

#[test]
fn no_evidence_comes_three_inconclusive_trials_after_the_threshold() {
    assert!(!backend_exhausted(3, threshold(1)));
    assert!(backend_exhausted(4, threshold(1)));
    assert!(!backend_exhausted(6, threshold(4)));
    assert!(backend_exhausted(7, threshold(4)));
}

#[test]
fn the_kernel_log_is_retried_on_the_same_schedule() {
    let waits: Vec<_> = (0..4)
        .map(|recorded| kernel_retry(recorded).map(|due| due.wait))
        .collect();
    assert_eq!(
        waits,
        [Some(MINUTE), Some(5 * MINUTE), Some(30 * MINUTE), None]
    );
}

fn workers(readings: &[&[(i64, i64)]], loaded: &[i64], regime: Regime) -> TrialSummary {
    let samples: Vec<TrialConditions> = readings
        .iter()
        .enumerate()
        .map(|(i, reading)| TrialConditions {
            elapsed_ms: i64::try_from(i + 1).unwrap() * 1000,
            worker_cpu_ms: reading.iter().map(|&(n, cpu)| (core(n), cpu)).collect(),
            ..TrialConditions::default()
        })
        .collect();
    let last = samples.last().map(|sample| sample.elapsed_ms);
    let summary =
        summarize_samples(samples.into_iter().map(Ok), set(loaded), regime, |_| None).unwrap();
    TrialSummary { summary, last }
}

struct TrialSummary {
    summary: super::SampleSummary,
    last: Option<i64>,
}

fn set(ids: &[i64]) -> CoreSet {
    ids.iter().map(|&id| core(id)).collect()
}

#[test_case("first stall", &[
                &[(2, 1000), (7, 1000)],
                &[(2, 1000), (7, 2000)],
                &[(2, 1000), (7, 2000)],
            ], Regime::R7, Some((2, 2000)); "first stall")]
#[test_case("scheduled R6 suspension", &[&[(2, 0), (7, 0)], &[(2, 0), (7, 100)], &[(2, 0), (7, 200)]], Regime::R6, None; "scheduled R6 suspension")]
#[test_case("scheduled R8 suspension", &[&[(2, 0), (7, 0)], &[(2, 0), (7, 100)], &[(2, 0), (7, 200)]], Regime::R8, None; "scheduled R8 suspension")]
#[test_case("scheduled R9 suspension", &[&[(2, 0), (7, 0)], &[(2, 0), (7, 100)], &[(2, 0), (7, 200)]], Regime::R9, None; "scheduled R9 suspension")]
#[test_case("advancing until end", &[&[(2, 1000), (7, 1000)], &[(2, 2000), (7, 2000)]], Regime::R7, None; "advancing until end")]
#[test_case("tie", &[&[(2, 1000), (7, 1000)], &[(2, 1000), (7, 1000)]], Regime::R7, None; "tie")]
#[test_case("missing reading", &[
                &[(2, 1000), (7, 1000)],
                &[(2, 1000)],
                &[(2, 1000), (7, 2000)],
            ], Regime::R7, None; "missing reading")]
#[test_case("no samples", &[], Regime::R7, None; "no samples")]
#[test_case("one sample", &[&[(2, 1000), (7, 1000)]], Regime::R7, None; "one sample")]
#[test_case("transient plateau resumes", &[
                &[(2, 1000), (7, 1000)],
                &[(2, 1000), (7, 2000)],
                &[(2, 2000), (7, 3000)],
            ], Regime::R7, None; "transient plateau resumes")]
#[test_case("later worker stalls first", &[
                &[(2, 1000), (7, 1000)],
                &[(2, 2000), (7, 1000)],
                &[(2, 2000), (7, 1000)],
            ], Regime::R7, Some((7, 2000)); "later worker stalls first")]
#[test_case("counter decrease", &[
                &[(2, 1000), (7, 1000)],
                &[(2, 0), (7, 2000)],
                &[(2, 0), (7, 3000)],
            ], Regime::R7, None; "counter decrease")]
fn only_a_unique_earliest_persistent_stall_names_a_core(
    name: &str,
    readings: &[&[(i64, i64)]],
    regime: Regime,
    want: Option<(i64, i64)>,
) {
    let r7 = Regime::R7;

    let got = workers(readings, &[2, 7], regime);
    let stalled = got
        .summary
        .stalled
        .map(|stall| (i64::from(stall.core.get()), stall.since_ms));
    assert_eq!(stalled, want, "{name}");
    assert_eq!(
        got.summary.last.map(|last| last.elapsed_ms),
        got.last,
        "{name}"
    );

    let single = workers(&[&[(2, 1000)], &[(2, 1000)]], &[2], r7);
    assert_eq!(single.summary.stalled, None);
}

fn voltage_sample(a: f32, b: f32) -> TrialConditions {
    let mut table = PmTable::default();
    table.voltage_request_v[2] = a;
    table.voltage_request_v[7] = b;
    table.voltage_request_v[0] = 1.75;
    table.voltage_request_v[15] = 2.0;
    TrialConditions {
        pm_table: Some(table),
        ..TrialConditions::default()
    }
}

#[test_case("loaded subset odd", &[2, 7], vec![
                voltage_sample(1.0, 1.125),
                voltage_sample(1.5, 1.25),
                voltage_sample(1.25, 1.0),
            ], Some((1.25, 1.125)); "loaded subset odd")]
#[test_case("loaded subset even", &[2, 7], vec![
                voltage_sample(1.5, 1.25),
                voltage_sample(1.0, 1.125),
                voltage_sample(1.25, 1.0),
                voltage_sample(1.375, 1.125),
            ], Some((1.3125, 1.125)); "loaded subset even")]
#[test_case("missing lanes skipped", &[2, 7], vec![
                TrialConditions::default(),
                voltage_sample(1.0, 1.125),
                TrialConditions::default(),
                voltage_sample(1.5, 1.25),
                TrialConditions::default(),
            ], Some((1.3125, 1.125)); "missing lanes skipped")]
#[test_case("single core", &[7], vec![voltage_sample(1.5, 1.0), voltage_sample(1.375, 1.25)], Some((1.125, 1.0)); "single core")]
#[test_case("all missing", &[2, 7], vec![TrialConditions::default(), TrialConditions::default()], None; "all missing")]
#[test_case("no samples", &[2, 7], vec![], None; "no samples")]
#[test_case("no loaded cores", &[], vec![voltage_sample(1.0, 1.25)], None; "no loaded cores")]
#[test_case("non-finite lane skipped", &[2, 7], vec![voltage_sample(f32::NAN, 1.0), voltage_sample(1.5, 1.0)], Some((1.5, 1.5)); "non-finite lane skipped")]
fn the_requested_voltage_is_the_loaded_cores_highest_request_by_median_and_minimum(
    name: &str,
    loaded: &[i64],
    samples: Vec<TrialConditions>,
    want: Option<(f64, f64)>,
) {
    let got = summarize_samples(samples.into_iter().map(Ok), set(loaded), Regime::R7, |_| {
        None
    })
    .unwrap()
    .voltage;
    assert_eq!(
        got.map(|voltage| (voltage.median_v, voltage.min_v)),
        want,
        "{name}"
    );
}

fn telemetry_samples(count: usize) -> Vec<TrialConditions> {
    (0..count)
        .map(|i| {
            let mut sample = voltage_sample(1.25, 1.5);
            sample.elapsed_ms = 5000 + i64::try_from(i).unwrap() * 1000;
            sample.core_mhz = [(core(2), 5000), (core(7), 5100)]
                .into_iter()
                .collect::<PerCore<i64>>();
            sample
        })
        .collect()
}

#[test_case(Regime::R1; "Regime::R1")]
#[test_case(Regime::R3; "Regime::R3")]
#[test_case(Regime::R6; "Regime::R6")]
#[test_case(Regime::R7; "Regime::R7")]
fn request_telemetry_needs_twenty_samples_after_the_warmup_and_every_loaded_core_on_a_known_ccd(
    regime: Regime,
) {
    let ccd = |core: CoreId| CcdId::try_from(core.get() / 4).ok();

    let enough = summarize_samples(
        telemetry_samples(20).into_iter().map(Ok),
        set(&[2, 7]),
        regime,
        ccd,
    )
    .unwrap();
    let telemetry = enough.telemetry.unwrap();
    assert_eq!(telemetry.requests.get(core(7)), Some(1.5), "{regime}");
    assert_eq!(telemetry.ccd_mhz.len(), 2, "{regime}");
    let short = summarize_samples(
        telemetry_samples(19).into_iter().map(Ok),
        set(&[2, 7]),
        regime,
        ccd,
    )
    .unwrap();
    assert!(short.telemetry.is_none(), "{regime}");

    let unknown = summarize_samples(
        telemetry_samples(20).into_iter().map(Ok),
        set(&[2, 7]),
        Regime::R7,
        |core: CoreId| (core.get() != 7).then_some(CcdId::new(0).unwrap()),
    )
    .unwrap();
    assert!(unknown.telemetry.is_none());
    assert!(unknown.voltage.is_some());
}

#[test_case(Regime::R8; "neighbour load")]
#[test_case(Regime::R9; "all threads")]
fn mixed_regime_telemetry_starts_at_thirty_seconds_and_suspension_is_not_a_stall(regime: Regime) {
    let ccd = |core: CoreId| CcdId::try_from(core.get() / 4).ok();
    let before = summarize_samples(
        telemetry_samples(44).into_iter().map(Ok),
        set(&[2, 7]),
        regime,
        ccd,
    )
    .unwrap();
    assert!(before.telemetry.is_none());
    assert_eq!(before.stalled, None);
    let at_boundary = summarize_samples(
        telemetry_samples(45).into_iter().map(Ok),
        set(&[2, 7]),
        regime,
        ccd,
    )
    .unwrap();
    assert_eq!(
        at_boundary.telemetry.unwrap().requests.get(core(7)),
        Some(1.5)
    );
    assert_eq!(at_boundary.stalled, None);
}

#[test]
fn idle_core_clocks_do_not_change_loaded_core_summaries() {
    let samples = telemetry_samples(20);
    let loaded = set(&[2, 7]);
    let ccd = |core: CoreId| CcdId::try_from(core.get() / 4).ok();
    let before =
        summarize_samples(samples.iter().cloned().map(Ok), loaded, Regime::R7, ccd).unwrap();
    let extended = samples.into_iter().map(|mut sample| {
        sample.core_mhz.set(core(0), 100);
        sample.core_mhz.set(core(4), 6000);
        Ok(sample)
    });
    let after = summarize_samples(extended, loaded, Regime::R7, ccd).unwrap();
    assert_eq!(after, before);
}

#[test]
fn a_summary_fills_an_end_with_the_stall_of_a_failure_and_the_last_sample_of_a_crash() {
    let samples = [(1, 1000), (2, 1000), (3, 1000)].map(|(second, cpu)| TrialConditions {
        elapsed_ms: second * 1000,
        tctl_c: Some(70 + second),
        core_mhz: [
            (core(2), 4400),
            (core(7), 4200),
            (core(0), 100),
            (core(15), 6000),
        ]
        .into_iter()
        .collect(),
        worker_cpu_ms: [(core(2), second * 100), (core(7), cpu)]
            .into_iter()
            .collect(),
        ..voltage_sample(1.25, 1.5)
    });
    let summary = summarize_samples(
        samples.into_iter().map(Ok),
        set(&[2, 7]),
        Regime::R7,
        |_| None,
    )
    .unwrap();
    let end = |outcome, crashed| {
        let mut end = trial_end(togi_journal::TrialId::from("0001".to_owned()), outcome, 3);
        summary.fill(&mut end, crashed).unwrap();
        end
    };

    let failed = end(Outcome::Failure, true);
    assert_eq!(failed.voltage_request_median_v, Some(1.5));
    assert_eq!(
        (failed.stalled_core, failed.worker_stalled_ms),
        (Some(core(7)), Some(2000))
    );
    assert_eq!(
        (failed.last_sample_s, failed.last_sample_tctl_c),
        (Some(3), Some(73))
    );
    assert_eq!(
        (failed.last_sample_min_mhz, failed.last_sample_max_mhz),
        (Some(4200), Some(4400))
    );

    let passed = end(Outcome::Pass, false);
    assert_eq!(passed.voltage_request_median_v, Some(1.5));
    assert_eq!(
        (passed.stalled_core, passed.worker_stalled_ms),
        (None, None)
    );
    assert_eq!(passed.last_sample_s, None);
}

#[test_case(i64::from(i32::MIN), true; "minimum temperature")]
#[test_case(i64::from(i32::MAX), true; "maximum temperature")]
#[test_case(i64::from(i32::MIN) - 1, false; "negative temperature overflow")]
#[test_case(i64::from(i32::MAX) + 1, false; "positive temperature overflow")]
fn trial_maximum_temperature_is_checked(value: i64, fits: bool) {
    let mut case = Case::new();
    case.result.tctl_max_c = Some(value);
    let result = adjudicate(&TrialEvidence {
        result: &case.result,
        mces: &[],
        reset: None,
        missing: None,
        duration: MINUTE,
        containment: None,
    });
    if fits {
        assert_eq!(result.unwrap().tctl_max_c.map(i64::from), Some(value));
    } else {
        assert!(matches!(
            result,
            Err(crate::SessionError::TrialTemperature(_))
        ));
    }
}

#[test_case(Some(i64::from(i32::MIN)), Some(0), true; "minimum fields")]
#[test_case(Some(i64::from(i32::MAX)), Some(i64::from(u32::MAX)), true; "maximum fields")]
#[test_case(None, None, true; "absent fields")]
#[test_case(Some(i64::from(i32::MIN) - 1), Some(4000), false; "temperature below minimum")]
#[test_case(Some(i64::from(i32::MAX) + 1), Some(4000), false; "temperature above maximum")]
#[test_case(Some(70), Some(-1), false; "negative frequency")]
#[test_case(Some(70), Some(i64::from(u32::MAX) + 1), false; "frequency above maximum")]
fn last_sample_narrowing_is_exact_or_rejects_the_whole_summary(
    temperature: Option<i64>,
    clock: Option<i64>,
    fits: bool,
) {
    let sample = TrialConditions {
        elapsed_ms: 1000,
        tctl_c: temperature,
        core_mhz: clock.map(|mhz| (core(2), mhz)).into_iter().collect(),
        ..voltage_sample(1.0, 1.0)
    };
    let summary = summarize_samples([Ok(sample)], set(&[2]), Regime::R1, |_| None).unwrap();
    let mut end = trial_end("0001".to_owned().into(), Outcome::Failure, 1);
    let before = end.clone();
    let result = summary.fill(&mut end, true);
    if fits {
        result.unwrap();
        assert_eq!(end.last_sample_tctl_c.map(i64::from), temperature);
        assert_eq!(end.last_sample_min_mhz.map(i64::from), clock);
        assert_eq!(end.last_sample_max_mhz.map(i64::from), clock);
    } else {
        assert!(matches!(
            result,
            Err(togi_machine::SampleError::Temperature(_) | togi_machine::SampleError::Clock(_))
        ));
        assert_eq!(end, before);
    }
}

#[test_case(i64::from(u32::MAX), true; "largest ccd frequency")]
#[test_case(i64::from(u32::MAX) + 1, false; "overflowing ccd frequency")]
fn ccd_frequency_narrowing_is_checked_before_filling_any_fields(clock: i64, fits: bool) {
    let samples = telemetry_samples(20).into_iter().map(|mut sample| {
        sample.core_mhz.set(core(2), clock);
        sample
    });
    let summary = summarize_samples(samples.map(Ok), set(&[2]), Regime::R1, |_| {
        Some(CcdId::new(0).unwrap())
    })
    .unwrap();
    let mut end = trial_end("0001".to_owned().into(), Outcome::Pass, 30);
    let before = end.clone();
    let result = summary.fill(&mut end, false);
    if fits {
        result.unwrap();
        assert_eq!(
            end.ccd_mhz
                .values()
                .copied()
                .map(i64::from)
                .collect::<Vec<_>>(),
            [clock]
        );
    } else {
        assert!(matches!(result, Err(togi_machine::SampleError::Clock(_))));
        assert_eq!(end, before);
    }
}

fn tested() -> CpuId {
    CpuId::from(2)
}

fn fault(
    cpu: Option<CpuId>,
    core: Option<CoreId>,
    worker_core: Option<CoreId>,
) -> RecordedProcessFault {
    RecordedProcessFault {
        seq: Seq::new(9).unwrap(),
        pid: 1,
        kind: ProcessFaultKind::Segmentation,
        monotonic_ns: 0,
        process_pid: 100,
        cpu,
        core,
        worker_core,
    }
}

fn with_faults(case: &Case, faults: &[RecordedProcessFault]) -> Adjudication {
    adjudicate(&TrialEvidence {
        result: &case.result,
        mces: &case.mces,
        reset: case.reset.as_ref(),
        missing: case.missing.as_deref(),
        duration: MINUTE,
        containment: case.containment.as_deref(),
        process: Some(ProcessFaults {
            recorded: faults,
            tested: &[tested()],
        }),
    })
    .unwrap()
}

fn unexpected_exit() -> TrialFailure {
    TrialFailure {
        signal: Signal::UnexpectedExit,
        core: core(3),
        pid: None,
    }
}

#[test]
fn a_kernel_placed_fault_replaces_the_workers_core_of_an_unexpected_exit() {
    let mut case = Case::new();
    case.result.failure = Some(unexpected_exit());
    let end = with_faults(&case, &[fault(Some(tested()), Some(core(4)), None)]);
    assert_eq!(
        (end.outcome, end.signal, end.core),
        (Outcome::Failure, Some(Signal::UnexpectedExit), Some(core(4)))
    );
}

#[test]
fn an_exit_the_kernel_placed_outside_the_tested_cpus_is_inconclusive() {
    let mut case = Case::new();
    case.result.failure = Some(unexpected_exit());
    let end = with_faults(
        &case,
        &[fault(Some(CpuId::from(99)), Some(core(4)), Some(core(3)))],
    );
    assert_eq!(
        (end.outcome, end.signal, end.core),
        (Outcome::Inconclusive, None, None)
    );
    assert_eq!(
        end.reason.as_deref(),
        Some("worker fault executed outside tested CPUs")
    );
}

#[test]
fn a_fault_without_a_core_still_fails_the_trial_and_is_worded_by_the_evidence_below_it() {
    let mut case = Case::new();
    case.result.ran = MINUTE / 2;
    let end = with_faults(&case, &[fault(Some(tested()), None, None)]);
    assert_eq!(
        (end.outcome, end.signal, end.core),
        (Outcome::Failure, Some(Signal::UnexpectedExit), None)
    );
    assert_eq!(
        end.reason.as_deref(),
        Some("trial ended before its full duration")
    );
}

#[test]
fn a_fault_without_a_core_outranks_machine_checks_and_a_reset_but_not_an_escape() {
    let mut case = Case::new();
    case.mces = vec![mce(true)];
    case.reset = Some(ResetVerdict {
        outcome: Outcome::Failure,
        signal: Some(Signal::Crash),
        reason: None,
    });
    let faults = [fault(Some(tested()), None, None)];
    let end = with_faults(&case, &faults);
    assert_eq!(
        (end.outcome, end.signal, end.core),
        (Outcome::Failure, Some(Signal::UnexpectedExit), None)
    );
    case.result.escaped = vec![CpuId::from(99)];
    let end = with_faults(&case, &faults);
    assert_eq!((end.outcome, end.signal), (Outcome::Inconclusive, None));
}

const CLEAR: Ladder<'static> = Ladder {
    escaped: &[],
    uncontained: None,
    failure: None,
    machine_checks: None,
    reset: None,
    inconclusive: false,
    short: false,
};

const CRASH: ResetRuling = ResetRuling {
    outcome: Outcome::Failure,
    signal: Some(Signal::Crash),
};

#[test_case(CLEAR, Verdict::Passed; "nothing")]
#[test_case(Ladder { short: true, ..CLEAR }, Verdict::Short; "short")]
#[test_case(Ladder { inconclusive: true, short: true, ..CLEAR }, Verdict::Inconclusive; "inconclusive")]
#[test_case(Ladder { reset: Some(CRASH), inconclusive: true, ..CLEAR }, Verdict::Reset(CRASH); "reset")]
#[test_case(Ladder { machine_checks: Some(Signal::UncorrectedMce), reset: Some(CRASH), ..CLEAR }, Verdict::MachineChecks(Signal::UncorrectedMce); "machine checks")]
#[test_case(Ladder { failure: Some(UNPLACED), machine_checks: Some(Signal::UncorrectedMce), ..CLEAR }, Verdict::Failure(UNPLACED); "failure")]
#[test_case(Ladder { uncontained: Some("unconfirmed"), failure: Some(UNPLACED), ..CLEAR }, Verdict::Uncontained; "uncontained")]
#[test_case(Ladder { escaped: &[CpuId::from(99)], uncontained: Some("unconfirmed"), ..CLEAR }, Verdict::Escaped; "escaped")]
fn the_ladder_decides_by_the_first_rung_that_holds(ladder: Ladder<'_>, verdict: Verdict) {
    assert_eq!(ladder.decide(), verdict);
}

const UNPLACED: FailureSignal = FailureSignal {
    signal: Signal::Stall,
    core: None,
    pid: None,
};

#[test_case(Verdict::Failure(UNPLACED), Outcome::Failure, Some(Signal::Stall); "failure")]
#[test_case(Verdict::MachineChecks(Signal::CorrectedMce), Outcome::Failure, Some(Signal::CorrectedMce); "machine checks")]
#[test_case(Verdict::Reset(CRASH), Outcome::Failure, Some(Signal::Crash); "reset")]
#[test_case(Verdict::Reset(ResetRuling { outcome: Outcome::Inconclusive, signal: None }), Outcome::Inconclusive, None; "inconclusive reset")]
#[test_case(Verdict::Short, Outcome::Inconclusive, None; "short")]
#[test_case(Verdict::Passed, Outcome::Pass, None; "passed")]
fn a_verdict_names_its_outcome_and_signal(
    verdict: Verdict,
    outcome: Outcome,
    signal: Option<Signal>,
) {
    assert_eq!((verdict.outcome(), verdict.signal()), (outcome, signal));
}
