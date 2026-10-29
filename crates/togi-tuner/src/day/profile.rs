use crate::{UnitProfile, UnitProfileError};
use togi_machine::{CcdProfile, CcdProfileError, CoreInfo, ObservedOffset, PerCore};

/// Why a set of per-core offsets is not a day profile.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CoreProfileError {
    #[error("a core profile needs at least one core")]
    Empty,
    #[error("expanding a CCD profile onto the topology")]
    Ccd(#[from] CcdProfileError),
    #[error("expanding a unit profile onto the topology")]
    Unit(#[from] UnitProfileError),
}

/// The per-core offsets a daily event or a veto names: what the boot actually ran at, nonuniform or
/// not. A nonuniform historical profile is compared on all its physical cores, never averaged into a
/// fictitious uniform CCD profile (`docs/spec/tuner.md`, Whole-profile veto and escalation).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CoreProfile(PerCore<ObservedOffset>);

impl CoreProfile {
    pub fn new(offsets: PerCore<ObservedOffset>) -> Result<Self, CoreProfileError> {
        if offsets.is_empty() {
            Err(CoreProfileError::Empty)
        } else {
            Ok(Self(offsets))
        }
    }

    /// Every core of the topology at its CCD's offset.
    pub fn of_ccd(profile: &CcdProfile, topology: &[CoreInfo]) -> Result<Self, CoreProfileError> {
        let mut observed = PerCore::default();
        for (core, offset) in profile.expand(topology)?.iter() {
            observed.set(core, offset.observed());
        }
        Self::new(observed)
    }

    /// Expands the exact tuning groups onto their physical member cores.
    pub fn of_units(profile: &UnitProfile, cores: &[CoreInfo]) -> Result<Self, CoreProfileError> {
        Self::new(
            profile
                .expand_topology(cores)?
                .iter()
                .map(|(core, offset)| (core, offset.observed()))
                .collect(),
        )
    }

    pub fn offsets(&self) -> &PerCore<ObservedOffset> {
        &self.0
    }

    /// CO 0 on every core: an event at it is platform evidence (`docs/spec/tuner.md`,
    /// Classification and pending questions).
    pub fn is_zero(&self) -> bool {
        self.0
            .iter()
            .all(|(_, offset)| offset == ObservedOffset::default())
    }

    /// Whether a veto at `self` forbids `candidate`: every core of the vetoed profile is at or deeper
    /// (numerically at or below) in the candidate. A candidate lacking a core of the veto is not
    /// comparable and not forbidden, like [`CcdProfile::at_or_deeper_than`] over different CCDs.
    pub fn forbids(&self, candidate: &Self) -> bool {
        self.0
            .iter()
            .all(|(core, limit)| candidate.0.get(core).is_some_and(|offset| offset <= limit))
    }
}
