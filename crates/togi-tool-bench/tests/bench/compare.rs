use crate::common::{self, record};
use togi_tool_bench::compare::{Pair, Verdict, compare, pairing, report};
use togi_tool_bench::summary;
use togi_tool_bench::{ModelCheck, Record, Status, set_scenario_shares};
use togi_tool_sim::Split;

fn pair<'a>(candidate: &'a Record, baseline: &'a Record) -> Pair<'a> {
    Pair {
        candidate,
        baseline,
    }
}

fn base() -> Record {
    Record {
        hazard_max_per_h: 0.1,
        ..record("default", 1)
    }
}

fn violations(c: &togi_tool_bench::compare::Comparison) -> [usize; 3] {
    [c.v1, c.v2, c.v3]
}

fn text(write: impl FnOnce(&mut Vec<u8>) -> std::io::Result<()>) -> String {
    let mut out = Vec::new();
    write(&mut out).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn a_baseline_file_compares_exactly_as_the_full_records_do() {
    let full: Vec<Record> = (1..=6_u32)
        .map(|n| Record {
            seed: u64::from(n),
            wall_s: Some(30.0 + f64::from(n)),
            model_check: Some(ModelCheck {
                eligible_groups: 3,
                flagged_groups: 1,
                idle_failures: 0,
            }),
            machine: Some("m.json".to_owned()),
            sim_hours: 10.0 + f64::from(n),
            ..base()
        })
        .collect();
    let candidate: Vec<Record> = full
        .iter()
        .map(|run| Record {
            wall_s: Some(1.0),
            model_check: None,
            crashes: 2,
            sim_hours: run.sim_hours * 1.5,
            ..run.clone()
        })
        .collect();
    let slim: Vec<Record> = full.iter().map(Record::baseline).collect();
    let against_full = text(|out| report(out, &candidate, &full));
    let against_slim = text(|out| report(out, &candidate, &slim));
    assert!(against_full.contains("verdict: "), "{against_full}");
    assert!(against_full.contains("max_hazard_delta"), "{against_full}");
    assert_eq!(against_slim, against_full);
}

type Edit = (&'static str, fn(&mut Record), Verdict, [usize; 3]);

#[test]
fn runs_pair_by_scenario_and_seed_in_scenario_then_seed_order() {
    let a = [
        record("b", 2),
        record("a", 9),
        record("a", 1),
        record("a", 8),
    ];
    let b = [
        record("a", 9),
        record("b", 2),
        record("c", 1),
        record("a", 1),
    ];
    let keys: Vec<_> = pairing(&a, &b)
        .iter()
        .map(|p| (p.candidate.scenario.clone(), p.candidate.seed))
        .collect();
    assert_eq!(
        keys,
        [
            ("a".to_owned(), 1),
            ("a".to_owned(), 9),
            ("b".to_owned(), 2)
        ]
    );
}

#[test_case::test_case(("equal", |_| {}, Verdict::Neutral, [0; 3]); "equal")]
#[test_case::test_case(("faster", |r| r.sim_hours = 8.0, Verdict::Accept, [0; 3]); "faster")]
#[test_case::test_case(("slower", |r| r.sim_hours = 12.0, Verdict::Reject, [0; 3]); "slower")]
#[test_case::test_case((
            "lost conclusion",
            |r| r.status = Status::DeadEnd,
            Verdict::Reject,
            [1, 0, 0],
        ); "lost conclusion")]
#[test_case::test_case((
            "censored",
            |r| {
                r.status = Status::Censored;
                r.sim_hours = 40.0;
            },
            Verdict::Reject,
            [1, 0, 0],
        ); "censored")]
#[test_case::test_case((
            "hazard",
            |r| r.hazard_max_per_h = 0.111,
            Verdict::Reject,
            [0, 1, 0],
        ); "hazard")]
fn the_verdict_follows_the_violations_and_the_time_interval(case: Edit) {
    let (name, edit, verdict, expected) = case;

    let baseline = base();

    let mut candidate = base();
    edit(&mut candidate);
    let c = compare(&[pair(&candidate, &baseline)]);
    assert_eq!(c.verdict(), verdict, "{name}");
    assert_eq!(violations(&c), expected, "{name}");
}

#[test]
fn a_slower_target_is_a_violation_and_no_pairs_is_neutral() {
    let (mut a, mut b) = (base(), base());
    a.scenario = "target".to_owned();
    b.scenario = "target".to_owned();
    a.sim_hours = 11.0;
    let c = compare(&[pair(&a, &b)]);
    assert_eq!(violations(&c), [0, 0, 1]);
    assert_eq!(c.verdict(), Verdict::Reject);
    assert_eq!(compare(&[]).verdict(), Verdict::Neutral);
}

#[test]
fn a_hazard_rise_of_exactly_the_threshold_is_not_a_violation() {
    let mut a = base();
    let mut b = base();
    b.hazard_max_per_h = 0.0;
    a.hazard_max_per_h = 0.01;
    assert_eq!(compare(&[pair(&a, &b)]).v2, 0);
}

