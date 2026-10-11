use std::cmp::Ordering;
use std::num::NonZeroU32;

use togi_machine::{CoreSet, Regime, WorkloadId};

/// A trial's intended duration in whole seconds.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Secs(NonZeroU32);

impl Secs {
    pub const fn new(secs: NonZeroU32) -> Self {
        Self(secs)
    }

    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// What makes two trials comparable evidence: the workload with its regime, the loaded cores
/// and the intended duration.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ClassKey {
    workload: WorkloadId,
    loaded: CoreSet,
    duration: Secs,
}

impl ClassKey {
    pub const fn new(workload: WorkloadId, loaded: CoreSet, duration: Secs) -> Self {
        Self {
            workload,
            loaded,
            duration,
        }
    }

    pub const fn regime(self) -> Regime {
        self.workload.regime()
    }

    pub const fn workload(self) -> WorkloadId {
        self.workload
    }

    pub const fn loaded(self) -> CoreSet {
        self.loaded
    }

    pub const fn duration(self) -> Secs {
        self.duration
    }

    /// Whether the class loads several cores under R7, where a failure names its core.
    pub const fn multi_r7(self) -> bool {
        matches!(self.workload.regime(), Regime::R7) && self.loaded.len() > 1
    }
}

impl Ord for ClassKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.workload
            .cmp(&other.workload)
            .then_with(|| self.loaded.iter().cmp(other.loaded.iter()))
            .then(self.duration.cmp(&other.duration))
    }
}

impl PartialOrd for ClassKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
