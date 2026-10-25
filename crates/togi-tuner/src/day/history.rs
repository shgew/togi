use togi_journal::Seq;
use togi_machine::{CcdId, CcdProfile};

use super::events::EventId;

/// One CCD's share of an applied promotion that an escalation reverted: the cursor that moves past
/// the step (`docs/spec/tuner.md`, Whole-profile veto and escalation).
#[derive(
    Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
pub struct Reverted {
    /// The applied promotion, by the journal position of its application.
    pub step: Seq,
    pub ccd: CcdId,
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
        wanted: CcdProfile,
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
    /// The journal position of the step-back application.
    pub at: Seq,
    pub profile: &'a CcdProfile,
    pub events: &'a [EventId],
    pub reverted: &'a [Reverted],
    pub outcome: RetreatOutcome<'a>,
}

#[derive(Clone, Copy)]
pub(crate) enum RetreatOutcome<'a> {
    /// The retreat profile itself was applied.
    Reached,
    /// CO 0 was applied until `wanted` qualifies.
    Fallback { wanted: &'a CcdProfile },
}

/// A successful application: the whole profile read back and why it was applied.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DayApplication {
    /// The journal position of its commit; it orders the application against events and intervals.
    pub at: Seq,
    pub profile: CcdProfile,
    pub kind: ApplicationKind,
}

/// Why an application cannot extend the history.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum HistoryError {
    #[error("application {next} does not follow application {previous}")]
    OutOfOrder { previous: Seq, next: Seq },
    #[error("application {at} names other CCDs than the baseline")]
    Ccds { at: Seq },
}

/// A promotion since the latest firmware reset, with the profile it left and the one it reached.
#[derive(Clone, Copy, Debug)]
pub struct Promotion<'a> {
    pub at: Seq,
    pub from: &'a CcdProfile,
    pub to: &'a CcdProfile,
}

impl Promotion<'_> {
    /// The CCDs the promotion moved (one, for a proposed step).
    pub fn changed(&self) -> impl Iterator<Item = CcdId> + '_ {
        self.to
            .iter()
            .filter(|&(ccd, offset)| self.from.get(ccd) != Some(offset))
            .map(|(ccd, _)| ccd)
    }
}

/// The ordered successful applications of the day: the whole profiles D took and, derived from
/// them, each CCD's history of applied values.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DayHistory {
    before: Vec<DayApplication>,
    current: DayApplication,
}

impl DayHistory {
    /// A history that starts at the verified baseline applied at `at`.
    pub fn baseline(at: Seq, profile: CcdProfile) -> Self {
        Self {
            before: Vec::new(),
            current: DayApplication {
                at,
                profile,
                kind: ApplicationKind::Baseline,
            },
        }
    }

    /// Appends a successful application, later than the last and over the same CCDs.
    pub fn push(&mut self, application: DayApplication) -> Result<(), HistoryError> {
        if application.at <= self.current.at {
            return Err(HistoryError::OutOfOrder {
                previous: self.current.at,
                next: application.at,
            });
        }
        if !application.profile.ccds().eq(self.current.profile.ccds()) {
            return Err(HistoryError::Ccds { at: application.at });
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
            let (events, reverted, outcome) = match &application.kind {
                ApplicationKind::Retreat { events, reverted } => {
                    (events, reverted, RetreatOutcome::Reached)
                }
                ApplicationKind::Fallback {
                    events,
                    reverted,
                    wanted,
                } => (events, reverted, RetreatOutcome::Fallback { wanted }),
                ApplicationKind::Baseline
                | ApplicationKind::Promotion
                | ApplicationKind::SafetyCap
                | ApplicationKind::FirmwareZero
                | ApplicationKind::Reapplication => return None,
            };
            Some(RetreatRecord {
                at: application.at,
                profile: &application.profile,
                events,
                reverted,
                outcome,
            })
        })
    }
}
