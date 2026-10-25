use jiff::SignedDuration;
use togi_journal::{
    Activity, Attribution, CheckingChain, Combination, CycleEvent, Decision, HuntOutcome,
    HuntStage, Member, Outcome, ShutdownReason, TrialProgress, TunerWarning, WarningKind,
};
use togi_machine::{Condition, PerCore, Regime, ResetKind, Signal};

use super::support::{
    EPOCH, Log, build, config_loaded, crash_detected, cycle, decision, end, failure, hunt_end,
    hunt_group, hunt_start, intent, nz, profile, set, shutdown, target, trial, trial_start,
    workload,
};
use crate::project::Projection;
use crate::report::{Exposure, compute, report};
use crate::{Error, Journal};

fn review_boundary() -> (Log, jiff::Timestamp) {
    let mut log = Log::with_build(&[(0, 0), (1, 1)], build("1.2.3", None));
    log.spaced(config_loaded(build("1.2.3", None)));
    log.spaced(cycle(1, CycleEvent::Start, false));
    let step = |log: &mut Log, id: &str, outcome: Outcome, hunt: u32, group: u32| {
        let mut planned = intent(id, Regime::R6, &[0, 1], &[-10, -10]);
        planned.duration_s = 60;
        if id == "trigger" {
            planned.duration_s = 120;
            planned.profile = profile(&[-11, -11]);
        }
        if hunt == 0 {
            planned.activity = Some(Activity::Checking);
            planned.cycle = Some(nz(1));
        } else {
            planned.activity = Some(Activity::Hunt);
            planned.condition = Condition::Parked;
            planned.hunt = Some(nz(hunt));
            planned.group = Some(nz(group));
        }
        let duration = planned.duration_s;
        log.spaced(planned);
        log.spaced(trial_start(id));
        if duration == 120 {
            log.spaced(TrialProgress {
                trial: id.to_owned().into(),
                detail: String::new(),
                signal: None,
                core: None,
                fault: None,
                pid: None,
            });
        }
        let mut ended = end(id, outcome);
        ended.duration_s = duration;
        if outcome == Outcome::Inconclusive {
            ended.reason = Some("no reliable result".to_owned());
        }
        log.spaced(ended);
    };
    step(&mut log, "prior", Outcome::Pass, 0, 0);
    let mut cycle_end = cycle(1, CycleEvent::End, true);
    cycle_end.passed = true;
    let parked_seq = log.spaced(cycle_end);
    step(&mut log, "trigger", Outcome::Failure, 0, 0);
    let mut trigger = failure(Some("trigger"));
    trigger.regime = Some(Regime::R6);
    trigger.profile = Some(profile(&[-11, -11]));
    let failure_seq = log.spaced(trigger);
    for hunt in [1, 2] {
        let mut start = hunt_start(hunt, "trigger", 1);
        start.failure = failure_seq;
        start.parked_seq = Some(parked_seq);
        start.parked = profile(&[-10, -10]);
        start.failing = profile(&[-11, -11]);
        log.spaced(start);
        if hunt == 1 {
            let mut skipped = hunt_group(1, 1, HuntStage::Part);
            skipped.skipped = true;
            log.spaced(skipped);
            log.spaced(hunt_end(1, HuntOutcome::Cancelled));
        }
    }
    let cutoff = log.events.last().unwrap().time;
    log.spaced(TunerWarning {
        warning: WarningKind::Monotonicity,
        trial: None,
        passes: Vec::new(),
        detail: String::new(),
    });
    for (i, outcome) in [Outcome::Pass, Outcome::Failure, Outcome::Inconclusive]
        .into_iter()
        .enumerate()
    {
        let group = u32::try_from(i).unwrap() + 1;
        log.spaced(hunt_group(2, group, HuntStage::Part));
        step(&mut log, &i.to_string(), outcome, 2, group);
        if outcome == Outcome::Failure {
            log.spaced(failure(Some(&i.to_string())));
        }
    }
    let mut combination = hunt_end(2, HuntOutcome::Combination);
    combination.members = vec![
        Member {
            core: super::support::core(1),
            offset: target(-10),
        },
        Member {
            core: super::support::core(0),
            offset: target(-10),
        },
    ];
    log.spaced(combination);
    (log, cutoff)
}

