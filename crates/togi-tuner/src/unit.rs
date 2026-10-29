//! Ordered tuning identities, topology membership and complete unit profiles.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use togi_machine::{CcdProfile, CoreId, CoreInfo, CoreSet, PerCore, TargetOffset};
#[cfg(test)]
use togi_machine::CcdId;

use crate::Topology;

/// Every CCD precedes every core; identities within a kind ascend numerically.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub enum UnitId {
    Ccd(CcdId),
    Core(CoreId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Phase {
    Coarse,
    Core,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TuningUnit {
    pub id: UnitId,
    pub cores: CoreSet,
    pub parent: Option<UnitId>,
}

/// A phase's disjoint tuning groups in ascending identity order.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Units {
    phase: Phase,
    units: Vec<TuningUnit>,
}

impl UnitId {
    pub const fn ccd(self) -> Option<CcdId> {
        match self {
            Self::Ccd(ccd) => Some(ccd),
            Self::Core(_) => None,
        }
    }
}

impl std::fmt::Display for UnitId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ccd(ccd) => ccd.fmt(f),
            Self::Core(core) => write!(f, "core {core}"),
        }
    }
}

impl Units {
    pub fn coarse(topology: &Topology) -> Self {
        Self {
            phase: Phase::Coarse,
            units: topology
                .ccds()
                .iter()
                .map(|&(ccd, cores)| TuningUnit {
                    id: UnitId::Ccd(ccd),
                    cores,
                    parent: None,
                })
                .collect(),
        }
    }

    pub fn for_profile(topology: &Topology, profile: &UnitProfile) -> Self {
        if profile.units().any(|unit| matches!(unit, UnitId::Core(_))) {
            Self::cores(topology)
        } else {
            Self::coarse(topology)
        }
    }

    pub fn cores(topology: &Topology) -> Self {
        Self {
            phase: Phase::Core,
            units: topology
                .all()
                .iter()
                .map(|core| TuningUnit {
                    id: UnitId::Core(core),
                    cores: [core].into_iter().collect(),
                    parent: topology.ccd_of(core).map(UnitId::Ccd),
                })
                .collect(),
        }
    }

    pub const fn phase(&self) -> Phase {
        self.phase
    }

    pub fn iter(&self) -> impl Iterator<Item = &TuningUnit> {
        self.units.iter()
    }

    pub fn get(&self, id: UnitId) -> Option<&TuningUnit> {
        self.units
            .binary_search_by_key(&id, |unit| unit.id)
            .ok()
            .map(|index| &self.units[index])
    }

    pub fn unit_of(&self, core: CoreId) -> Option<UnitId> {
        self.units
            .iter()
            .find(|unit| unit.cores.contains(core))
            .map(|unit| unit.id)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum UnitProfileError {
    #[error("a unit profile needs at least one unit")]
    Empty,
    #[error("unit {0:?} is missing from the profile")]
    Missing(UnitId),
    #[error("unit {0:?} is not part of the unit table")]
    Extra(UnitId),
}

/// One offset per tuning group; expansion assigns it to every member core.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(
    try_from = "Vec<(UnitId, TargetOffset)>",
    into = "Vec<(UnitId, TargetOffset)>"
)]
pub struct UnitProfile(BTreeMap<UnitId, TargetOffset>);

impl UnitProfile {
    pub fn zero(units: &Units) -> Self {
        Self(
            units
                .iter()
                .map(|unit| (unit.id, TargetOffset::ZERO))
                .collect(),
        )
    }

    pub fn units(&self) -> impl Iterator<Item = UnitId> + '_ {
        self.0.keys().copied()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn uniform(
        units: impl IntoIterator<Item = UnitId>,
        offset: TargetOffset,
    ) -> Result<Self, UnitProfileError> {
        Self::try_from(
            units
                .into_iter()
                .map(|unit| (unit, offset))
                .collect::<Vec<_>>(),
        )
    }

    pub fn get(&self, unit: UnitId) -> Option<TargetOffset> {
        self.0.get(&unit).copied()
    }

    /// Changes an existing unit, retaining the profile's membership.
    #[must_use]
    pub fn with(&self, unit: UnitId, offset: TargetOffset) -> Self {
        let mut next = self.clone();
        if let Some(slot) = next.0.get_mut(&unit) {
            *slot = offset;
        }
        next
    }

