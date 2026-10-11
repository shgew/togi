use std::hash::{Hash, Hasher};

use togi_machine::{CoreId, CoreSet, PerCore, TargetOffset};

use crate::{Depth, Topology};

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ProfileError {
    #[error("profile has no offset for core {0}")]
    Missing(CoreId),
    #[error("profile names core {0} outside the topology")]
    Extra(CoreId),
}

/// An offset for every core of a topology. A core the profile does not hold reads as zero, and
/// `with` leaves the profile unchanged for it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Profile(PerCore<TargetOffset>);

impl Profile {
    pub fn new(topology: &Topology, offsets: PerCore<TargetOffset>) -> Result<Self, ProfileError> {
        if let Some(core) = topology
            .all()
            .iter()
            .find(|&core| offsets.get(core).is_none())
        {
            return Err(ProfileError::Missing(core));
        }
        if let Some((core, _)) = offsets.iter().find(|&(core, _)| !topology.contains(core)) {
            return Err(ProfileError::Extra(core));
        }
        Ok(Self(offsets))
    }

    pub fn zero(topology: &Topology) -> Self {
        Self::over(topology.all(), |_| TargetOffset::ZERO)
    }

    /// Zero everywhere but `offset` at `core`.
    pub fn alone(topology: &Topology, core: CoreId, offset: TargetOffset) -> Self {
        Self::zero(topology).with(core, offset)
    }

    pub(crate) fn over(cores: CoreSet, mut offset: impl FnMut(CoreId) -> TargetOffset) -> Self {
        Self(cores.iter().map(|core| (core, offset(core))).collect())
    }

    pub fn get(&self, core: CoreId) -> TargetOffset {
        self.0.get(core).unwrap_or(TargetOffset::ZERO)
    }

    #[must_use]
    pub fn with(&self, core: CoreId, offset: TargetOffset) -> Self {
        let mut next = *self;
        if self.0.get(core).is_some() {
            next.0.set(core, offset);
        }
        next
    }

    pub fn iter(&self) -> impl Iterator<Item = (CoreId, TargetOffset)> + '_ {
        self.0.iter()
    }

    pub fn cores(&self) -> CoreSet {
        self.iter().map(|(core, _)| core).collect()
    }

    pub fn is_zero(&self) -> bool {
        self.iter().all(|(_, offset)| offset == TargetOffset::ZERO)
    }

    /// The only core with a nonzero offset, if exactly one has.
    pub fn sole_nonzero(&self) -> Option<CoreId> {
        let mut nonzero = self
            .iter()
            .filter(|&(_, offset)| offset != TargetOffset::ZERO);
        let (core, _) = nonzero.next()?;
        nonzero.next().is_none().then_some(core)
    }

    /// Whether every core here is at least as deep as in `other`: a pass here supports `other`.
    pub fn covers(&self, other: &Self) -> bool {
        self.iter()
            .all(|(core, offset)| !offset.shallower_than(other.get(core)))
    }

    /// Whether every core here is at most as deep as in `other`: a failure here rules `other` out.
    pub fn within(&self, other: &Self) -> bool {
        self.iter()
            .all(|(core, offset)| !offset.deeper_than(other.get(core)))
    }

    /// The sum of the offsets; the more negative, the deeper.
    pub fn total(&self) -> i32 {
        self.iter().map(|(_, offset)| offset.signed()).sum()
    }
}

impl Hash for Profile {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for (core, offset) in self.iter() {
            core.hash(state);
            offset.hash(state);
        }
    }
}