#[test]
fn the_time_ratio_weights_scenarios_equally_and_the_bootstrap_is_fixed() {
    let mut candidates = Vec::new();
    let mut baselines = Vec::new();
    for seed in 0..24 {
        candidates.push(Record {
            sim_hours: 5.0,
            ..record("default", seed)
        });
        baselines.push(record("default", seed));
    }
    candidates.push(Record {
        sim_hours: 20.0,
        ..record("target", 0)
    });
    baselines.push(record("target", 0));
    let pairs: Vec<_> = candidates
        .iter()
        .zip(&baselines)
        .map(|(a, b)| pair(a, b))
        .collect();
    let c = compare(&pairs);
    assert!((c.ratio - 1.0).abs() < 1e-12, "{}", c.ratio);
    assert!((c.lo - 1.0).abs() < 1e-12 && (c.hi - 1.0).abs() < 1e-12);
    assert_eq!(c.v3, 1);
    for (i, candidate) in candidates.iter_mut().enumerate() {
        candidate.sim_hours = 5.0 + f64::from(u8::try_from(i % 3).unwrap());
    }
    let pairs: Vec<_> = candidates
        .iter()
        .zip(&baselines)
        .map(|(a, b)| pair(a, b))
        .collect();
    let (first, second) = (compare(&pairs), compare(&pairs));
    assert_eq!(first, second);
    assert!(first.lo <= first.hi && !first.lo.is_nan());
}

#[test]
fn real_answer_shares_pool_over_the_scenario_and_the_pairs() {
    let mut rows = [
        Record {
            trials: 2,
            real_answers: 1,
            ..record("target", 1)
        },
        Record {
            trials: 6,
            real_answers: 1,
            ..record("target", 2)
        },
        record("empty", 1),
    ];
    set_scenario_shares(&mut rows);
    let shares: Vec<_> = rows.iter().map(|r| r.scenario_real_answer_share).collect();
    assert_eq!(shares, [0.25, 0.25, 0.0]);
    let base = [
        Record {
            trials: 10,
            real_answers: 1,
            ..record("target", 1)
        },
        Record {
            trials: 10,
            real_answers: 3,
            ..record("target", 2)
        },
    ];
    let c = compare(&[pair(&rows[0], &base[0]), pair(&rows[1], &base[1])]);
    assert_eq!(
        [c.real_answer_share, c.baseline_real_answer_share],
        [0.25, 0.2]
    );
}

fn candidates() -> Vec<Record> {
    let r = |scenario, seed, status, hours, crashes, hazard, trials, real| Record {
        status,
        sim_hours: hours,
        crashes,
        hazard_max_per_h: hazard,
        trials,
        real_answers: real,
        ..record(scenario, seed)
    };
    vec![
        r("zeta", 2, Status::Concluded, 4.0, 2, 0.005, 2, 1),
        r("alpha", 1, Status::Concluded, 2.0, 0, 0.0, 6, 1),
        r("zeta", 3, Status::Error, 10.0, 4, 0.015, 8, 1),
        r("zeta", 4, Status::Censored, 300.0, 999, 0.01, 9000, 0),
    ]
}

fn baselines() -> Vec<Record> {
    let r = |scenario, seed, split, hours, crashes, hazard, trials, real| Record {
        split,
        sim_hours: hours,
        crashes,
        hazard_max_per_h: hazard,
        trials,
        real_answers: real,
        ..record(scenario, seed)
    };
    vec![
        r("alpha", 1, Split::Dev, 4.0, 0, 0.0, 4, 2),
        r("zeta", 2, Split::Dev, 8.0, 1, 0.005, 4, 3),
        r("alpha", 9, Split::Holdout, 1.0, 0, 0.0, 0, 0),
    ]
}

#[test]
fn the_summary_lists_scenarios_in_record_order_then_the_total() {
    common::assert_snapshot(
        &text(|out| summary::report(out, &candidates())),
        include_str!(
            "snapshots/compare__the_summary_lists_scenarios_in_record_order_then_the_total.snap"
        ),
    );
}

#[test]
fn the_comparison_names_each_scenario_and_the_verdict() {
    common::assert_snapshot(
        &text(|out| report(out, &candidates()[..2], &baselines())),
        include_str!("snapshots/compare__the_comparison_names_each_scenario_and_the_verdict.snap"),
    );
}

#[test]
fn unmatched_runs_in_the_candidates_splits_are_warned_about() {
    let mut base = baselines();
    base.push(record("missing", 7));
    common::assert_snapshot(
        &text(|out| report(out, &candidates(), &base)),
        include_str!(
            "snapshots/compare__unmatched_runs_in_the_candidates_splits_are_warned_about.snap"
        ),
    );
}

#[test]
fn a_rejected_candidate_says_which_violations() {
    let mut a = candidates().remove(0);
    a.status = Status::DeadEnd;
    a.hazard_max_per_h = 0.1;
    let base = baselines().remove(1);
    common::assert_snapshot(
        &text(|out| report(out, &[a], &[base])),
        include_str!("snapshots/compare__a_rejected_candidate_says_which_violations.snap"),
    );
}

#[test]
fn a_baseline_run_that_changed_split_pairs_and_is_not_counted_unmatched() {
    let candidate = Record {
        split: Split::Dev,
        ..record("default", 5)
    };
    let baseline = Record {
        split: Split::Holdout,
        ..record("default", 5)
    };
    let out = text(|out| report(out, &[candidate], &[baseline]));
    assert!(!out.contains("WARNING"), "{out}");
    assert!(out.contains("pairs=1"), "{out}");
}
