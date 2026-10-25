use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{CcdId, CoreInfo, PerCore, TargetOffset};

/// Why a set of CCD offsets is not a profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CcdProfileError {
    #[error("a CCD profile needs at least one CCD")]
    Empty,
    #[error("CCD {0} is missing from the profile")]
    Missing(CcdId),
    #[error("CCD {0} is not part of the topology")]
    Extra(CcdId),
}

/// One Curve Optimizer offset per CCD, every core of a CCD sharing it: the profile Ruleset 13 tests
/// and applies (`docs/spec/tuner.md`, Invariants). Ordered by ascending CCD; it serializes as an
/// object keyed by CCD id.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(
    try_from = "BTreeMap<CcdId, TargetOffset>",
    into = "BTreeMap<CcdId, TargetOffset>"
)]
pub struct CcdProfile(BTreeMap<CcdId, TargetOffset>);

impl CcdProfile {
    /// Every given CCD at `offset`.
    pub fn uniform(
        ccds: impl IntoIterator<Item = CcdId>,
        offset: TargetOffset,
    ) -> Result<Self, CcdProfileError> {
        Self::try_from(
            ccds.into_iter()
                .map(|ccd| (ccd, offset))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    /// Every given CCD at CO 0.
    pub fn zero(ccds: impl IntoIterator<Item = CcdId>) -> Result<Self, CcdProfileError> {
        Self::uniform(ccds, TargetOffset::ZERO)
    }

    /// The CCD profile of a per-core vector, or `None` when a CCD's cores differ or a core of
    /// the topology has no offset: such a vector is an observation, not a Ruleset 13 profile.
    pub fn of_cores(cores: &PerCore<TargetOffset>, topology: &[CoreInfo]) -> Option<Self> {
        let mut ccds = BTreeMap::new();
        for info in topology {
            let offset = cores.get(info.core)?;
            match ccds.insert(info.ccd, offset) {
                Some(other) if other != offset => return None,
                _ => {}
            }
        }
        Self::try_from(ccds).ok()
    }

    pub fn get(&self, ccd: CcdId) -> Option<TargetOffset> {
        self.0.get(&ccd).copied()
    }

    /// The CCDs in ascending order with their offsets.
    pub fn iter(&self) -> impl Iterator<Item = (CcdId, TargetOffset)> + '_ {
        self.0.iter().map(|(&ccd, &offset)| (ccd, offset))
    }

    pub fn ccds(&self) -> impl Iterator<Item = CcdId> + '_ {
        self.0.keys().copied()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Never true: a profile holds at least one CCD.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// This profile with `ccd` at `offset`; unchanged when it does not hold `ccd`.
    #[must_use]
    pub fn with(&self, ccd: CcdId, offset: TargetOffset) -> Self {
        let mut next = self.clone();
        if let Some(slot) = next.0.get_mut(&ccd) {
            *slot = offset;
        }
        next
    }

    pub fn is_zero(&self) -> bool {
        self.0.values().all(|&offset| offset == TargetOffset::ZERO)
    }

    /// Whether every CCD of `other` is at or deeper (numerically at or below) in this profile:
    /// the comparison a whole-profile veto forbids (`docs/spec/tuner.md`, Whole-profile veto).
    /// Profiles over different CCDs never compare.
    pub fn at_or_deeper_than(&self, other: &Self) -> bool {
        self.0.len() == other.0.len()
            && other
                .iter()
                .all(|(ccd, offset)| self.get(ccd).is_some_and(|own| own <= offset))
    }

    /// Every core of the topology at its CCD's offset.
    pub fn expand(&self, topology: &[CoreInfo]) -> Result<PerCore<TargetOffset>, CcdProfileError> {
        let mut cores = PerCore::default();
        for info in topology {
            let offset = self
                .get(info.ccd)
                .ok_or(CcdProfileError::Missing(info.ccd))?;
            cores.set(info.core, offset);
        }
        if let Some(extra) = self
            .ccds()
            .find(|ccd| topology.iter().all(|info| info.ccd != *ccd))
        {
            return Err(CcdProfileError::Extra(extra));
        }
        Ok(cores)
    }
}

impl TryFrom<BTreeMap<CcdId, TargetOffset>> for CcdProfile {
    type Error = CcdProfileError;
    fn try_from(ccds: BTreeMap<CcdId, TargetOffset>) -> Result<Self, CcdProfileError> {
        if ccds.is_empty() {
            Err(CcdProfileError::Empty)
        } else {
            Ok(Self(ccds))
        }
    }
}

impl From<CcdProfile> for BTreeMap<CcdId, TargetOffset> {
    fn from(profile: CcdProfile) -> Self {
        profile.0
    }
}