    pub fn iter(&self) -> impl Iterator<Item = (UnitId, TargetOffset)> + '_ {
        self.0.iter().map(|(&unit, &offset)| (unit, offset))
    }

    pub fn is_zero(&self) -> bool {
        self.0.values().all(|&offset| offset == TargetOffset::ZERO)
    }

    pub fn at_or_deeper_than(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && other
                .iter()
                .all(|(unit, offset)| self.get(unit).is_some_and(|own| own <= offset))
    }

    pub fn expand(&self, units: &Units) -> Result<PerCore<TargetOffset>, UnitProfileError> {
        let mut cores = PerCore::default();
        for unit in units.iter() {
            let offset = self
                .get(unit.id)
                .ok_or(UnitProfileError::Missing(unit.id))?;
            for core in unit.cores {
                cores.set(core, offset);
            }
        }
        if let Some(extra) = self.0.keys().find(|&&id| units.get(id).is_none()) {
            return Err(UnitProfileError::Extra(*extra));
        }
        Ok(cores)
    }

    /// Expands at adapters that receive a physical core listing rather than a unit table.
    pub fn expand_topology(
        &self,
        topology: &[CoreInfo],
    ) -> Result<PerCore<TargetOffset>, UnitProfileError> {
        let core_phase = self.units().any(|unit| matches!(unit, UnitId::Core(_)));
        let id_of = |info: &CoreInfo| {
            if core_phase {
                UnitId::Core(info.core)
            } else {
                UnitId::Ccd(info.ccd)
            }
        };
        let mut cores = PerCore::default();
        for info in topology {
            let unit = id_of(info);
            cores.set(
                info.core,
                self.get(unit).ok_or(UnitProfileError::Missing(unit))?,
            );
        }
        if let Some(extra) = self
            .units()
            .find(|&unit| !topology.iter().any(|info| id_of(info) == unit))
        {
            return Err(UnitProfileError::Extra(extra));
        }
        Ok(cores)
    }

    pub fn of_ccd(profile: &CcdProfile) -> Self {
        Self(
            profile
                .iter()
                .map(|(ccd, offset)| (UnitId::Ccd(ccd), offset))
                .collect(),
        )
    }

    pub fn matches_ccd(&self, profile: &CcdProfile) -> bool {
        self.iter().eq(profile
            .iter()
            .map(|(ccd, offset)| (UnitId::Ccd(ccd), offset)))
    }

    pub fn to_ccd(&self) -> Option<CcdProfile> {
        self.iter()
            .map(|(unit, offset)| match unit {
                UnitId::Ccd(ccd) => Some((ccd, offset)),
                UnitId::Core(_) => None,
            })
            .collect::<Option<BTreeMap<_, _>>>()
            .and_then(|ccds| CcdProfile::try_from(ccds).ok())
    }

    pub fn of_cores(cores: &PerCore<TargetOffset>) -> Self {
        Self(
            cores
                .iter()
                .map(|(core, offset)| (UnitId::Core(core), offset))
                .collect(),
        )
    }
}

impl TryFrom<BTreeMap<UnitId, TargetOffset>> for UnitProfile {
    type Error = UnitProfileError;

    fn try_from(entries: BTreeMap<UnitId, TargetOffset>) -> Result<Self, Self::Error> {
        if entries.is_empty() {
            Err(UnitProfileError::Empty)
        } else {
            Ok(Self(entries))
        }
    }
}

impl TryFrom<Vec<(UnitId, TargetOffset)>> for UnitProfile {
    type Error = UnitProfileError;

    fn try_from(entries: Vec<(UnitId, TargetOffset)>) -> Result<Self, Self::Error> {
        if entries.is_empty() {
            Err(UnitProfileError::Empty)
        } else {
            Ok(Self(entries.into_iter().collect()))
        }
    }
}

impl From<UnitProfile> for Vec<(UnitId, TargetOffset)> {
    fn from(profile: UnitProfile) -> Self {
        profile.0.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use togi_machine::CoreInfo;

    fn topology() -> Topology {
        Topology::new(
            &(0..16)
                .rev()
                .map(|id| CoreInfo {
                    core: CoreId::new(id).unwrap(),
                    ccd: CcdId::new(id / 8).unwrap(),
                    cpus: Vec::new(),
                })
                .collect::<Vec<_>>(),
        )
        .unwrap()
    }

    #[test]
    fn coarse_units_follow_ascending_ccd_order() {
        let units = Units::coarse(&topology());
        assert_eq!(units.phase(), Phase::Coarse);
        assert_eq!(
            units.iter().map(|unit| unit.id).collect::<Vec<_>>(),
            vec![
                UnitId::Ccd(CcdId::new(0).unwrap()),
                UnitId::Ccd(CcdId::new(1).unwrap())
            ]
        );
        assert!(units.iter().all(|unit| unit.parent.is_none()));
    }

    #[test]
    fn a_unit_profile_round_trips_through_its_ccd_profile() {
        let ccd = CcdProfile::zero(topology().ccds().iter().map(|&(ccd, _)| ccd))
            .unwrap()
            .with(CcdId::new(1).unwrap(), TargetOffset::new(-17).unwrap());
        assert_eq!(UnitProfile::of_ccd(&ccd).to_ccd(), Some(ccd));
    }

    #[test]
    fn a_unit_profile_expands_to_every_member_core() {
        let topology = topology();
        let units = Units::coarse(&topology);
        let profile = UnitProfile::zero(&units).with(
            UnitId::Ccd(CcdId::new(1).unwrap()),
            TargetOffset::new(-17).unwrap(),
        );
        let expanded = profile.expand(&units).unwrap();
        assert_eq!(expanded.len(), 16);
        for core in topology.all() {
            assert_eq!(
                expanded.get(core),
                Some(TargetOffset::new(if core.get() < 8 { 0 } else { -17 }).unwrap())
            );
            assert_eq!(units.unit_of(core), topology.ccd_of(core).map(UnitId::Ccd));
        }
    }

    #[test]
    fn sixteen_core_units_expand_to_a_nonuniform_vector() {
        let units = Units::cores(&topology());
        let cores: PerCore<_> = (0..16)
            .map(|id| (CoreId::new(id).unwrap(), TargetOffset::new(-id).unwrap()))
            .collect();
        let profile = UnitProfile::of_cores(&cores);
        assert_eq!(units.phase(), Phase::Core);
        assert_eq!(profile.expand(&units).unwrap(), cores);
        assert_eq!(profile.to_ccd(), None);
        for unit in units.iter() {
            assert_eq!(unit.cores.len(), 1);
            assert!(unit.parent.is_some());
        }
    }
}
