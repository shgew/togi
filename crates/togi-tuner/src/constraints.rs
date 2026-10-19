use std::fmt;
use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};
use togi_machine::{CoreId, CoreSet, PerCore, TargetOffset};

use crate::optimum::solve;
use crate::{Depth, Profile, Ranking, Topology, TopologyMismatch};

/// The shallowest offset a core failed at; reaching it or going deeper fails again.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FailurePoint(TargetOffset);

impl FailurePoint {
    pub const fn new(offset: TargetOffset) -> Self {
        Self(offset)
    }

    pub const fn offset(self) -> TargetOffset {
        self.0
    }

    /// Merges a newly observed failure with the recorded point; the shallower point wins.
    #[must_use]
    pub fn merge(self, prior: Option<Self>) -> Self {
        prior
            .filter(|old| old.offset().shallower_than(self.offset()))
            .unwrap_or(self)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CombinationId(NonZeroU32);

impl CombinationId {
    pub const fn new(id: NonZeroU32) -> Self {
        Self(id)
    }

    pub const fn get(self) -> u32 {
        self.0.get()
    }

    pub const fn non_zero(self) -> NonZeroU32 {
        self.0
    }
}

impl fmt::Display for CombinationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "C{}", self.0)
    }
}

/// Offsets at which the member cores fail together though each passes alone.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Combination {
    id: CombinationId,
    members: PerCore<TargetOffset>,
}

impl Combination {
    pub const fn id(&self) -> CombinationId {
        self.id
    }

