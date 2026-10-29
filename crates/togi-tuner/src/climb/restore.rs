use togi_machine::PerCore;

use crate::day::{Classification, CoreProfile, DailyEvent};
use crate::unit::{UnitProfile, Units};

use super::{ApplicationId, ClimbError, ClimbHistory};

/// The complete physical vector and the mapping under which it was applied.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SavedProfile {
    pub units: Units,
    pub profile: UnitProfile,
}

impl SavedProfile {
    pub fn actual(&self) -> Result<CoreProfile, ClimbError> {
        let mut observed = PerCore::default();
        for (core, offset) in self.profile.expand(&self.units)?.iter() {
            observed.set(core, offset.observed());
        }
        Ok(CoreProfile::new(observed)?)
    }

    /// Remap only when every member of a current unit has the identical saved value.
    pub fn for_units(&self, units: &Units) -> Result<UnitProfile, ClimbError> {
        let cores = self.profile.expand(&self.units)?;
        let values = units
            .iter()
            .map(|unit| {
                let mut values = unit.cores.iter().map(|core| cores.get(core));
                let first = values.next().flatten().ok_or(ClimbError::Refinement)?;
                if values.any(|value| value != Some(first)) {
                    return Err(ClimbError::Refinement);
                }
                Ok((unit.id, first))
            })
            .collect::<Result<Vec<_>, ClimbError>>()?;
        let profile = UnitProfile::try_from(values)?;
        if profile.expand(units)? != cores {
            return Err(ClimbError::Refinement);
        }
        Ok(profile)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RestorationKind {
    Firmware,
    Reclassified { retreat: ApplicationId },
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RestorationProposal {
    pub kind: RestorationKind,
    pub profile: UnitProfile,
}

pub fn candidate(
    history: &ClimbHistory,
    events: &[DailyEvent],
) -> Result<Option<RestorationProposal>, ClimbError> {
    if let Some(saved) = history.saved() {
        let profile = saved.for_units(history.units())?;
        if profile != *history.current() {
            return Ok(Some(RestorationProposal {
                kind: RestorationKind::Firmware,
                profile,
            }));
        }
    }
    for record in history.retreats().iter().rev() {
        if record.events.is_empty() || !record.restorable || history.restored(record.application) {
            continue;
        }
        if !record.events.iter().all(|id| {
            events
                .iter()
                .any(|event| event.id == *id && event.class == Classification::NonCpu)
        }) {
            continue;
        }
        let before = SavedProfile {
            units: record.units.clone(),
            profile: record.before.clone(),
        }
        .for_units(history.units())?;
        let after = SavedProfile {
            units: record.units.clone(),
            profile: record.after.clone(),
        }
        .for_units(history.units())?;
        let profile = UnitProfile::try_from(
            history
                .current()
                .iter()
                .map(|(unit, current)| {
                    let restored = before
                        .get(unit)
                        .filter(|&depth| after.get(unit).is_some_and(|after| depth < after));
                    (unit, restored.map_or(current, |depth| current.min(depth)))
                })
                .collect::<Vec<_>>(),
        )?;
        // Preserve unrelated later progress; a consumed restoration is never replayed.
        if profile == *history.current() {
            continue;
        }
        return Ok(Some(RestorationProposal {
            kind: RestorationKind::Reclassified {
                retreat: record.application,
            },
            profile,
        }));
    }
    Ok(None)
}
