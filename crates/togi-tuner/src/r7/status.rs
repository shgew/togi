use std::collections::HashMap;

use togi_machine::{CoreId, CoreSet, PerCore, TargetOffset};

use togi_machine::WorkloadId;

use crate::{BackendId, Depth, Profile};

const BUCKETS: usize = 51;

/// Passes by the offset the core had, as depth from 0 to 50.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Depths([u32; BUCKETS]);

impl serde::Serialize for Depths {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(self.0.as_slice(), serializer)
    }
}

impl<'de> serde::Deserialize<'de> for Depths {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Depths;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(formatter, "exactly {BUCKETS} depth counters")
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut sequence: A,
            ) -> Result<Depths, A::Error> {
                let mut depths = [0; BUCKETS];
                for (index, depth) in depths.iter_mut().enumerate() {
                    *depth = sequence
                        .next_element()?
                        .ok_or_else(|| serde::de::Error::invalid_length(index, &self))?;
                }
                if sequence.next_element::<serde::de::IgnoredAny>()?.is_some() {
                    return Err(serde::de::Error::invalid_length(BUCKETS + 1, &self));
                }
                Ok(Depths(depths))
            }
        }
        deserializer.deserialize_seq(Visitor)
    }
}

impl Default for Depths {
    fn default() -> Self {
        Self([0; BUCKETS])
    }
}

fn bucket(offset: TargetOffset) -> usize {
    match usize::try_from(-offset.signed()) {
        Ok(depth) => depth,
        Err(_) => unreachable!("TargetOffset depth is bounded to zero through 50"),
    }
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

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct Cell {
    total: Depths,
    #[serde(with = "togi_machine::serde_pairs")]
    by_load: HashMap<CoreSet, Depths>,
}

/// Per `(backend, R7 workload, core)`, the R7 passes in which the core was a top requester, by
/// the offset it had then, pooled across durations and loaded sets. A core is self-sufficient
/// for a workload once one pass was at an offset equal to or deeper than its current one.
/// Failures never enter, and no later failure invalidates a pass. A reset of a core removes
/// every pass whose load includes it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct SelfSufficiency {
    #[serde(with = "togi_machine::serde_pairs")]
    cells: HashMap<(BackendId, WorkloadId), PerCore<Option<Cell>>>,
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
            let cell = self
                .cells
                .entry((backend, workload))
                .or_default()
                .get_or_insert_with(core, Cell::default);
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
            .get(&(backend, workload))
            .and_then(|cores| cores.get_ref(core))
            .and_then(Option::as_ref)
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
        self.cells.retain(|_, cores| {
            for (_, slot) in cores.iter_mut() {
                let Some(cell) = slot.as_mut() else { continue };
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
                if cell.total.is_empty() {
                    *slot = None;
                }
            }
            cores.iter_ref().any(|(_, cell)| cell.is_some())
        });
    }
}