    pub const fn members(&self) -> &PerCore<TargetOffset> {
        &self.members
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Reached {
    FailurePoint { core: CoreId, point: FailurePoint },
    Combination(CombinationId),
}

impl fmt::Display for Reached {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FailurePoint { core, point } => write!(
                f,
                "failure point {} of core {:02}",
                point.offset().get(),
                core.get()
            ),
            Self::Combination(id) => write!(f, "combination {id}"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Limit {
    Floor,
    Reached(Reached),
}

impl fmt::Display for Limit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Floor => f.write_str("at the floor -50"),
            Self::Reached(reached) => reached.fmt(f),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ConstraintError {
    #[error("core {0} is outside the topology")]
    UnknownCore(CoreId),
    #[error("a combination needs at least one member")]
    EmptyCombination,
    #[error("combination {0} already exists")]
    DuplicateId(CombinationId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Insertion {
    Added,
    Duplicate(CombinationId),
}

/// The failure points and combinations recorded for a topology's cores. They accumulate until a
/// core is cleared.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Constraints {
    cores: CoreSet,
    failure_points: PerCore<FailurePoint>,
    combinations: Vec<Combination>,
    by_core: PerCore<Option<Vec<usize>>>,
    recent: CoreSet,
}

impl Constraints {
    pub fn new(topology: &Topology) -> Self {
        Self {
            cores: topology.all(),
            failure_points: PerCore::default(),
            combinations: Vec::new(),
            by_core: Default::default(),
            recent: CoreSet::default(),
        }
    }

    /// The core whose latest update made its failure point shallower, or the members of the
    /// latest combination.
    pub const fn recent(&self) -> CoreSet {
        self.recent
    }

    pub const fn failure_points(&self) -> &PerCore<FailurePoint> {
        &self.failure_points
    }

    /// The combinations in insertion order.
    pub fn combinations(&self) -> &[Combination] {
        &self.combinations
    }

    /// The combinations `core` is a member of, in insertion order.
    pub fn combinations_with(&self, core: CoreId) -> impl Iterator<Item = &Combination> {
        self.by_core
            .get_ref(core)
            .and_then(Option::as_ref)
            .into_iter()
            .flatten()
            .filter_map(|&index| self.combinations.get(index))
    }

    /// Keeps the shallowest of the recorded and the new failure point.
    pub fn insert_failure_point(
        &mut self,
        core: CoreId,
        point: FailurePoint,
    ) -> Result<(), ConstraintError> {
        if !self.cores.contains(core) {
            return Err(ConstraintError::UnknownCore(core));
        }
        let prior = self.failure_points.get(core);
        let point = point.merge(prior);
        if prior != Some(point) {
            self.set_point(core, Some(point));
        }
        Ok(())
    }

    /// Replaces the failure point of `core`, which a `core.phase` or `tuner.decision` records
    /// whole; `None` clears it.
    pub fn set_failure_point(
        &mut self,
        core: CoreId,
        point: Option<FailurePoint>,
    ) -> Result<(), ConstraintError> {
        if !self.cores.contains(core) {
            return Err(ConstraintError::UnknownCore(core));
        }
        self.set_point(core, point);
        Ok(())
    }

    fn set_point(&mut self, core: CoreId, point: Option<FailurePoint>) {
        let old = self.failure_points.get(core);
        if let Some(point) = point {
            let shallower = old != Some(point) && point.merge(old) == point;
            if shallower {
                self.recent = CoreSet::default();
                self.recent.insert(core);
            }
        }
        self.failure_points = self
            .failure_points
            .iter()
            .filter(|&(owner, _)| owner != core)
            .chain(point.map(|point| (core, point)))
            .collect();
    }

    /// Adds a combination unless one with the same members exists, which keeps its place and id.
    pub fn insert_combination(
        &mut self,
        id: CombinationId,
        members: PerCore<TargetOffset>,
    ) -> Result<Insertion, ConstraintError> {
        let Some((first, _)) = members.iter().next() else {
            return Err(ConstraintError::EmptyCombination);
        };
        if let Some((core, _)) = members.iter().find(|&(core, _)| !self.cores.contains(core)) {
            return Err(ConstraintError::UnknownCore(core));
        }
        let same = self
            .combinations_with(first)
            .find(|combination| combination.members == members)
            .map(Combination::id);
        if let Some(same) = same {
            self.recent = members.iter().map(|(core, _)| core).collect();
            return Ok(Insertion::Duplicate(same));
        }
        if self
            .combinations
            .iter()
            .any(|combination| combination.id == id)
        {
            return Err(ConstraintError::DuplicateId(id));
        }
        self.recent = members.iter().map(|(core, _)| core).collect();
        let index = self.combinations.len();
        for (core, _) in members.iter() {
            self.by_core.get_or_insert_with(core, Vec::new).push(index);
        }
        self.combinations.push(Combination { id, members });
        Ok(Insertion::Added)
    }

    /// Drops the failure point of `core` and every combination it belongs to, returning the ids of
    /// the combinations dropped in insertion order.
    pub fn clear_core(&mut self, core: CoreId) -> Vec<CombinationId> {
        self.failure_points = self
            .failure_points
            .iter()
            .filter(|&(owner, _)| owner != core)
            .collect();
        let cleared: Vec<CombinationId> =
            self.combinations_with(core).map(Combination::id).collect();
        self.remove_combinations(&cleared);
        cleared
    }

    /// Drops the combinations with these ids; ids that name none are ignored.
    pub fn remove_combinations(&mut self, ids: &[CombinationId]) {
        if !self
            .combinations
            .iter()
            .any(|combination| ids.contains(&combination.id))
        {
            return;
        }
        self.combinations
            .retain(|combination| !ids.contains(&combination.id));
        self.by_core = Default::default();
        for (index, combination) in self.combinations.iter().enumerate() {
            for (member, _) in combination.members.iter() {
                self.by_core
                    .get_or_insert_with(member, Vec::new)
                    .push(index);
            }
        }
    }

    /// The first failure point `profile` reaches in core order, else the first combination in
    /// insertion order. A failure point is reached when the core is at or deeper than it; a
    /// combination when every member is at or deeper than its offset. A profile of other cores is
    /// an error, never read as zero.
    pub fn reaches(&self, profile: &Profile) -> Result<Option<Reached>, TopologyMismatch> {
        TopologyMismatch::check(self.cores, profile.cores())?;
        let reached = |core: CoreId, limit: TargetOffset| {
            profile
                .get(core)
                .is_none_or(|offset| !offset.shallower_than(limit))
        };
        for (core, point) in self.failure_points.iter() {
            if reached(core, point.offset()) {
                return Ok(Some(Reached::FailurePoint { core, point }));
            }
        }
        Ok(self
            .combinations
            .iter()
            .find(|combination| {
                combination
                    .members
                    .iter()
                    .all(|(member, offset)| reached(member, offset))
            })
            .map(|combination| Reached::Combination(combination.id)))
    }

    /// Whether `core` cannot go one count deeper from `profile`: it is at the floor, or one count
    /// deeper reaches a failure point or combination.
    pub fn at_limit(
        &self,
        core: CoreId,
        profile: &Profile,
    ) -> Result<Option<Limit>, TopologyMismatch> {
        TopologyMismatch::check(self.cores, profile.cores())?;
        let Some(deeper) = profile.get(core).and_then(|offset| offset.deeper_by(1)) else {
            return Ok(Some(Limit::Floor));
        };
        Ok(self
            .reaches(&profile.with(core, deeper))?
            .map(Limit::Reached))
    }

    /// The profile of greatest total depth between each core's floor (just shallower than its
    /// failure point) and `hi` that reaches no failure point or combination, or `None` when there
    /// is none. Among equal totals, the one deeper at the first core of `ranking` where they
    /// differ. `hi` and `ranking` must be of these cores.
    pub fn optimum(
        &self,
        hi: &Profile,
        ranking: &Ranking,
    ) -> Result<Option<Profile>, TopologyMismatch> {
        TopologyMismatch::check(self.cores, hi.cores())?;
        TopologyMismatch::check(self.cores, ranking.order().iter().copied().collect())?;
        Ok(solve(
            self.cores,
            &self.failure_points,
            &self.combinations,
            hi,
            ranking,
        ))
    }
}
