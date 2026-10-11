use std::cmp::Ordering;
use std::num::NonZeroU32;

use togi_machine::{Backend, CoreSet, REGIMES, Regime, Workload, workloads};

/// A workload of the catalog, named by its regime and its place in that regime's list; it orders
/// the way the catalog lists workloads.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WorkloadId {
    regime: Regime,
    index: u8,
}

impl WorkloadId {
    pub fn new(regime: Regime, index: usize) -> Option<Self> {
        if index >= workloads(regime).len() {
            return None;
        }
        let index = u8::try_from(index).ok()?;
        Some(Self { regime, index })
    }

    pub fn by_id(id: &str) -> Option<Self> {
        REGIMES.into_iter().find_map(|regime| {
            let index = workloads(regime)
                .iter()
                .position(|workload| workload.id == id)?;
            Self::new(regime, index)
        })
    }

    pub const fn regime(self) -> Regime {
        self.regime
    }

    pub fn workload(self) -> &'static Workload {
        &workloads(self.regime)[usize::from(self.index)]
    }

    pub fn backend(self) -> Backend {
        self.workload().backend
    }
}

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
        self.workload.regime
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
        matches!(self.workload.regime, Regime::R7) && self.loaded.len() > 1
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
