use std::collections::BTreeMap;

use crate::UnitProfile;
use togi_machine::CoreInfo;

use super::error::DayStepError;
use super::events::{Certainty, Classification, DailyEvent, EventId};
use super::history::{DayHistory, RetreatOutcome};
use super::profile::CoreProfile;

/// How a night's reproduction task for an event closed. Either outcome closes only its bounded
/// task; neither clears the veto or classifies the event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReproductionOutcome {
    Reproduced,
    Unreproduced,
    /// The failed profile was nonuniform, so no night retests it.
    Untestable,
}

/// Where an event's required step back stands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RetreatState {
    /// Its retreat profile was applied.
    Applied,
    /// The applied D is no longer inside the event's veto: nothing to step back.
    NotNeeded,
    /// D is inside the event's veto and no retreat was applied.
    Required,
    /// CO 0 was applied until `wanted`, the retreat profile, qualifies.
    AwaitingQualification { wanted: UnitProfile },
}

/// A CPU-suspect event's whole-profile veto: no day profile may be at or deeper than `profile` on
/// every core.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Veto {
    pub event: EventId,
    pub certainty: Certainty,
    /// The actual failed profile, nonuniform or not.
    pub profile: CoreProfile,
}

/// The vetoes the events currently stand for, in event order. Reclassifying an event drops its
/// veto; CO 0 events, being platform evidence, never had one.
pub fn vetoes(events: &[DailyEvent]) -> Vec<Veto> {
    events
        .iter()
        .filter_map(|event| {
            Some(Veto {
                event: event.id.clone(),
                certainty: event.class.certainty()?,
                profile: event.profile,
            })
        })
        .collect()
}

/// An event with its step back and reproduction state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventStatus {
    pub id: EventId,
    pub class: Classification,
    pub retreat: RetreatState,
    pub reproduction: Option<ReproductionOutcome>,
}

impl EventStatus {
    /// Whether the event no longer blocks promotion. A CPU-suspect event, provisional or definite,
    /// is resolved only once its required retreat was applied and its reproduction task closed; a
    /// non-CPU reclassification resolves it; an unexplained event stays unresolved
    /// (`docs/spec/tuner.md`, Whole-profile veto and escalation).
    pub fn is_resolved(&self) -> bool {
        match self.class {
            Classification::CpuSuspect(_) | Classification::ProvisionalCpuSuspect(_) => {
                matches!(
                    self.retreat,
                    RetreatState::Applied | RetreatState::NotNeeded
                ) && self.reproduction.is_some()
            }
            Classification::NonCpu | Classification::Platform => true,
            Classification::PendingQuestion => false,
        }
    }
}

/// The retreat and reproduction state of every event, in event order.
pub fn event_statuses(
    topology: &[CoreInfo],
    history: &DayHistory,
    events: &[DailyEvent],
    reproductions: &BTreeMap<EventId, ReproductionOutcome>,
) -> Result<Vec<EventStatus>, DayStepError> {
    let applied = CoreProfile::of_units(&history.current().profile, topology)?;
    Ok(events
        .iter()
        .map(|event| {
            let retreat = if !event.class.is_cpu_suspect() {
                RetreatState::NotNeeded
            } else {
                match history
                    .retreats()
                    .filter(|record| record.events.contains(&event.id))
                    .last()
                {
                    Some(record) => match record.outcome {
                        RetreatOutcome::Reached => RetreatState::Applied,
                        RetreatOutcome::Fallback { wanted } => {
                            RetreatState::AwaitingQualification {
                                wanted: wanted.clone(),
                            }
                        }
                    },
                    None if event.profile.forbids(&applied) => RetreatState::Required,
                    None => RetreatState::NotNeeded,
                }
            };
            EventStatus {
                id: event.id.clone(),
                class: event.class,
                retreat,
                reproduction: reproductions.get(&event.id).copied(),
            }
        })
        .collect())
}
