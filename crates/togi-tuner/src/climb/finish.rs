use std::collections::BTreeMap;

use jiff::Timestamp;
use togi_machine::TargetOffset;

use crate::day::EventId;
use crate::unit::{UnitId, UnitProfile, Units};

use super::{ApplicationId, Blocker, Ceiling, Ceilings};

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum StopReason {
    FirstBackstep {
        event: EventId,
        application: Option<ApplicationId>,
    },
    AdverseCeiling,
    Veto,
    Predecessor { original: togi_journal::Provenance },
    RestorationBackstep {
        event: EventId,
        application: ApplicationId,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FinishReason {
    Ceiling,
    Stopped(StopReason),
    AllowanceExhausted,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FinishWitness {
    pub reason: FinishReason,
    pub at: Timestamp,
    pub profile: UnitProfile,
    pub last_clean: TargetOffset,
    pub ceiling: Option<Ceiling>,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum UnitState {
    Climbing,
    Held { reason: Blocker },
    Finished(FinishWitness),
}

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FinishBook(#[serde(with = "crate::policy::wire::pairs")] BTreeMap<UnitId, FinishWitness>);

impl FinishBook {
    pub fn get(&self, unit: UnitId) -> Option<&FinishWitness> {
        self.0.get(&unit)
    }
    pub fn is_finished(&self, unit: UnitId) -> bool {
        self.0.contains_key(&unit)
    }

    /// A transient blocker never replaces a recorded finish or creates a successful result.
    pub fn state(&self, unit: UnitId, held: Option<Blocker>) -> UnitState {
        if let Some(witness) = self.get(unit) {
            UnitState::Finished(witness.clone())
        } else if let Some(reason) = held {
            UnitState::Held { reason }
        } else {
            UnitState::Climbing
        }
    }

    pub(crate) fn stop(&mut self, unit: UnitId, witness: FinishWitness) {
        // Adverse evidence may replace a ceiling finish, never an earlier permanent stop.
        if self
            .get(unit)
            .is_some_and(|old| matches!(old.reason, FinishReason::Stopped(_)))
        {
            return;
        }
        self.0.insert(unit, witness);
    }

    pub fn observe(
        &mut self,
        at: Timestamp,
        profile: &UnitProfile,
        ceilings: &Ceilings,
        exhausted: bool,
    ) {
        for (unit, depth) in profile.iter() {
            if self.is_finished(unit) {
                continue;
            }
            let reason = if exhausted {
                Some(FinishReason::AllowanceExhausted)
            } else if ceilings.get(&unit).and_then(|ceiling| ceiling.c) == Some(depth) {
                Some(FinishReason::Ceiling)
            } else {
                None
            };
            if let Some(reason) = reason {
                self.0.insert(
                    unit,
                    FinishWitness {
                        reason,
                        at,
                        profile: profile.clone(),
                        last_clean: depth,
                        ceiling: ceilings.get(&unit).cloned(),
                    },
                );
            }
        }
    }

    pub(super) fn inherit(&self, units: &Units) -> Self {
        Self(
            units
                .iter()
                .filter_map(|unit| {
                    let witness = unit.parent.and_then(|parent| self.get(parent))?;
                    // Ceiling completion belongs to the coarse climb; only adverse stops prohibit refinement.
                    matches!(
                        witness.reason,
                        FinishReason::Stopped(_) | FinishReason::AllowanceExhausted
                    )
                    .then(|| (unit.id, witness.clone()))
                })
                .collect(),
        )
    }
}
