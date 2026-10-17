use std::time::Duration;
use togi_config::DeadEndThreshold;

use togi_journal::{Outcome, Seq, TrialEnd};
use togi_machine::{
    BootId, CcdId, CoreId, CoreSet, CpuId, PerCore, PmTable, Regime, Signal, TrialConditions,
    TrialFailure, TrialResult,
};

use super::{
    Adjudication, Placement, RecordedMce, ResetVerdict, TrialEvidence, Window, adjudicate,
    backend_exhausted, backend_retry, crash_owns, join_diagnostic, kernel_retry, mce_in_window,
    mce_signal, place_kernel_mce, summarize_samples,
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
        })
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
            apply: |case| case.result.escaped = vec![CpuId(99)],
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

#[test]
fn a_backend_failure_survives_every_weaker_diagnostic_and_keeps_its_core() {
    for signal in [
        Signal::ComputationError,
        Signal::Stall,
        Signal::UnexpectedExit,
    ] {
        let mut case = Case::new();
        case.result.failure = Some(TrialFailure {
            signal,
            core: core(5),
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
}

#[test]
fn a_trial_passes_only_when_it_ran_its_full_duration() {
    for (ran, outcome) in [
        (
            MINUTE.saturating_sub(Duration::from_nanos(1)),
            Outcome::Inconclusive,
        ),
        (MINUTE, Outcome::Pass),
        (MINUTE + Duration::from_nanos(1), Outcome::Pass),
    ] {
        let mut case = Case::new();
        case.result.ran = ran;
        let end = case.adjudicate();
        assert_eq!(end.outcome, outcome, "{ran:?}");
        assert_eq!(
            end.reason.as_deref(),
            (outcome == Outcome::Inconclusive).then_some("trial ended before its full duration")
        );
    }
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
    case.result.escaped = vec![CpuId(99)];
    case.missing = Some("sampling unavailable".into());
    let end = case.adjudicate();
    assert_eq!(end.escaped, [CpuId(99)]);
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

#[test]
fn a_kernel_machine_check_belongs_to_a_trial_inside_its_window_both_ends_inclusive() {
    let window = Some(Window {
        since: Duration::from_secs(10),
        until: Duration::from_secs(20),
    });
    let ns = Duration::from_nanos(1);
    for (name, mono, want) in [
        ("zero", Duration::ZERO, Placement::BetweenTrials),
        (
            "before",
            Duration::from_secs(10).saturating_sub(ns),
            Placement::BetweenTrials,
        ),
        ("start", Duration::from_secs(10), Placement::Trial),
        ("inside", Duration::from_secs(15), Placement::Trial),
        ("teardown", Duration::from_secs(20), Placement::Trial),
        (
            "future",
            Duration::from_secs(20) + ns,
            Placement::BetweenTrials,
        ),
    ] {
        assert_eq!(
            place_kernel_mce(mono, true, window, false, false),
            want,
            "{name}"
        );
    }
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

#[test]
#[expect(clippy::type_complexity, reason = "a table of cases")]
fn only_a_unique_earliest_persistent_stall_names_a_core() {
    let r7 = Regime::R7;
    let cases: Vec<(&str, Vec<&[(i64, i64)]>, Regime, Option<(i64, i64)>)> = vec![
        (
            "first stall",
            vec![
                &[(2, 1000), (7, 1000)],
                &[(2, 1000), (7, 2000)],
                &[(2, 1000), (7, 2000)],
            ],
            r7,
            Some((2, 2000)),
        ),
        (
            "scheduled R6 suspension",
            vec![&[(2, 0), (7, 0)], &[(2, 0), (7, 100)], &[(2, 0), (7, 200)]],
            Regime::R6,
            None,
        ),
        (
            "advancing until end",
            vec![&[(2, 1000), (7, 1000)], &[(2, 2000), (7, 2000)]],
            r7,
            None,
        ),
        (
            "tie",
            vec![&[(2, 1000), (7, 1000)], &[(2, 1000), (7, 1000)]],
            r7,
            None,
        ),
        (
            "missing reading",
            vec![
                &[(2, 1000), (7, 1000)],
                &[(2, 1000)],
                &[(2, 1000), (7, 2000)],
            ],
            r7,
            None,
        ),
        ("no samples", vec![], r7, None),
        ("one sample", vec![&[(2, 1000), (7, 1000)]], r7, None),
        (
            "transient plateau resumes",
            vec![
                &[(2, 1000), (7, 1000)],
                &[(2, 1000), (7, 2000)],
                &[(2, 2000), (7, 3000)],
            ],
            r7,
            None,
        ),
        (
            "later worker stalls first",
            vec![
                &[(2, 1000), (7, 1000)],
                &[(2, 2000), (7, 1000)],
                &[(2, 2000), (7, 1000)],
            ],
            r7,
            Some((7, 2000)),
        ),
        (
            "counter decrease",
            vec![
                &[(2, 1000), (7, 1000)],
                &[(2, 0), (7, 2000)],
                &[(2, 0), (7, 3000)],
            ],
            r7,
            None,
        ),
    ];
    for (name, readings, regime, want) in cases {
        let got = workers(&readings, &[2, 7], regime);
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
    }
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

#[test]
#[expect(clippy::type_complexity, reason = "a table of cases")]
fn the_requested_voltage_is_the_loaded_cores_highest_request_by_median_and_minimum() {
    let none = TrialConditions::default;
    let cases: Vec<(&str, &[i64], Vec<TrialConditions>, Option<(f64, f64)>)> = vec![
        (
            "loaded subset odd",
            &[2, 7],
            vec![
                voltage_sample(1.0, 1.125),
                voltage_sample(1.5, 1.25),
                voltage_sample(1.25, 1.0),
            ],
            Some((1.25, 1.125)),
        ),
        (
            "loaded subset even",
            &[2, 7],
            vec![
                voltage_sample(1.5, 1.25),
                voltage_sample(1.0, 1.125),
                voltage_sample(1.25, 1.0),
                voltage_sample(1.375, 1.125),
            ],
            Some((1.3125, 1.125)),
        ),
        (
            "missing lanes skipped",
            &[2, 7],
            vec![
                none(),
                voltage_sample(1.0, 1.125),
                none(),
                voltage_sample(1.5, 1.25),
                none(),
            ],
            Some((1.3125, 1.125)),
        ),
        (
            "single core",
            &[7],
            vec![voltage_sample(1.5, 1.0), voltage_sample(1.375, 1.25)],
            Some((1.125, 1.0)),
        ),
        ("all missing", &[2, 7], vec![none(), none()], None),
        ("no samples", &[2, 7], vec![], None),
        (
            "no loaded cores",
            &[],
            vec![voltage_sample(1.0, 1.25)],
            None,
        ),
        (
            "non-finite lane skipped",
            &[2, 7],
            vec![voltage_sample(f32::NAN, 1.0), voltage_sample(1.5, 1.0)],
            Some((1.5, 1.5)),
        ),
    ];
    for (name, loaded, samples, want) in cases {
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

#[test]
fn request_telemetry_needs_twenty_samples_after_the_warmup_and_every_loaded_core_on_a_known_ccd() {
    let ccd = |core: CoreId| Some(CcdId(core.get() / 4));
    for regime in [Regime::R1, Regime::R3, Regime::R6, Regime::R7] {
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
    }
    let unknown = summarize_samples(
        telemetry_samples(20).into_iter().map(Ok),
        set(&[2, 7]),
        Regime::R7,
        |core: CoreId| (core.get() != 7).then_some(CcdId(0)),
    )
    .unwrap();
    assert!(unknown.telemetry.is_none());
    assert!(unknown.voltage.is_some());
}

#[test]
fn a_summary_fills_an_end_with_the_stall_of_a_failure_and_the_last_sample_of_a_crash() {
    let samples = [(1, 1000), (2, 1000), (3, 1000)].map(|(second, cpu)| TrialConditions {
        elapsed_ms: second * 1000,
        tctl_c: Some(70 + second),
        core_mhz: [(core(2), 4400), (core(7), 4200)].into_iter().collect(),
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
        let mut end = TrialEnd {
            trial: togi_journal::TrialId::from("0001".to_owned()),
            kernel_cursor: None,
            kernel_error: None,
            outcome,
            signal: None,
            core: None,
            duration_s: 3,
            tctl_max_c: None,
            voltage_request_median_v: None,
            voltage_request_min_v: None,
            voltage_requests_v: PerCore::default(),
            top_requesters: CoreSet::default(),
            ccd_mhz: std::collections::BTreeMap::new(),
            last_sample_s: None,
            last_sample_tctl_c: None,
            last_sample_min_mhz: None,
            last_sample_max_mhz: None,
            stalled_core: None,
            worker_stalled_ms: None,
            reason: None,
            interrupted: false,
            last_evidence: false,
            escaped: Vec::new(),
            backend_missing: false,
            containment_error: None,
        };
        summary.fill(&mut end, crashed);
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
