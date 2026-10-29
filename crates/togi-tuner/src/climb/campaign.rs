use std::collections::BTreeSet;

use crate::day::{Classification, DailyEvent, EventId};
use crate::unit::Phase;

pub const LEARNING_ALLOWANCE: usize = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum EventPhase {
    Learning,
    PostFinish,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Charge {
    pub event: EventId,
    pub aliases: BTreeSet<EventId>,
    pub phase: EventPhase,
    pub unit_phase: Phase,
    pub provisional: bool,
    pub reclassified: bool,
}

/// A campaign outlives sessions, firmware and phase changes. It has no renewal operation.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Campaign {
    Unreconstructible,
    Known { charges: Vec<Charge> },
}

impl Campaign {
    /// Only an explicitly approved fresh campaign starts with an unused allowance.
    pub const fn approved_fresh() -> Self {
        Self::Known {
            charges: Vec::new(),
        }
    }

    pub fn charges(&self) -> &[Charge] {
        match self {
            Self::Known { charges } => charges,
            Self::Unreconstructible => &[],
        }
    }

    pub fn consumed(&self) -> Option<usize> {
        match self {
            Self::Known { charges } => Some(charges.len()),
            Self::Unreconstructible => None,
        }
    }

    pub fn remaining(&self) -> Option<usize> {
        self.consumed()
            .map(|count| LEARNING_ALLOWANCE.saturating_sub(count))
    }

    pub fn exhausted(&self) -> bool {
        self.remaining() == Some(0)
    }

    pub fn reclassified(&self) -> usize {
        self.charges()
            .iter()
            .filter(|charge| charge.reclassified)
            .count()
    }

    /// Merge identity aliases even after charging. Bridging aliases merge previously separate records.
    pub fn observe(
        &mut self,
        event: &DailyEvent,
        aliases: &[EventId],
        phase: EventPhase,
        unit_phase: Phase,
    ) {
        let Self::Known { charges } = self else {
            return;
        };
        let mut identities: BTreeSet<_> = aliases.iter().cloned().collect();
        identities.insert(event.id.clone());
        let mut found = None;
        let mut first = None;
        let mut index = 0;
        while index < charges.len() {
            if charges[index].aliases.is_disjoint(&identities) {
                index += 1;
            } else {
                first.get_or_insert(index);
                let charge = charges.remove(index);
                identities.extend(charge.aliases.iter().cloned());
                if found.is_none() {
                    found = Some(charge);
                }
                index = 0;
            }
        }
        if let Some(mut charge) = found {
            charge.aliases = identities;
            charge.reclassified = event.class == Classification::NonCpu;
            charges.insert(first.unwrap_or(charges.len()), charge);
        } else if event.class.is_cpu_suspect() && !event.profile.is_zero() {
            charges.push(Charge {
                event: event.id.clone(),
                aliases: identities,
                phase,
                unit_phase,
                provisional: matches!(event.class, Classification::ProvisionalCpuSuspect(_)),
                reclassified: false,
            });
        }
    }
}
