use std::collections::BTreeMap;

use togi_machine::{CcdId, CoreId, CoreInfo, CoreSet, PerCore, order};

const MAX_CCDS: usize = 3;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TopologyError {
    #[error("topology has no cores")]
    Empty,
    #[error("core {0} listed twice")]
    DuplicateCore(CoreId),
    #[error("{0} CCDs; at most 3 are supported")]
    TooManyCcds(usize),
}

/// The cores of a machine, their CCDs and the order trials walk them in.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Topology {
    order: Vec<CoreId>,
    ccd: PerCore<Option<CcdId>>,
    ccds: Vec<(CcdId, CoreSet)>,
    all: CoreSet,
    parts: Vec<CoreSet>,
}

impl Topology {
    pub fn new(cores: &[CoreInfo]) -> Result<Self, TopologyError> {
        if cores.is_empty() {
            return Err(TopologyError::Empty);
        }
        let mut all = CoreSet::default();
        let mut ccd = PerCore::default();
        let mut by_ccd: BTreeMap<CcdId, CoreSet> = BTreeMap::new();
        for info in cores {
            if !all.insert(info.core) {
                return Err(TopologyError::DuplicateCore(info.core));
            }
            ccd.set(info.core, Some(info.ccd));
            by_ccd.entry(info.ccd).or_default().insert(info.core);
        }
        if by_ccd.len() > MAX_CCDS {
            return Err(TopologyError::TooManyCcds(by_ccd.len()));
        }
        let ccds: Vec<(CcdId, CoreSet)> = by_ccd.into_iter().collect();
        let mut parts: Vec<CoreSet> = ccds.iter().map(|&(_, set)| set).collect();
        if parts.len() > 1 {
            parts.push(all);
        }
        Ok(Self {
            order: order(cores),
            ccd,
            ccds,
            all,
            parts,
        })
    }

    /// Cores alternating between CCDs: index 0 of each CCD in ascending CCD order, then index 1,
    /// and so on.
    pub fn order(&self) -> &[CoreId] {
        &self.order
    }

    pub fn ccd_of(&self, core: CoreId) -> Option<CcdId> {
        self.ccd.get(core).flatten()
    }

    /// The CCDs in ascending order with their cores.
    pub fn ccds(&self) -> &[(CcdId, CoreSet)] {
        &self.ccds
    }

    pub fn all(&self) -> CoreSet {
        self.all
    }

    /// Every full CCD, then all cores when there is more than one CCD.
    pub fn parts(&self) -> &[CoreSet] {
        &self.parts
    }

    pub fn contains(&self, core: CoreId) -> bool {
        self.all.contains(core)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RankingError {
    #[error("ranking names core {0} outside the topology")]
    Unknown(CoreId),
    #[error("ranking names core {0} twice")]
    Duplicate(CoreId),
    #[error("ranking leaves out core {0}")]
    Missing(CoreId),
}

/// Every core of a topology exactly once, the best-ranked first.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Ranking(Vec<CoreId>);

impl Ranking {
    pub fn new(topology: &Topology, cores: Vec<CoreId>) -> Result<Self, RankingError> {
        let mut seen = CoreSet::default();
        for &core in &cores {
            if !topology.contains(core) {
                return Err(RankingError::Unknown(core));
            }
            if !seen.insert(core) {
                return Err(RankingError::Duplicate(core));
            }
        }
        if let Some(missing) = topology.all().iter().find(|&core| !seen.contains(core)) {
            return Err(RankingError::Missing(missing));
        }
        Ok(Self(cores))
    }

    /// Core ids ascending: the order when no ranking is known.
    pub fn ascending(topology: &Topology) -> Self {
        Self(topology.all().iter().collect())
    }

    pub fn order(&self) -> &[CoreId] {
        &self.0
    }
}
