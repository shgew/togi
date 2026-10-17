use std::num::NonZeroUsize;

use test_case::test_case;
use togi_tool_sim::{Run, ShardError, Suite, partition, shard};

use crate::real::real_runs;

fn count(value: usize) -> NonZeroUsize {
    NonZeroUsize::new(value).unwrap()
}

fn positions(all: &[Run], part: &[Run]) -> Vec<usize> {
    part.iter()
        .map(|run| all.iter().position(|other| other.id == run.id).unwrap())
        .collect()
}

#[test_case(1; "one shard")]
#[test_case(2; "two shards")]
#[test_case(3; "three shards")]
#[test_case(5; "five shards")]
#[test_case(7; "seven shards")]
#[test_case(10; "ten shards")]
#[test_case(220; "220 shards")]
#[test_case(300; "300 shards")]
fn every_run_lands_in_exactly_one_balanced_shard(shards: usize) {
    let runs = real_runs();
    let parts = partition(&runs, count(shards), |_| 1);
    assert_eq!(parts.len(), shards);
    let mut ids: Vec<&str> = parts.iter().flatten().map(|run| run.id.as_str()).collect();
    ids.sort_unstable();
    let mut want: Vec<&str> = runs.iter().map(|run| run.id.as_str()).collect();
    want.sort_unstable();
    assert_eq!(ids, want);
    let sizes: Vec<usize> = parts.iter().map(Vec::len).collect();
    let (min, max) = (sizes.iter().min().unwrap(), sizes.iter().max().unwrap());
    assert!(max - min <= 1, "{sizes:?}");
}

#[test]
fn each_shard_keeps_suite_order() {
    let runs = real_runs();
    for part in partition(&runs, count(7), |_| 1) {
        let at = positions(&runs, &part);
        assert!(at.windows(2).all(|pair| pair[0] < pair[1]), "{at:?}");
    }
}

#[test]
fn one_shard_is_the_whole_suite() {
    let runs = real_runs();
    assert_eq!(shard(&runs, count(1), 0).unwrap(), runs);
}

#[test]
fn the_partition_is_deterministic_and_shard_agrees_with_it() {
    let runs = real_runs();
    let parts = partition(&runs, count(6), |_| 1);
    assert_eq!(parts, partition(&runs, count(6), |_| 1));
    for (index, part) in parts.iter().enumerate() {
        assert_eq!(&shard(&runs, count(6), index).unwrap(), part);
    }
}

#[test]
fn a_shard_outside_the_count_is_an_error() {
    let runs = real_runs();
    assert_eq!(
        shard(&runs, count(4), 4),
        Err(ShardError {
            index: 4,
            count: count(4)
        })
    );
}

#[test]
fn more_shards_than_runs_leaves_the_extra_shards_empty() {
    let suite = Suite::parse(
        r#"{"scenarios":[{"name":"x","dev":[1,2]}]}"#,
        std::path::Path::new(""),
    )
    .unwrap();
    let parts = partition(&suite.runs(), count(3), |_| 1);
    assert_eq!(parts.iter().map(Vec::len).collect::<Vec<_>>(), [1, 1, 0]);
}

#[test]
fn costly_runs_are_spread_by_cost() {
    let runs = real_runs();
    let heavy = |run: &Run| if run.scenario == "target" { 10 } else { 1 };
    let parts = partition(&runs, count(4), heavy);
    let loads: Vec<u64> = parts
        .iter()
        .map(|part| part.iter().map(heavy).sum())
        .collect();
    let (min, max) = (loads.iter().min().unwrap(), loads.iter().max().unwrap());
    assert!(max - min <= 10, "{loads:?}");
    let target: Vec<usize> = parts
        .iter()
        .map(|part| part.iter().filter(|run| run.scenario == "target").count())
        .collect();
    assert!(
        target.iter().max().unwrap() - target.iter().min().unwrap() <= 1,
        "{target:?}"
    );
}
