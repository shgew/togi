use std::collections::{BTreeMap, BTreeSet};

use togi_machine::{CcdId, CcdProfile, CoreInfo, TargetOffset};

use super::error::DayStepError;
use super::events::{Certainty, Classification, DailyEvent, EventId};
use super::history::{DayHistory, Promotion, RetreatOutcome, Reverted};
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
    AwaitingQualification { wanted: CcdProfile },
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
    let applied = CoreProfile::of_ccd(&history.current().profile, topology)?;
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

/// An event a retreat answers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetreatEvent {
    pub id: EventId,
    pub certainty: Certainty,
}

/// Why a retreat happens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RetreatCause {
    /// The first CPU-suspect event at its profile reverts the newest applied promotion.
    StepBack {
        events: Vec<RetreatEvent>,
        reverted: Vec<Reverted>,
    },
    /// A further event at the stepped-back profile reverts every CCD's own newest remaining step.
    Escalation {
        events: Vec<RetreatEvent>,
        reverted: Vec<Reverted>,
    },
    /// A cap shallower than D: the profile is cut to the cap componentwise, with no hold.
    SafetyCap { caps: CcdProfile },
    /// CO 0 held the retreat profile `wanted` until it qualified; it now does.
    Completion {
        events: Vec<RetreatEvent>,
        wanted: CcdProfile,
    },
}

/// The event-driven retreat: its target profile and the causes that make it.
pub(crate) struct EventRetreat {
    pub to: CcdProfile,
    pub causes: Vec<RetreatCause>,
}

struct Group {
    further: bool,
    events: Vec<RetreatEvent>,
    reverted: Vec<Reverted>,
}

/// The value a CCD returns to when its newest step is reverted: the step's previous day value,
/// never deeper than the CCD already is.
fn previous_value(promotion: &Promotion<'_>, ccd: CcdId, now: TargetOffset) -> TargetOffset {
    promotion
        .from
        .get(ccd)
        .map_or(TargetOffset::ZERO, |before| before.max(now))
}

pub(crate) fn zero_like(profile: &CcdProfile) -> CcdProfile {
    profile.iter().fold(profile.clone(), |zero, (ccd, _)| {
        zero.with(ccd, TargetOffset::ZERO)
    })
}

