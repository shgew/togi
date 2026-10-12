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
        loads[lightest] = loads[lightest].saturating_add(cost(&runs[position]));
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