#[test]
fn the_review_boundary_scenario_reports_trials_and_failures_across_a_cutoff() {
    let (log, cutoff) = review_boundary();
    let journal = log.journal();
    let p = Projection::new(&journal);
    let all = compute(&p, None);
    let recent = compute(&p, Some(cutoff));
    assert_eq!(all.inconclusive.len(), 1);
    assert_eq!(all.inconclusive[0].intent.trial.as_str(), "2");
    assert_eq!(all.inconclusive[0].intent.condition, Condition::Parked);
    assert_eq!(
        recent.exposure.into_iter().collect::<Vec<_>>(),
        [(
            (Regime::R6, workload(Regime::R6, 0)),
            Exposure {
                trials: 3,
                seconds: 180,
                failures: 1
            }
        )]
    );
    assert_eq!(
        recent.failures.counts.into_iter().collect::<Vec<_>>(),
        [(
            (
                Some(Regime::R6),
                Some(workload(Regime::R6, 0)),
                Signal::ComputationError,
                Attribution::Unattributed
            ),
            1
        )]
    );
}

#[test]
fn the_review_boundary_report_before_and_after_a_cutoff() {
    let (log, cutoff) = review_boundary();
    let journal = log.journal();
    assert_snapshot!(
        "scenario__review_boundary_whole_journal",
        report(&journal, None)
    );
    assert_snapshot!(
        "scenario__review_boundary_since_cutoff",
        report(&journal, Some(cutoff))
    );
}

