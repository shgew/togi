use crate::{UnitId, UnitProfile};
use togi_journal::Seq;

use super::events::EventId;

/// One unit's share of an applied promotion that an escalation reverted: the cursor that moves past
/// the step (`docs/spec/tuner.md`, Whole-profile veto and escalation).
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct Reverted {
    /// The applied promotion, by the journal position of its application.
    pub step: Seq,
    pub unit: UnitId,
}

/// Why a profile was applied. Only a successful readback commits an application, and every kind
/// but [`Self::Reapplication`] starts a new hold.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ApplicationKind {
    /// The verified all-zero starting baseline.
    Baseline,
    /// A day-step promotion.
    Promotion,
    /// A required step back reached its profile for these events, consuming these steps.
    Retreat {
        events: Vec<EventId>,
        reverted: Vec<Reverted>,
    },
    /// A required step back whose profile `wanted` lacked qualification: CO 0 applied until it
    /// qualifies. The steps are consumed already.
    Fallback {
        events: Vec<EventId>,
        reverted: Vec<Reverted>,
        wanted: UnitProfile,
    },
    /// A cap shallower than D forced the profile down to the cap.
    SafetyCap,
    /// A firmware change reset D to CO 0.
    FirmwareZero,
    /// The committed D applied again (a service restart); it preserves the hold.
    Reapplication,
}

/// A step-back application as the escalation chain reads it.
#[derive(Clone)]
pub(crate) struct RetreatRecord<'a> {
    pub events: &'a [EventId],
    pub outcome: RetreatOutcome<'a>,
}

#[derive(Clone, Copy)]
pub(crate) enum RetreatOutcome<'a> {
    /// The retreat profile itself was applied.
    Reached,
    /// CO 0 was applied until `wanted` qualifies.
    Fallback { wanted: &'a UnitProfile },
}

/// A successful application: the whole profile read back and why it was applied.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DayApplication {
    /// The journal position of its commit; it orders the application against events and intervals.
    pub at: Seq,
    pub profile: UnitProfile,
    pub kind: ApplicationKind,
}

/// Why an application cannot extend the history.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum HistoryError {
    #[error("application {next} does not follow application {previous}")]
    OutOfOrder { previous: Seq, next: Seq },
    #[error("application {at} names other units than the baseline")]
    Units { at: Seq },
}

/// A promotion since the latest firmware reset, with the profile it left and the one it reached.
#[derive(Clone, Copy, Debug)]
pub struct Promotion<'a> {
    pub at: Seq,
    pub from: &'a UnitProfile,
    pub to: &'a UnitProfile,
}

impl Promotion<'_> {
    /// The units the promotion moved (one, for a proposed step).
    pub fn changed(&self) -> impl Iterator<Item = UnitId> + '_ {
        self.to
            .iter()
            .filter(|&(unit, offset)| self.from.get(unit) != Some(offset))
            .map(|(unit, _)| unit)
    }
}

/// The ordered successful applications of the day: the whole profiles D took and, derived from
/// them, each unit's history of applied values.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DayHistory {
    before: Vec<DayApplication>,
    current: DayApplication,
}

impl DayHistory {
    /// A history that starts at the verified baseline applied at `at`.
    pub fn baseline(at: Seq, profile: UnitProfile) -> Self {
        Self {
            before: Vec::new(),
            current: DayApplication {
                at,
                profile,
                kind: ApplicationKind::Baseline,
            },
        }
    }

    /// Appends a successful application, later than the last and over the same units.
    pub fn push(&mut self, application: DayApplication) -> Result<(), HistoryError> {
        if application.at <= self.current.at {
            return Err(HistoryError::OutOfOrder {
                previous: self.current.at,
                next: application.at,
            });
        }
        if !application.profile.units().eq(self.current.profile.units()) {
            return Err(HistoryError::Units { at: application.at });
        }
        let previous = std::mem::replace(&mut self.current, application);
        self.before.push(previous);
        Ok(())
    }

    /// The latest application: the committed D.
    pub fn current(&self) -> &DayApplication {
        &self.current
    }

    /// Every application, oldest first.
    pub fn iter(&self) -> impl DoubleEndedIterator<Item = &DayApplication> {
        self.before.iter().chain(std::iter::once(&self.current))
    }

    /// The journal position after which hold credit counts: the latest application that started a
    /// hold, since reapplying the committed D preserves it.
    pub fn hold_origin(&self) -> Seq {
        self.iter()
            .rev()
            .find(|application| !matches!(application.kind, ApplicationKind::Reapplication))
            .map_or(self.current.at, |application| application.at)
    }

    /// The promotions since the latest firmware reset, oldest first. Steps before a reset are never
    /// reverted: old facts do not apply in a new firmware context.
    pub fn promotions(&self) -> Vec<Promotion<'_>> {
        let mut promotions = Vec::new();
        let mut previous: Option<&DayApplication> = None;
        for application in self.iter() {
            match &application.kind {
                ApplicationKind::FirmwareZero => promotions.clear(),
                ApplicationKind::Promotion => {
                    if let Some(previous) = previous {
                        promotions.push(Promotion {
                            at: application.at,
                            from: &previous.profile,
                            to: &application.profile,
                        });
                    }
                }
                ApplicationKind::Baseline
                | ApplicationKind::Retreat { .. }
                | ApplicationKind::Fallback { .. }
                | ApplicationKind::SafetyCap
                | ApplicationKind::Reapplication => {}
            }
            previous = Some(application);
        }
        promotions
    }

    /// The step-back applications, oldest first.
    pub(crate) fn retreats(&self) -> impl Iterator<Item = RetreatRecord<'_>> {
        self.iter().filter_map(|application| {
            let (events, outcome) = match &application.kind {
                ApplicationKind::Retreat { events, .. } => (events, RetreatOutcome::Reached),
                ApplicationKind::Fallback { events, wanted, .. } => (events, RetreatOutcome::Fallback { wanted }),
                ApplicationKind::Baseline
                | ApplicationKind::Promotion
                | ApplicationKind::SafetyCap
                | ApplicationKind::FirmwareZero
                | ApplicationKind::Reapplication => return None,
            };
            Some(RetreatRecord {
                events,
                outcome,
            })
        })
    }
}
