use jiff::SignedDuration;
use togi_journal::{
    Activity, Attribution, CheckingChain, Combination, CycleEvent, Decision, HuntOutcome,
    HuntStage, Member, Outcome, ShutdownReason, TrialProgress, TunerWarning, WarningKind,
};
use togi_machine::{Condition, CoreSet, PerCore, Regime, ResetKind, Signal};

use super::support::{
    EPOCH, Log, build, config_loaded, crash_detected, cycle, decision, end, failure, hunt_end,
    hunt_group, hunt_start, intent, nz, profile, set, shutdown, target, trial, trial_start,
    workload,
};
use crate::evidence::{GroupReuse, StageReuse};
use crate::hunts::StepKey;
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
fn the_review_boundary_scenario_keeps_the_evidence_a_cutoff_hides() {
    let (log, cutoff) = review_boundary();
    let journal = log.journal();
    let p = Projection::new(&journal);
    let all = compute(&p, None);
    let recent = compute(&p, Some(cutoff));
    let group = |group, passes, failures| GroupReuse {
        hunt: 2,
        group,
        stage: HuntStage::Part,
        prior: 1,
        required: 1,
        established: true,
        passes,
        failures,
        trials: 1,
        seconds: 60,
    };
    for m in [&all, &recent] {
        assert_eq!(
            m.evidence.groups,
            [group(1, 1, 0), group(2, 0, 1), group(3, 0, 0)]
        );
        assert_eq!(
            m.evidence.stages.iter().collect::<Vec<_>>(),
            [(
                &(2, HuntStage::Part),
                &StageReuse {
                    groups: 3,
                    established: 3,
                    passed: 1,
                    failed: 1,
                    trials: 3,
                    seconds: 180
                }
            )]
        );
    }
    let key = |outcome| StepKey {
        rerun: false,
        cycle: 1,
        regime: Regime::R6,
        cores: "00,01".to_owned(),
        outcome: Some(outcome),
    };
    assert_eq!(
        all.checking.steps.iter().collect::<Vec<_>>(),
        [(&key(Outcome::Pass), &1), (&key(Outcome::Failure), &1)]
    );
    let cancelled = &all.hunts[0];
    assert_eq!(
        (
            cancelled.hunt.groups.len(),
            cancelled.ran,
            cancelled.inferred,
            cancelled.skipped
        ),
        (1, 0, 0, 1)
    );
    assert!(cancelled.hunt.start.parked_seq.is_some());
    assert_eq!(recent.hunts.len(), 1);
    assert_eq!(recent.hunts[0].hunt.id, 2);
    assert_eq!(
        recent.evidence.warnings.iter().collect::<Vec<_>>(),
        [(&WarningKind::Monotonicity, &1)]
    );
    assert_eq!(recent.evidence.single_carried, 0);
    assert_eq!(all.inconclusive.len(), 1);
    assert_eq!(all.inconclusive[0].intent.trial.as_str(), "2");
    assert_eq!(all.inconclusive[0].intent.condition, Condition::Parked);
    assert_eq!(recent.checking, crate::hunts::CheckingMetrics::default());
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
        ccd: togi_machine::CcdId(1),
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
    log.at(600).add(cycle(1, CycleEvent::End, true));
    log.at(601).add(shutdown(ShutdownReason::Cycles));
    log
}

#[test]
fn a_combination_failure_is_reported_in_every_section() {
    let log = combination();
    assert_snapshot!(
        "scenario__a_combination_failure_is_reported_in_every_section",
        log.report()
    );
    let journal = log.journal();
    let p = Projection::new(&journal);
    let m = compute(&p, None);
    assert_eq!(m.runs.len(), 1, "a crash and a reboot make one run");
    assert_eq!(m.hunts[0].hunt.commitment.as_deref(), Some("03 -22->-19"));
    assert_eq!(m.r7.backoffs.len(), 1);
    assert_eq!(m.r7.chains.len(), 1);
}

