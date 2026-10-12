use std::collections::BTreeMap;

use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed};

use super::support::{core, run};
use crate::summary::Range;
use crate::{Error, Metric, MetricKind, Status, metrics, summarize};

fn range(min: f64, p10: f64, median: f64, p90: f64, max: f64) -> Range {
    Range {
        min,
        p10,
        median,
        p90,
        max,
    }
}

fn cores(ranges: &[Range]) -> BTreeMap<togi_machine::CoreId, Range> {
    ranges
        .iter()
        .enumerate()
        .map(|(n, range)| (core(i64::try_from(n).unwrap()), *range))
        .collect()
}

#[test]
fn an_odd_count_has_the_middle_run_as_its_median() {
    let runs = [
        run(Status::Concluded, 30.0, 3, 1, &[-5, -9]),
        run(Status::Concluded, 10.0, 1, 0, &[-1, -7]),
        run(Status::Concluded, 20.0, 2, 2, &[-3, -8]),
    ];
    let got = summarize(&runs).unwrap();
    assert_eq!(
        (got.runs, got.concluded, got.dead_ends, got.censored),
        (3, 3, 0, 0)
    );
    assert_eq!(got.hours, Some(range(10.0, 10.0, 20.0, 30.0, 30.0)));
    assert_eq!(got.crashes, range(1.0, 1.0, 2.0, 3.0, 3.0));
    assert_eq!(got.hunts, range(0.0, 0.0, 1.0, 2.0, 2.0));
    assert_eq!(got.depth, range(-14.0, -14.0, -11.0, -8.0, -8.0));
    assert_eq!(
        got.cores,
        cores(&[
            range(-5.0, -5.0, -3.0, -1.0, -1.0),
            range(-9.0, -9.0, -8.0, -7.0, -7.0)
        ])
    );
}

#[test]
fn an_even_count_averages_the_middle_two() {
    let runs = [
        run(Status::Concluded, 4.0, 0, 0, &[-1]),
        run(Status::Concluded, 1.0, 0, 0, &[-2]),
        run(Status::Concluded, 3.0, 0, 0, &[-4]),
        run(Status::Concluded, 2.0, 0, 0, &[-3]),
    ];
    let got = summarize(&runs).unwrap();
    assert_eq!(got.hours, Some(range(1.0, 1.0, 2.5, 4.0, 4.0)));
    assert_eq!(got.depth, range(-4.0, -4.0, -2.5, -1.0, -1.0));
    assert_eq!(got.cores, cores(&[range(-4.0, -4.0, -2.5, -1.0, -1.0)]));
}

#[test]
fn censored_and_dead_end_runs_count_everywhere_but_hours() {
    let runs = [
        run(Status::Concluded, 5.0, 1, 1, &[-6]),
        run(Status::Censored, 50.0, 9, 3, &[-2]),
        run(Status::DeadEnd, 7.0, 4, 2, &[0]),
    ];
    let got = summarize(&runs).unwrap();
    assert_eq!(
        (got.runs, got.concluded, got.dead_ends, got.censored),
        (3, 1, 1, 1)
    );
    assert_eq!(got.hours, Some(range(5.0, 5.0, 5.0, 5.0, 5.0)));
    assert_eq!(got.crashes, range(1.0, 1.0, 4.0, 9.0, 9.0));
    assert_eq!(got.hunts, range(1.0, 1.0, 2.0, 3.0, 3.0));
    assert_eq!(got.depth, range(-6.0, -6.0, -2.0, 0.0, 0.0));
}

#[test]
fn without_a_concluded_run_hours_have_no_range() {
    let runs = [
        run(Status::Censored, 50.0, 9, 3, &[-2]),
        run(Status::DeadEnd, 7.0, 4, 2, &[0]),
    ];
    let got = summarize(&runs).unwrap();
    assert_eq!(got.hours, None);
    assert_eq!(got.crashes, range(4.0, 4.0, 6.5, 9.0, 9.0));
    assert_eq!(got.hunts, range(2.0, 2.0, 2.5, 3.0, 3.0));
    assert_eq!(got.depth, range(-2.0, -2.0, -1.0, 0.0, 0.0));
    let all = metrics(&runs).unwrap();
    let hours = all.iter().find(|m| m.kind == MetricKind::Hours).unwrap();
    assert_eq!(hours.range, None);
    assert_eq!(hours.percentile(1.0, false), None);
}

#[test]
fn percentiles_use_the_nearest_rank() {
    let runs: Vec<_> = (1..=36)
        .map(|n| run(Status::Concluded, f64::from(n), 0, 0, &[0]))
        .collect();
    let got = summarize(&runs).unwrap();
    assert_eq!(got.hours, Some(range(1.0, 4.0, 18.5, 33.0, 36.0)));
}

