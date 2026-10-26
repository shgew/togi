use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed};
use test_case::test_case;
use togi_journal::{Attribution, Outcome, TrialProgress, TrialSample, TrialSignal};
use togi_machine::{CpuId, Regime, Signal};

use super::support::{
    Log, build, config_loaded, crash_detected, end, failure, intent, trial_start, workload,
};
use crate::project::Projection;
use crate::report::{RecoveryGap, compute, summarize_gaps};

/// A trial cut short by a crash. Its last evidence comes `ran` after it starts, after a wall-clock `jump`;
/// the next boot logs its first event `recovery` after that evidence, 10 seconds after booting.
struct Interruption {
    evidence: Evidence,
    signal: Signal,
    ran: i64,
    jump: i64,
    recovery: i64,
}

#[derive(Clone, Copy, Debug)]
enum Evidence {
    Progress,
    Signal,
    Sample,
}

impl Interruption {
    fn new(evidence: Evidence, signal: Signal) -> Self {
        Self {
            evidence,
            signal,
            ran: 20,
            jump: 0,
            recovery: 80,
        }
    }

    fn log(&self) -> Log {
        let mut log = Log::new(&[(0, 0)]);
        let at = 100;
        log.boot("t/a").at(at).mono(10_000);
        let planned = intent("t", Regime::R7, &[0, 1], &[0, 0]);
        let intent_seq = log.add(planned);
        log.add(trial_start("t"));
        let evidence_at = at + self.ran + self.jump;
        log.at(evidence_at);
        let id = || "t".to_owned().into();
        match self.evidence {
            Evidence::Progress => log.add(TrialProgress {
                trial: id(),
                detail: String::new(),
                signal: Some(self.signal),
                core: None,
                fault: None,
                pid: None,
            }),
            Evidence::Signal => log.add(TrialSignal {
                trial: id(),
                schedule: None,
                stops: 0,
                conts: 0,
            }),
            Evidence::Sample => log.add(TrialSample {
                trial: id(),
                warning: String::new(),
                pid: 1,
                tid: 1,
                cpu: CpuId::from(0),
            }),
        };
        log.boot("t/b").at(evidence_at + self.recovery);
        log.add(config_loaded(build("test", None)));
        log.add(crash_detected("t/a", Some(intent_seq)));
        let mut ended = end("t", Outcome::Failure);
        ended.signal = Some(self.signal);
        ended.duration_s = u32::try_from(self.ran).unwrap();
        log.add(ended);
        let mut failed = failure(Some("t"));
        failed.signal = self.signal;
        failed.regime = Some(Regime::R7);
        log.add(failed);
        log
    }
}

#[test_case(-3600, Evidence::Progress; "backward progress")]
#[test_case(-3600, Evidence::Signal; "backward signal")]
#[test_case(-3600, Evidence::Sample; "backward sample")]
#[test_case(3600, Evidence::Progress; "forward progress")]
#[test_case(3600, Evidence::Signal; "forward signal")]
#[test_case(3600, Evidence::Sample; "forward sample")]
fn the_recovery_gap_runs_from_the_last_evidence_whatever_the_wall_clock_did(
    jump: i64,
    evidence: Evidence,
) {
    let mut cut = Interruption::new(evidence, Signal::Crash);
    cut.jump = jump;
    let journal = cut.log().journal();
    let p = Projection::new(&journal);
    let m = compute(&p, None);
    assert_eq!(
        m.recovery,
        RecoveryGap {
            count: 1,
            shortest: 80.0,
            median: 80.0,
            longest: 80.0
        },
        "{evidence:?} after a jump of {jump} s"
    );
    assert!(
        (m.time.downtime_s - 70.0).abs() < 1e-9,
        "{evidence:?} after a jump of {jump} s: downtime {}",
        m.time.downtime_s
    );
}

#[test_case(Signal::ComputationError; "computation error")]
#[test_case(Signal::Stall; "stall")]
#[test_case(Signal::CorrectedMce; "corrected mce")]
#[test_case(Signal::UncorrectedMce; "uncorrected mce")]
fn a_crash_keeps_its_stronger_signal_in_every_metric(signal: Signal) {
    let journal = Interruption::new(Evidence::Progress, signal)
        .log()
        .journal();
    let p = Projection::new(&journal);
    let m = compute(&p, None);
    assert_eq!(
        m.recovery,
        RecoveryGap {
            count: 1,
            shortest: 80.0,
            median: 80.0,
            longest: 80.0
        },
        "{signal}"
    );
    assert!(
        (m.time.downtime_s - 70.0).abs() < 1e-9,
        "{signal}: downtime {}",
        m.time.downtime_s
    );
    assert_eq!(m.failures.timing, [0, 1, 0, 0, 0], "{signal}");
    assert_eq!(
        m.failures.counts.into_iter().collect::<Vec<_>>(),
        [(
            (
                Some(Regime::R7),
                Some(workload(Regime::R7, 0)),
                signal,
                Attribution::Unattributed
            ),
            1
        )],
        "{signal}"
    );
}

