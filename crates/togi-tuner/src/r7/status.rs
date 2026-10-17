use std::collections::HashMap;

use togi_machine::{CoreId, CoreSet, TargetOffset};

use togi_machine::WorkloadId;

use crate::{BackendId, Depth, Profile};

const BUCKETS: usize = 51;

/// Passes by the offset the core had, as depth from 0 to 50.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Depths([u32; BUCKETS]);

impl Default for Depths {
    fn default() -> Self {
        Self([0; BUCKETS])
    }
}

fn bucket(offset: TargetOffset) -> usize {
    usize::try_from(-offset.signed())
        .unwrap_or(0)
        .min(BUCKETS - 1)
}

impl Depths {
    fn add(&mut self, bucket: usize) {
        self.0[bucket] += 1;
    }

    fn subtract(&mut self, other: &Self) {
        for (n, gone) in self.0.iter_mut().zip(other.0) {
            *n = n.saturating_sub(gone);
        }
    }

    fn at_least(&self, bucket: usize) -> u32 {
        self.0[bucket..].iter().sum()
    }

    fn is_empty(&self) -> bool {
        self.0.iter().all(|&n| n == 0)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Cell {
    total: Depths,
    by_load: HashMap<CoreSet, Depths>,
}

/// Per `(backend, R7 workload, core)`, the R7 passes in which the core was a top requester, by
/// the offset it had then, pooled across durations and loaded sets. A core is self-sufficient
/// for a workload once one pass was at an offset equal to or deeper than its current one.
/// Failures never enter, and no later failure invalidates a pass. A reset of a core removes
/// every pass whose load includes it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SelfSufficiency {
    cells: HashMap<(BackendId, WorkloadId, CoreId), Cell>,
}

impl SelfSufficiency {
    /// Counts a passing R7 trial of `loaded` under `backend`: each core of `top`, the trial's
    /// top requesters, gets a pass at its offset in `profile`.
    pub(crate) fn pass(
        &mut self,
        backend: BackendId,
        workload: WorkloadId,
        loaded: CoreSet,
        top: CoreSet,
        profile: &Profile,
    ) {
        for (core, offset) in profile.among(top) {
            let at = bucket(offset);
            let cell = self.cells.entry((backend, workload, core)).or_default();
            cell.total.add(at);
            cell.by_load.entry(loaded).or_default().add(at);
        }
    }

    /// The passes in which `core` was a top requester at an offset equal to or deeper than
    /// `current`.
    pub(crate) fn passes(
        &self,
        backend: BackendId,
        workload: WorkloadId,
        core: CoreId,
        current: TargetOffset,
    ) -> u32 {
        self.cells
            .get(&(backend, workload, core))
            .map_or(0, |cell| cell.total.at_least(bucket(current)))
    }

    #[cfg(test)]
    pub(crate) fn is_self_sufficient(
        &self,
        backend: BackendId,
        workload: WorkloadId,
        core: CoreId,
        current: TargetOffset,
    ) -> bool {
        self.passes(backend, workload, core, current) > 0
    }

    /// Removes the passes whose load includes `core`, from every core's count.
    pub(crate) fn reset_core(&mut self, core: CoreId) {
        self.cells.retain(|_, cell| {
            let doomed: Vec<CoreSet> = cell
                .by_load
                .keys()
                .copied()
                .filter(|loaded| loaded.contains(core))
                .collect();
            for loaded in doomed {
                if let Some(depths) = cell.by_load.remove(&loaded) {
                    cell.total.subtract(&depths);
                }
            }
            !cell.total.is_empty()
        });
    }
}