/// A combination failure on a two-CCD machine: an R7 trial crashes, the next boot hunts it down to two cores,
/// and the backoff commits to the combination.
fn combination() -> Log {
    let mut log = Log::with_build(
        &[(0, 0), (1, 0), (2, 1), (3, 1)],
        build("1.2.3", Some("abc")),
    );
    log.mono(5_000);
    log.at(0).add(config_loaded(build("1.2.3", Some("abc"))));
    log.at(30).add(cycle(1, CycleEvent::Start, false));
    let all_core = |id: &str, offsets: &[i64]| {
        let mut planned = intent(id, Regime::R7, &[0, 1, 2, 3], offsets);
        planned.activity = Some(Activity::Checking);
        planned.cycle = Some(nz(1));
        planned
    };
    let mut passing = end("0001", Outcome::Pass);
    passing.duration_s = 120;
    passing.tctl_max_c = Some(62);
    passing.voltage_requests_v = [(0, 1.05), (1, 1.10), (2, 1.04), (3, 1.09)]
        .into_iter()
        .map(|(n, v)| (super::support::core(n), v))
        .collect::<PerCore<f64>>();
    passing.top_requesters = set(&[1, 3]);
    log.at(60).add(all_core("0001", &[-20, -20, -20, -20]));
    log.at(61).add(trial_start("0001"));
    let passed = log.at(181).add(passing);
    let crashed = log.at(200).add(all_core("0002", &[-22, -22, -22, -22]));
    log.at(201).add(trial_start("0002"));
    log.at(231).add(TrialProgress {
        trial: "0002".to_owned().into(),
        detail: String::new(),
        signal: None,
        core: None,
        fault: None,
        pid: None,
    });
    log.boot("b").at(330).mono(12_000);
    log.add(config_loaded(build("1.2.3", Some("abc"))));
    let mut detected = crash_detected("a", Some(crashed));
    detected.reset_reason = Some(ResetKind::Watchdog);
    log.add(detected);
    let mut ended = end("0002", Outcome::Failure);
    ended.signal = Some(Signal::Crash);
    ended.duration_s = 30;
    ended.interrupted = true;
    log.add(ended);
    let mut crash = failure(Some("0002"));
    crash.signal = Signal::Crash;
    crash.regime = Some(Regime::R7);
    crash.profile = Some(profile(&[-22, -22, -22, -22]));
    let failed = log.add(crash);
    let mut start = hunt_start(1, "0002", 1);
    start.regime = Regime::R7;
    start.workload = workload(Regime::R7, 0);
    start.cores = set(&[0, 1, 2, 3]);
    start.failure = failed;
    start.parked = profile(&[0, 0, 0, 0]);
    start.failing = profile(&[-22, -22, -22, -22]);
    start.candidates = set(&[1, 3]);
    start.trials = 1;
    log.at(340).add_caused(&[failed], start);
    let hunted = |id: &str, group: u32, cores: &[i64]| {
        let mut planned = intent(id, Regime::R7, cores, &[0, -22, 0, -22]);
        planned.activity = Some(Activity::Hunt);
        planned.condition = Condition::Parked;
        planned.hunt = Some(nz(1));
        planned.group = Some(nz(group));
        planned.duration_s = 60;
        planned
    };
    let mut part = hunt_group(1, 1, HuntStage::Part);
    part.cores = set(&[1, 3]);
    log.at(341).add(part);
    trial(&mut log, hunted("0003", 1, &[1, 3]), Outcome::Failure);
    let mut inferred = hunt_group(1, 2, HuntStage::Complement);
    inferred.inferred = Some(togi_journal::Inference::Failure);
    log.add_caused(&[failed], inferred);
    let mut finish = hunt_end(1, HuntOutcome::Combination);
    finish.cores = set(&[1, 3]);
    finish.members = vec![
        Member {
            core: super::support::core(1),
            offset: target(-22),
        },
        Member {
            core: super::support::core(3),
            offset: target(-22),
        },
    ];
    let ended_hunt = log.at(500).add(finish);
    let combined = log.add_caused(
        &[ended_hunt],
        Combination {
            combination: nz(1),
            members: vec![
                Member {
                    core: super::support::core(1),
                    offset: target(-22),
                },
                Member {
                    core: super::support::core(3),
                    offset: target(-22),
                },
            ],
            fallback: false,
            hunt: nz(1),
            reason: String::new(),
        },
    );
    let mut backoff = decision(Decision::Backoff, 3, -22, -19);
    backoff.activity = Activity::Hunt;
    backoff.reason = "request 1.090 V\nto 1.100 V".to_owned();
    log.add_caused(&[combined, failed], backoff);
    log.add(CheckingChain {
        cycle: nz(1),
        step: nz(2),
        ccd: togi_machine::CcdId::new(1).unwrap(),
        workload: workload(Regime::R7, 0),
        groups: vec![set(&[3]), set(&[2])],
        cores: set(&[2]),
        source_seqs: vec![passed],
        profile: profile(&[-20, -20, -20, -19]),
        part: nz(2),
        rederived: None,
    });
    log.add(TunerWarning {
        warning: WarningKind::Monotonicity,
        trial: None,
        passes: Vec::new(),
        detail: String::new(),
    });
    let mut searched = intent("0004", Regime::R1, &[], &[-21, 0, 0, 0]);
    searched.core = Some(super::support::core(0));
    searched.offset = Some(target(-21));
    searched.activity = Some(Activity::Search);
    searched.condition = Condition::Alone;
    searched.duration_s = 60;
    let mut unclear = end("0004", Outcome::Inconclusive);
    unclear.reason = Some("run on\nanother core".to_owned());
    log.at(520).add(searched);
    log.add(trial_start("0004"));
    log.add(unclear);
    let mut cycle_end = cycle(1, CycleEvent::End, true);
    cycle_end.passed = true;
    log.at(600).add(cycle_end);
    log.at(601).add(shutdown(ShutdownReason::Cycles));
    log
}

#[test]
fn a_combination_failure_is_reported_in_the_session_failure_and_request_sections() {
    let log = combination();
    assert_snapshot!(
        "scenario__a_combination_failure_is_reported_in_the_session_failure_and_request_sections",
        log.report()
    );
    let journal = log.journal();
    let p = Projection::new(&journal);
    let m = compute(&p, None);
    assert_eq!(m.runs.len(), 1, "a crash and a reboot make one run");
}

#[test]
fn a_cutoff_between_runs_keeps_the_totals_of_the_run_it_overlaps() {
    let log = combination();
    let journal = log.journal();
    let cutoff = Some(EPOCH + SignedDuration::from_secs(400));
    let recent = report(&journal, cutoff);
    assert!(
        recent.contains("since     2026-01-01T00:06:40Z"),
        "{recent}"
    );
    let p = Projection::new(&journal);
    let m = compute(&p, cutoff);
    assert_eq!(m.runs.len(), 1);
}