#[test]
fn metrics_list_cores_in_core_order_then_depth_hours_crashes_hunts() {
    let runs = [run(Status::Concluded, 1.0, 0, 0, &[-1, -2, -3])];
    let names: Vec<_> = metrics(&runs)
        .unwrap()
        .iter()
        .map(|metric| metric.kind.to_string())
        .collect();
    assert_eq!(
        names,
        [
            "core 00", "core 01", "core 02", "depth", "hours", "crashes", "hunts"
        ]
    );
}

#[test]
fn only_hours_crashes_and_hunts_can_still_grow() {
    let grows: Vec<_> = [
        MetricKind::Core(core(0)),
        MetricKind::Depth,
        MetricKind::Hours,
        MetricKind::Crashes,
        MetricKind::Hunts,
    ]
    .map(MetricKind::grows)
    .into();
    assert_eq!(grows, [false, false, true, true, true]);
}

#[test]
fn no_runs_or_runs_over_different_cores_are_refused() {
    assert!(matches!(metrics(&[]), Err(Error::NoRuns)));
    assert!(matches!(summarize(&[]), Err(Error::NoRuns)));
    let mut odd = run(Status::Concluded, 1.0, 0, 0, &[-1, -2, -3]);
    odd.machine = "odd.json".to_owned();
    odd.seed = 7;
    let error = summarize(&[run(Status::Concluded, 1.0, 0, 0, &[-1, -2]), odd]).unwrap_err();
    assert_eq!(
        error.to_string(),
        "run odd.json seed 7 has cores [0, 1, 2], others [0, 1]"
    );
}

#[test]
fn hours_format_to_two_decimals_and_the_rest_as_they_are() {
    let runs = [run(Status::Concluded, 1.0, 0, 0, &[-1])];
    let all = metrics(&runs).unwrap();
    let by = |kind| all.iter().find(|m| m.kind == kind).unwrap();
    assert_eq!(by(MetricKind::Hours).format(2.5), "2.50");
    assert_eq!(by(MetricKind::Hours).format(3.0), "3.00");
    assert_eq!(by(MetricKind::Crashes).format(3.0), "3");
    assert_eq!(by(MetricKind::Depth).format(-12.5), "-12.5");
    assert_eq!(by(MetricKind::Core(core(0))).format(-0.25), "-0.25");
}

fn metric_of(values: &[f64]) -> Metric {
    let runs: Vec<_> = values
        .iter()
        .map(|&hours| run(Status::Concluded, hours, 0, 0, &[0]))
        .collect();
    metrics(&runs)
        .unwrap()
        .into_iter()
        .find(|metric| metric.kind == MetricKind::Hours)
        .unwrap()
}

#[test]
fn a_percentile_is_the_share_below_plus_half_the_ties_or_only_the_share_below() {
    let metric = metric_of(&[1.0, 2.0, 2.0, 4.0]);
    assert_eq!(metric.percentile(2.0, false), Some(50.0));
    assert_eq!(metric.percentile(2.0, true), Some(25.0));
    assert_eq!(metric.percentile(0.0, false), Some(0.0));
    assert_eq!(metric.percentile(5.0, false), Some(100.0));
}

fn fixed() -> Config {
    Config {
        cases: 128,
        rng_seed: RngSeed::Fixed(0x666f_7265_6361_7374),
        failure_persistence: None,
        ..Config::default()
    }
}

proptest! {
    #![proptest_config(fixed())]

    #[test]
    fn a_range_is_ordered_and_its_ranks_are_values_some_run_produced(
        values in proptest::collection::vec(-1000i32..1000, 1..60)
    ) {
        let floats: Vec<f64> = values.iter().map(|&v| f64::from(v)).collect();
        let got = metric_of(&floats).range.unwrap();
        prop_assert!(got.min <= got.p10 && got.p10 <= got.p90 && got.p90 <= got.max);
        prop_assert!(got.min <= got.median && got.median <= got.max);
        for rank in [got.min, got.p10, got.p90, got.max] {
            prop_assert!(floats.contains(&rank));
        }
        let mut shuffled = floats.clone();
        shuffled.reverse();
        prop_assert_eq!(metric_of(&shuffled).range.unwrap(), got);
    }

    #[test]
    fn a_percentile_stays_within_zero_and_a_hundred_and_grows_with_the_value(
        values in proptest::collection::vec(-50i32..50, 1..40),
        low in -60i32..60,
        step in 0i32..20,
    ) {
        let floats: Vec<f64> = values.iter().map(|&v| f64::from(v)).collect();
        let metric = metric_of(&floats);
        let (a, b) = (f64::from(low), f64::from(low + step));
        for below in [false, true] {
            let (pa, pb) = (
                metric.percentile(a, below).unwrap(),
                metric.percentile(b, below).unwrap(),
            );
            prop_assert!((0.0..=100.0).contains(&pa) && (0.0..=100.0).contains(&pb));
            prop_assert!(pa <= pb);
        }
    }
}