#[test_case(0, 0; "zero seconds")]
#[test_case(1, 1; "one second")]
#[test_case(29, 1; "twenty nine seconds")]
#[test_case(30, 2; "thirty seconds")]
#[test_case(59, 2; "fifty nine seconds")]
#[test_case(60, 3; "sixty seconds")]
#[test_case(119, 3; "one hundred nineteen seconds")]
#[test_case(120, 4; "one hundred twenty seconds")]
fn crashes_are_binned_by_the_trial_seconds_before_their_last_evidence(ran: i64, bin: usize) {
    let mut cut = Interruption::new(Evidence::Progress, Signal::Crash);
    cut.ran = ran;
    let journal = cut.log().journal();
    let mut want = [0; 5];
    want[bin] = 1;
    assert_eq!(
        compute(&Projection::new(&journal), None).failures.timing,
        want,
        "{ran} s"
    );
}

#[test]
fn a_crash_in_the_last_boot_has_no_recovery_gap() {
    let mut log = Log::new(&[(0, 0)]);
    log.add(intent("t", Regime::R7, &[0, 1], &[0, 0]));
    log.add(trial_start("t"));
    let mut ended = end("t", Outcome::Failure);
    ended.signal = Some(Signal::Crash);
    log.add(ended);
    let journal = log.journal();
    let p = Projection::new(&journal);
    let m = compute(&p, None);
    assert_eq!(m.recovery, RecoveryGap::default());
    assert!(m.time.downtime_s.abs() < f64::EPSILON);
    assert_eq!(m.failures.timing, [0, 0, 0, 1, 0]);
}

#[test]
fn an_even_number_of_gaps_has_the_mean_of_the_middle_two_as_its_median() {
    assert_eq!(
        summarize_gaps(vec![90.0, 10.0, 50.0, 30.0]),
        RecoveryGap {
            count: 4,
            shortest: 10.0,
            median: 40.0,
            longest: 90.0
        }
    );
}

fn fixed() -> Config {
    Config {
        cases: 128,
        rng_seed: RngSeed::Fixed(0x7374_6174_7367_6170),
        failure_persistence: None,
        ..Config::default()
    }
}

proptest! {
    #![proptest_config(fixed())]

    #[test]
    fn a_gap_summary_is_ordered_and_counts_every_gap(
        gaps in proptest::collection::vec(0.0f64..10_000.0, 1..40)
    ) {
        let summary = summarize_gaps(gaps.clone());
        prop_assert_eq!(summary.count, gaps.len());
        prop_assert!(summary.shortest <= summary.median && summary.median <= summary.longest);
        prop_assert!(gaps.iter().all(|&gap| summary.shortest <= gap && gap <= summary.longest));
    }
}

#[test_case(None, 0; "absent uptime")]
#[test_case(Some(std::time::Duration::ZERO), 0; "zero uptime")]
#[test_case(Some(std::time::Duration::from_secs(10)), 10; "exact uptime")]
fn boot_projection_keeps_absence_distinct_from_exact_uptime(
    mono: Option<std::time::Duration>,
    seconds: i64,
) {
    let mut log = Log::new(&[(0, 0)]);
    log.events[0].mono = mono;
    let event_time = log.events[0].time;
    let boot = log.events[0].boot.clone();
    let journal = crate::Journal::new(log.events).unwrap();
    assert_eq!(
        journal.boot_start(&boot),
        event_time - jiff::SignedDuration::from_secs(seconds)
    );
}

#[test]
fn an_unrepresentable_recorded_boot_uptime_is_refused() {
    let mut log = Log::new(&[(0, 0)]);
    log.events[0].mono = Some(std::time::Duration::MAX);
    assert!(matches!(
        crate::Journal::new(log.events),
        Err(crate::Error::BootDuration(_))
    ));
}

#[test]
fn a_boot_start_before_the_timestamp_range_is_refused() {
    let mut log = Log::new(&[(0, 0)]);
    log.events[0].time = jiff::Timestamp::MIN;
    log.events[0].mono = Some(std::time::Duration::from_nanos(1));
    assert!(matches!(
        crate::Journal::new(log.events),
        Err(crate::Error::BootStart(_))
    ));
}