#[test]
fn a_cutoff_between_runs_and_hunts_keeps_the_totals_of_the_run_it_overlaps() {
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
    assert!(m.hunts.is_empty(), "the hunt began before the cutoff");
    assert!(m.r7.backoffs.len() == 1, "the backoff came after it");
}

#[test]
fn r7_backoffs_and_chains_are_listed_with_their_causes_and_sources() {
    let mut log = Log::with_build(&[(0, 0), (1, 1)], build("test", None));
    let mut planned = intent("0001", Regime::R7, &[0, 1], &[0, 0]);
    planned.workload = workload(Regime::R7, 0);
    log.add(planned);
    let mut multi = failure(Some("0001"));
    multi.regime = Some(Regime::R7);
    let multi = log.add(multi);
    let mut voltage = decision(Decision::Backoff, 0, -20, -17);
    voltage.reason = "request 1.100 V\nto 1.110 V".to_owned();
    log.add_caused(&[multi], voltage);
    let chain =
        |cycle, step, ccd, groups: Vec<CoreSet>, cores, sources: Vec<_>, part| CheckingChain {
            cycle: nz(cycle),
            step: nz(step),
            ccd: togi_machine::CcdId(ccd),
            workload: workload(Regime::R7, 0),
            groups,
            cores,
            source_seqs: sources,
            profile: profile(&[0, 0]),
            part: nz(part),
            rederived: None,
        };
    log.add(chain(
        1,
        2,
        0,
        vec![set(&[0]), set(&[1])],
        set(&[1]),
        Vec::new(),
        1,
    ));
    let mut single = intent("0002", Regime::R7, &[1], &[0, 0]);
    single.core = Some(super::support::core(1));
    single.cores = CoreSet::default();
    log.add(single);
    let mut solo = failure(Some("0002"));
    solo.regime = Some(Regime::R7);
    let solo = log.add(solo);
    log.add_caused(&[solo], decision(Decision::Backoff, 1, -20, -19));
    log.add(chain(
        2,
        1,
        1,
        vec![set(&[0, 1])],
        set(&[0, 1]),
        vec![multi, solo],
        1,
    ));
    let carried_failure = |cores: &[i64]| {
        let mut fact = super::evidence::carried(Outcome::Failure);
        fact.class = togi_journal::TrialClass {
            regime: Regime::R7,
            workload: Some(workload(Regime::R7, 0)),
            cores: set(cores),
            duration_s: 120,
        };
        fact
    };
    let carried_multi = log.add(carried_failure(&[0, 1]));
    log.add_caused(&[carried_multi], decision(Decision::Backoff, 0, -20, -16));
    let carried_single = log.add(carried_failure(&[1]));
    log.add_caused(&[carried_single], decision(Decision::Backoff, 1, -20, -19));
    log.add(intent("0003", Regime::R7, &[0, 1], &[0, 0]));
    log.add(trial_start("0003"));
    let ended = log.add(end("0003", Outcome::Failure));
    log.add_caused(&[ended], decision(Decision::Backoff, 0, -20, -15));
    let journal = log.journal();
    let p = Projection::new(&journal);
    let r = crate::r7::decisions(p.events, None);
    assert_eq!(
        r.backoffs
            .iter()
            .map(|b| (b.seq.get(), b.steps))
            .collect::<Vec<_>>(),
        [(4, 3), (11, 4), (17, 5)],
        "only backoffs of multi-core R7 failures are voltage-targeted: live, carried or ended"
    );
    assert_eq!(
        r.chains.iter().map(|c| c.seq.get()).collect::<Vec<_>>(),
        [5, 9]
    );
    assert_snapshot!(
        "scenario__r7_backoffs_and_chains_are_listed_with_their_causes_and_sources",
        super::support::rendered(|table| {
            crate::r7::render_decisions(table, &r);
        })
    );
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