/// First event at a profile: the newest remaining promotion goes back to its previous profile.
fn revert_newest(
    promotions: &[Promotion<'_>],
    consumed: &mut BTreeSet<Reverted>,
    current: &CcdProfile,
) -> (CcdProfile, Vec<Reverted>) {
    for promotion in promotions.iter().rev() {
        let open: Vec<Reverted> = promotion
            .changed()
            .map(|ccd| Reverted {
                step: promotion.at,
                ccd,
            })
            .filter(|reverted| !consumed.contains(reverted))
            .collect();
        if open.is_empty() {
            continue;
        }
        let mut next = current.clone();
        for reverted in &open {
            let now = current.get(reverted.ccd).unwrap_or(TargetOffset::ZERO);
            next = next.with(reverted.ccd, previous_value(promotion, reverted.ccd, now));
            consumed.insert(*reverted);
        }
        return (next, open);
    }
    (zero_like(current), Vec::new())
}

/// Further event: every CCD goes back to its previous day value, CO 0 where it has no remaining
/// step.
fn revert_all(
    promotions: &[Promotion<'_>],
    consumed: &mut BTreeSet<Reverted>,
    current: &CcdProfile,
) -> (CcdProfile, Vec<Reverted>) {
    let mut next = current.clone();
    let mut reverted = Vec::new();
    for (ccd, now) in current.iter() {
        let newest = promotions.iter().rev().find(|promotion| {
            promotion.changed().any(|changed| changed == ccd)
                && !consumed.contains(&Reverted {
                    step: promotion.at,
                    ccd,
                })
        });
        let value = match newest {
            Some(promotion) => {
                let step = Reverted {
                    step: promotion.at,
                    ccd,
                };
                consumed.insert(step);
                reverted.push(step);
                previous_value(promotion, ccd, now)
            }
            None => TargetOffset::ZERO,
        };
        next = next.with(ccd, value);
    }
    (next, reverted)
}

/// The profile at the shallower of `a` and `b` on every CCD: how a retreat is cut to a cap and how
/// retreat profiles combine without deepening anything.
pub(crate) fn shallower(a: &CcdProfile, b: &CcdProfile) -> Result<CcdProfile, DayStepError> {
    a.iter().try_fold(a.clone(), |cut, (ccd, offset)| {
        let other = b.get(ccd).ok_or(DayStepError::Ccd(ccd))?;
        Ok(cut.with(ccd, offset.max(other)))
    })
}

/// Plans the step back every required event asks for, in event order, from the committed D and
/// the escalation chain the active events and their applied retreats define. The first event at a
/// profile reverts the newest remaining promotion; an event at a profile an earlier retreat of the
/// chain stepped back to reverts every CCD's own newest remaining step. A target some veto still
/// forbids steps back further the same way, down to CO 0.
pub(crate) fn plan(
    topology: &[CoreInfo],
    history: &DayHistory,
    events: &[DailyEvent],
    statuses: &[EventStatus],
    vetoes: &[Veto],
) -> Result<Option<EventRetreat>, DayStepError> {
    let active: BTreeSet<&EventId> = events
        .iter()
        .filter(|event| event.class.is_cpu_suspect())
        .map(|event| &event.id)
        .collect();
    let promotions = history.promotions();
    let mut consumed = BTreeSet::new();
    let mut tails = Vec::new();
    for record in history
        .retreats()
        .filter(|record| record.events.iter().any(|id| active.contains(id)))
    {
        consumed.extend(record.reverted.iter().copied());
        if matches!(record.outcome, RetreatOutcome::Reached) {
            tails.push(CoreProfile::of_ccd(record.profile, topology)?);
        }
    }

    let mut current = history.current().profile.clone();
    let mut groups: Vec<Group> = Vec::new();
    for (event, status) in events.iter().zip(statuses) {
        if status.retreat != RetreatState::Required {
            continue;
        }
        let Some(certainty) = event.class.certainty() else {
            continue;
        };
        let witness = RetreatEvent {
            id: event.id.clone(),
            certainty,
        };
        if !event
            .profile
            .forbids(&CoreProfile::of_ccd(&current, topology)?)
        {
            // An earlier event's retreat already left this event's veto: one step back answers both.
            if let Some(group) = groups.last_mut() {
                group.events.push(witness);
            }
            continue;
        }
        let further = tails.contains(&event.profile);
        let (next, reverted) = if further {
            revert_all(&promotions, &mut consumed, &current)
        } else {
            revert_newest(&promotions, &mut consumed, &current)
        };
        tails.push(CoreProfile::of_ccd(&next, topology)?);
        current = next;
        groups.push(Group {
            further,
            events: vec![witness],
            reverted,
        });
    }
    if groups.is_empty() {
        return Ok(None);
    }
    while !current.is_zero() {
        let core = CoreProfile::of_ccd(&current, topology)?;
        if !vetoes.iter().any(|veto| veto.profile.forbids(&core)) {
            break;
        }
        let (next, reverted) = revert_all(&promotions, &mut consumed, &current);
        if let Some(group) = groups.last_mut() {
            group.reverted.extend(reverted);
        }
        current = if next == current {
            zero_like(&current)
        } else {
            next
        };
    }
    let causes = groups
        .into_iter()
        .map(|group| {
            if group.further {
                RetreatCause::Escalation {
                    events: group.events,
                    reverted: group.reverted,
                }
            } else {
                RetreatCause::StepBack {
                    events: group.events,
                    reverted: group.reverted,
                }
            }
        })
        .collect();
    Ok(Some(EventRetreat {
        to: current,
        causes,
    }))
}

/// The retreat profile CO 0 is holding for events, once any: the shallowest of their wanted
/// profiles and the events waiting for it.
pub(crate) fn awaiting(
    events: &[EventStatus],
) -> Result<Option<(Vec<RetreatEvent>, CcdProfile)>, DayStepError> {
    let mut witnesses = Vec::new();
    let mut combined: Option<CcdProfile> = None;
    for status in events {
        if let RetreatState::AwaitingQualification { wanted } = &status.retreat
            && let Some(certainty) = status.class.certainty()
        {
            witnesses.push(RetreatEvent {
                id: status.id.clone(),
                certainty,
            });
            combined = Some(match combined {
                Some(so_far) => shallower(&so_far, wanted)?,
                None => wanted.clone(),
            });
        }
    }
    Ok(combined.map(|wanted| (witnesses, wanted)))
}