#[test]
fn a_journal_of_one_event_reports_empty_sections() {
    assert_snapshot!(
        "scenario__a_journal_of_one_event_reports_empty_sections",
        Log::new(&[(0, 0)]).report()
    );
}

#[test]
fn a_journal_must_open_with_session_start() {
    assert!(matches!(Journal::new(Vec::new()), Err(Error::Empty)));
    let mut log = Log::new(&[(0, 0)]);
    log.add(config_loaded(build("test", None)));
    let events = log.events[1..].to_vec();
    assert!(matches!(Journal::new(events), Err(Error::NotStarted)));
}

#[test]
fn rulesets_are_listed_in_numeric_order_and_builds_by_name() {
    let mut opening = build("b", None);
    opening.ruleset = 5;
    let mut log = Log::with_build(&[(0, 0)], opening);
    let mut later = build("a", Some("x"));
    later.ruleset = 10;
    log.add(config_loaded(later));
    let text = log.report();
    assert!(text.contains("rulesets  5, 10\n"), "{text}");
    assert!(text.contains("builds    a+x, b\n"), "{text}");
}

#[test]
fn a_failure_that_names_neither_a_regime_nor_a_trial_shows_dashes() {
    let mut log = Log::new(&[(0, 0)]);
    let mut idle = failure(None);
    idle.signal = Signal::Crash;
    log.add(idle);
    let text = log.report();
    let row = text
        .lines()
        .skip_while(|line| *line != "Failures")
        .nth(2)
        .unwrap();
    assert_eq!(
        row.split_whitespace().collect::<Vec<_>>(),
        ["-", "-", "crash", "unattributed", "1"],
        "{text}"
    );
}

#[test]
fn an_inconclusive_trial_without_a_reason_shows_a_dash() {
    let mut log = Log::new(&[(0, 0)]);
    trial(
        &mut log,
        intent("0001", Regime::R1, &[0], &[0]),
        Outcome::Inconclusive,
    );
    let text = log.report();
    let row = text
        .lines()
        .skip_while(|line| *line != "Inconclusive trials")
        .nth(2)
        .unwrap();
    assert_eq!(
        row.split_whitespace().collect::<Vec<_>>(),
        ["0001", "together", "-"],
        "{text}"
    );
}

#[test_case::test_case(f64::MAX, false; "positive bin overflow")]
#[test_case::test_case(-f64::MAX, false; "negative bin overflow")]
#[test_case::test_case(f64::MAX, true; "carried positive bin overflow")]
#[test_case::test_case(-f64::MAX, true; "carried negative bin overflow")]
fn a_journal_refuses_unrepresentable_voltage_bins(volts: f64, carried: bool) {
    let mut log = Log::new(&[(0, 0)]);
    let requests = set(&[0]).iter().map(|core| (core, volts)).collect();
    if carried {
        let mut fact = super::support::carried(Outcome::Pass);
        fact.voltage_requests_v = requests;
        log.add(fact);
    } else {
        let mut ended = end("t", Outcome::Pass);
        ended.voltage_requests_v = requests;
        log.add(ended);
    }
    assert!(matches!(
        Journal::new(log.events),
        Err(Error::VoltageBin(_))
    ));
}

#[test_case::test_case(0.0, Some(0); "zero bin")]
#[test_case::test_case(0.004, Some(1); "one bin")]
#[test_case::test_case(-0.004, Some(-1); "negative bin")]
#[test_case::test_case(2.0_f64.powi(63) / 250.0, None; "first positive overflow")]
// Adjacent representable requests at these signed bin limits are 8 V apart.
#[test_case::test_case(2.0_f64.powi(63) / 250.0 - 8.0, Some(i64::MAX - 2047); "last accepted positive request")]
#[test_case::test_case(-2.0_f64.powi(63) / 250.0, Some(i64::MIN); "exact negative limit")]
#[test_case::test_case(-2.0_f64.powi(63) / 250.0 - 8.0, None; "first finite negative overflow")]
#[test_case::test_case(f64::MAX, None; "positive multiplication overflow")]
#[test_case::test_case(-f64::MAX, None; "negative multiplication overflow")]
fn voltage_bins_are_checked_at_signed_boundaries(volts: f64, expected: Option<i64>) {
    assert_eq!(crate::requests::checked_bin(volts).ok(), expected);
}
