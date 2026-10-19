use std::num::NonZeroUsize;

use crate::run::Run;

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
#[error("shard {index} is outside the {count} shards")]
pub struct ShardError {
    pub index: usize,
    pub count: NonZeroUsize,
}

/// Splits `runs` into `count` shards by the cost of each run: the costliest run first goes to the shard with the
/// least cost so far, the lowest index breaking ties. Each shard keeps the runs in their suite order, and the
/// result depends on nothing but the runs, the count and the costs, so every machine computes the same shards.
pub fn partition(runs: &[Run], count: NonZeroUsize, cost: impl Fn(&Run) -> u64) -> Vec<Vec<Run>> {
    let mut order: Vec<usize> = (0..runs.len()).collect();
    order.sort_by_key(|&position| std::cmp::Reverse(cost(&runs[position])));
    let mut loads = vec![0_u64; count.get()];
    let mut assigned: Vec<Vec<usize>> = vec![Vec::new(); count.get()];
    for position in order {
        let lightest = loads
            .iter()
            .enumerate()
            .min_by_key(|&(index, &load)| (load, index))
            .map_or(0, |(index, _)| index);
        loads[lightest] = saturating_shard_load(loads[lightest], cost(&runs[position]));
        assigned[lightest].push(position);
    }
    assigned
        .into_iter()
        .map(|mut positions| {
            positions.sort_unstable();
            positions
                .into_iter()
                .map(|position| runs[position].clone())
                .collect()
        })
        .collect()
}

/// The runs of shard `index` when every run costs the same: shard sizes differ by at most one.
pub fn shard(runs: &[Run], count: NonZeroUsize, index: usize) -> Result<Vec<Run>, ShardError> {
    partition(runs, count, |_| 1)
        .into_iter()
        .nth(index)
        .ok_or(ShardError { index, count })
}

/// Approximate shard costs cap at u64::MAX; saturated ties retain the lowest-index tie-break.
fn saturating_shard_load(load: u64, cost: u64) -> u64 {
    load.saturating_add(cost)
}

#[cfg(test)]
mod numeric_tests {
    use super::saturating_shard_load;

    #[test_case::test_case(0, 0, 0; "empty load")]
    #[test_case::test_case(u64::MAX - 1, 1, u64::MAX; "last exact load")]
    #[test_case::test_case(u64::MAX, 1, u64::MAX; "first overflowing load")]
    fn shard_loads_saturate(load: u64, cost: u64, expected: u64) {
        assert_eq!(saturating_shard_load(load, cost), expected);
    }
}
