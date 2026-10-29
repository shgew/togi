use std::collections::{BTreeMap, BTreeSet};

use crate::{UnitId, UnitProfile};
use togi_journal::Seq;
use togi_machine::{CoreInfo, TargetOffset};

use super::DayParams;
use super::error::DayStepError;
use super::events::{Classification, DailyEvent, EventId, QuestionState};
use super::history::{DayHistory, Promotion, RetreatOutcome, RetreatRecord};
use super::hold::{DayInterval, credited_secs};
use super::profile::CoreProfile;
use super::retreat::{
    EventStatus, ReproductionOutcome, RetreatCause, Veto, awaiting, event_statuses, plan,
    shallower, vetoes, zero_like,
};

/// Whether the firmware context changed since the committed D.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Firmware {
    Current,
    /// A BIOS, AGESA or microcode change not yet answered by a firmware zero.
    Changed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Freeze {
    Off,
    On,
}

/// Whether the cold boot began with the day paused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Pause {
    Running,
    /// Even if the pause ends later in the boot, this boot never promotes.
    Paused,
}

/// How the policy is invoked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Invocation {
    /// A service restart or resume: it may reapply the committed D but never promotes.
    SameBoot,
    ColdBoot {
        pause: Pause,
    },
}

/// Everything the day policy decides from. The night side and the fold supply the caps, the exact
/// qualifications and the firmware validity; this module only consumes them.
#[derive(Clone, Copy, Debug)]
pub struct DayInputs<'a> {
    pub topology: &'a [CoreInfo],
    /// The successful applications; the last is the committed D.
    pub history: &'a DayHistory,
    /// The classified daily events, in journal order.
    pub events: &'a [DailyEvent],
    /// The events whose bounded reproduction task closed.
    pub reproductions: &'a BTreeMap<EventId, ReproductionOutcome>,
    /// The typed intervals of the day, in journal order.
    pub intervals: &'a [DayInterval],
    /// Each unit's current cap S.
    pub caps: &'a UnitProfile,
    /// The exact profiles night-qualified under current content, epoch and firmware.
    pub qualified: &'a BTreeSet<UnitProfile>,
    /// The exact proposed profiles a conclusive qualification failure durably blocks in this session
    /// and firmware context (`docs/spec/tuner.md`, Failed next-step qualification).
    pub blocked: &'a BTreeSet<UnitProfile>,
    pub firmware: Firmware,
    pub freeze: Freeze,
    pub invocation: Invocation,
    pub params: DayParams,
}

/// One unit's next step toward its cap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proposal {
    pub unit: UnitId,
    pub from: TargetOffset,
    pub to: TargetOffset,
    /// `D - S` before the step.
    pub remaining: i64,
    /// Counts moved: `max(1, ceil((D - S) / 2))`.
    pub step: i64,
    /// The whole proposed profile.
    pub profile: UnitProfile,
    /// The undone step back this proposal restores; `None` for an ordinary half-distance step.
    pub restores: Option<Restoration>,
}

/// What a restoration proposal undoes: the step back of events since reclassified as no CPU
/// evidence, and the original promotion it reverted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Restoration {
    /// The reclassified events the step back answered.
    pub events: Vec<EventId>,
    /// The original promotion's application.
    pub promotion: Seq,
    /// The step back's application.
    pub retreat: Seq,
}

/// The next promotion proposal: among the units with `D > S`, the largest `D - S` (ties to the
/// lower unit id) moves `max(1, ceil((D - S) / 2))` counts toward S and never past it; other units
/// keep D (`docs/spec/tuner.md`, Hold and next proposal). `None` when no unit has distance left.
pub fn propose(
    applied: &UnitProfile,
    caps: &UnitProfile,
) -> Result<Option<Proposal>, DayStepError> {
    let mut best: Option<(UnitId, TargetOffset, i64)> = None;
    for (unit, offset) in applied.iter() {
        let cap = caps.get(unit).ok_or(DayStepError::Unit(unit))?;
        let distance = offset.get() - cap.get();
        if distance <= 0 {
            continue;
        }
        if best.is_none_or(|(_, _, widest)| distance > widest) {
            best = Some((unit, offset, distance));
        }
    }
    let Some((unit, from, remaining)) = best else {
        return Ok(None);
    };
    // `remaining` is at least 1, so `(remaining + 1) / 2` is its ceiling half and at least 1, and
    // never more than `remaining`: the step stops at the cap at the furthest.
    let step = (remaining + 1) / 2;
    let to = TargetOffset::new(from.get() - step)?;
    Ok(Some(Proposal {
        unit,
        from,
        to,
        remaining,
        step,
        profile: applied.with(unit, to),
        restores: None,
    }))
}

/// The restoration a reclassification owes (`docs/spec/tuner.md`, Undoing a reclassified retreat):
/// among the applied step backs whose events were all since reclassified as no CPU evidence, the
/// newest original promotion they reverted (ties to the lower unit id) that no later promotion
/// restored, as `(promotion, unit, step back)`. A step back that first fell back to CO 0 is applied
/// once its wanted profile qualifies, and owns the steps its fallback consumed only when every event
/// of that fallback was reclassified too. Promotions before a firmware reset are never restored.
fn restoration<'a>(
    history: &'a DayHistory,
    events: &[DailyEvent],
) -> Option<(Promotion<'a>, UnitId, RetreatRecord<'a>)> {
    let promotions = history.promotions();
    let reclassified = |id: &EventId| {
        events
            .iter()
            .any(|event| &event.id == id && event.class == Classification::NonCpu)
    };
    let mut fallbacks: Vec<RetreatRecord<'a>> = Vec::new();
    let mut best: Option<(Promotion<'a>, UnitId, RetreatRecord<'a>)> = None;
    for record in history.retreats() {
        if matches!(record.outcome, RetreatOutcome::Fallback { .. }) {
            fallbacks.push(record);
            continue;
        }
        if record.events.is_empty() || !record.events.iter().all(reclassified) {
            continue;
        }
        let completed = fallbacks
            .iter()
            .filter(|fallback| {
                fallback.events.iter().any(|id| record.events.contains(id))
                    && fallback.events.iter().all(reclassified)
            })
            .flat_map(|fallback| fallback.reverted);
        for reverted in record.reverted.iter().chain(completed) {
            let Some(promotion) = promotions
                .iter()
                .find(|promotion| promotion.at == reverted.step)
            else {
                continue;
            };
            let Some(value) = promotion.to.get(reverted.unit) else {
                continue;
            };
            let restored = promotions.iter().any(|later| {
                later.at > record.at
                    && later.changed().any(|unit| unit == reverted.unit)
                    && later.to.get(reverted.unit).is_some_and(|at| at <= value)
            });
            let newer = best.as_ref().is_none_or(|(chosen, unit, _)| {
                promotion.at > chosen.at || (promotion.at == chosen.at && reverted.unit < *unit)
            });
            if !restored && newer {
                best = Some((*promotion, reverted.unit, record.clone()));
            }
        }
    }
    best
}

/// The next promotion proposal the day rules make. A restoration a reclassification owes comes
/// first: its unit returns to the original promotion's value, never past the cap, and the other units
/// keep D; with no room left under the cap it proposes nothing rather than fall through to another
/// unit. Otherwise the half-distance step toward the caps.
pub fn next_proposal(
    history: &DayHistory,
    events: &[DailyEvent],
    caps: &UnitProfile,
) -> Result<Option<Proposal>, DayStepError> {
    let applied = &history.current().profile;
    let Some((promotion, unit, record)) = restoration(history, events) else {
        return propose(applied, caps);
    };
    let value = promotion.to.get(unit).ok_or(DayStepError::Unit(unit))?;
    let cap = caps.get(unit).ok_or(DayStepError::Unit(unit))?;
    let from = applied.get(unit).ok_or(DayStepError::Unit(unit))?;
    let to = value.max(cap);
    if from <= to {
        return Ok(None);
    }
    Ok(Some(Proposal {
        unit,
        from,
        to,
        remaining: from.get() - cap.get(),
        step: from.get() - to.get(),
        profile: applied.with(unit, to),
        restores: Some(Restoration {
            events: record.events.to_vec(),
            promotion: promotion.at,
            retreat: record.at,
        }),
    }))
}

/// What the hold looked like when a promotion was chosen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HoldCredit {
    /// D is all-zero: the first nonzero step needs no hold.
    NotRequired,
    /// The applied daily seconds at exactly D that completed the hold.
    Complete { credited_secs: u64 },
}

/// Why the policy holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Blocker {
    /// A service restart or resume never promotes.
    SameBoot,
    /// The cold boot started paused and never promotes.
    PausedColdBoot,
    Freeze,
    /// Unanswered questions whose events have not formed a provisional event.
    PendingQuestion {
        events: Vec<EventId>,
    },
    /// CPU-suspect events whose step back or reproduction is outstanding.
    UnresolvedEvents {
        events: Vec<EventId>,
    },
    /// Every unit is already at its cap.
    NothingToPromote,
    /// The chosen proposal is vetoed; no other unit or step is tried.
    Vetoed {
        proposal: Proposal,
        veto: Veto,
    },
    /// The exact proposed profile lacks night qualification.
    NotQualified {
        proposal: Proposal,
    },
    /// A conclusive qualification failure durably blocks the exact proposed profile; no other unit or
    /// step is tried.
    QualificationFailed {
        proposal: Proposal,
    },
    /// Fewer applied hours at the exact D than the hold needs.
    HoldIncomplete {
        credited_secs: u64,
        required_secs: u64,
    },
}

/// The day policy's decision (`docs/spec/tuner.md`, Day steps). A proposal is not an application:
/// the applying layer commits D only after a verified readback.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DayDecision {
    /// Keep D. Every reason that holds is listed, most fundamental first; never empty.
    Hold { blockers: Vec<Blocker> },
    Promote {
        proposal: Proposal,
        hold: HoldCredit,
        /// The caps the proposal stays within.
        caps: UnitProfile,
    },
    Retreat {
        to: UnitProfile,
        causes: Vec<RetreatCause>,
        /// The retreat profile that lacked qualification when `to` fell back to CO 0 for it.
        unqualified: Option<UnitProfile>,
    },
    FirmwareZero {
        to: UnitProfile,
        /// The retained re-qualification target.
        previous: UnitProfile,
    },
}

fn forbidden(
    vetoes: &[Veto],
    topology: &[CoreInfo],
    profile: &UnitProfile,
) -> Result<Option<Veto>, DayStepError> {
    let core = CoreProfile::of_units(profile, topology)?;
    Ok(vetoes
        .iter()
        .find(|veto| veto.profile.forbids(&core))
        .cloned())
}

/// The required retreat, if any: event step-backs and escalation, then the cap, then the
/// completion of a retreat CO 0 was holding.
fn retreat(
    inputs: &DayInputs<'_>,
    statuses: &[EventStatus],
    vetoes: &[Veto],
) -> Result<Option<DayDecision>, DayStepError> {
    let applied = &inputs.history.current().profile;
    let (mut to, mut causes) = match plan(
        inputs.topology,
        inputs.history,
        inputs.events,
        statuses,
        vetoes,
    )? {
        Some(retreat) => (retreat.to, retreat.causes),
        None => (applied.clone(), Vec::new()),
    };
    let capped = shallower(&to, inputs.caps)?;
    if capped != to {
        causes.push(RetreatCause::SafetyCap {
            caps: inputs.caps.clone(),
        });
        to = capped;
    }
    if causes.is_empty() {
        return completion(inputs, statuses, vetoes);
    }
    let unqualified = (!to.is_zero() && !inputs.qualified.contains(&to)).then(|| to.clone());
    if unqualified.is_some() {
        to = zero_like(&to);
    }
    if to == *applied {
        return Ok(None);
    }
    Ok(Some(DayDecision::Retreat {
        to,
        causes,
        unqualified,
    }))
}

/// Applies the retreat profile CO 0 held once it qualifies: a retreat, not a promotion, so it needs
/// no hold.
fn completion(
    inputs: &DayInputs<'_>,
    statuses: &[EventStatus],
    vetoes: &[Veto],
) -> Result<Option<DayDecision>, DayStepError> {
    let applied = &inputs.history.current().profile;
    let Some((events, wanted)) = awaiting(statuses)? else {
        return Ok(None);
    };
    let candidate = shallower(&wanted, inputs.caps)?;
    if candidate == *applied
        || !inputs.qualified.contains(&candidate)
        || forbidden(vetoes, inputs.topology, &candidate)?.is_some()
    {
        return Ok(None);
    }
    Ok(Some(DayDecision::Retreat {
        to: candidate,
        causes: vec![RetreatCause::Completion { events, wanted }],
        unqualified: None,
    }))
}

fn promotion(
    inputs: &DayInputs<'_>,
    statuses: &[EventStatus],
    vetoes: &[Veto],
) -> Result<DayDecision, DayStepError> {
    let applied = &inputs.history.current().profile;
    let mut blockers = Vec::new();
    match inputs.invocation {
        Invocation::SameBoot => blockers.push(Blocker::SameBoot),
        Invocation::ColdBoot {
            pause: Pause::Paused,
        } => blockers.push(Blocker::PausedColdBoot),
        Invocation::ColdBoot {
            pause: Pause::Running,
        } => {}
    }
    match inputs.freeze {
        Freeze::On => blockers.push(Blocker::Freeze),
        Freeze::Off => {}
    }
    let pending: Vec<EventId> = inputs
        .events
        .iter()
        .filter(|event| event.question == QuestionState::Pending)
        .map(|event| event.id.clone())
        .collect();
    if !pending.is_empty() {
        blockers.push(Blocker::PendingQuestion { events: pending });
    }
    let unresolved: Vec<EventId> = statuses
        .iter()
        .filter(|status| !status.is_resolved() && status.class != Classification::PendingQuestion)
        .map(|status| status.id.clone())
        .collect();
    if !unresolved.is_empty() {
        blockers.push(Blocker::UnresolvedEvents { events: unresolved });
    }

    let proposal = next_proposal(inputs.history, inputs.events, inputs.caps)?;
    match &proposal {
        None => blockers.push(Blocker::NothingToPromote),
        Some(proposal) => {
            if let Some(veto) = forbidden(vetoes, inputs.topology, &proposal.profile)? {
                blockers.push(Blocker::Vetoed {
                    proposal: proposal.clone(),
                    veto,
                });
            }
            if inputs.blocked.contains(&proposal.profile) {
                blockers.push(Blocker::QualificationFailed {
                    proposal: proposal.clone(),
                });
            } else if !inputs.qualified.contains(&proposal.profile) {
                blockers.push(Blocker::NotQualified {
                    proposal: proposal.clone(),
                });
            }
        }
    }

    let hold = if applied.is_zero() {
        HoldCredit::NotRequired
    } else {
        // The hold restarts at each CPU-suspect event at D without a clock of its own: such an
        // event forces a retreat, which is an application and so starts the next hold, and a
        // reclassified event leaves its raw intervals counted.
        let credited = credited_secs(inputs.intervals, applied, inputs.history.hold_origin());
        if credited < inputs.params.hold_secs {
            blockers.push(Blocker::HoldIncomplete {
                credited_secs: credited,
                required_secs: inputs.params.hold_secs,
            });
        }
        HoldCredit::Complete {
            credited_secs: credited,
        }
    };

    match proposal {
        Some(proposal) if blockers.is_empty() => Ok(DayDecision::Promote {
            proposal,
            hold,
            caps: inputs.caps.clone(),
        }),
        Some(_) | None => Ok(DayDecision::Hold { blockers }),
    }
}

/// The day policy: firmware zero, then required retreats (event step-backs, escalation and a cap
/// shallower than D), both ahead of freeze, pause and the hold, then promotion only when every
/// eligibility rule holds (`docs/spec/tuner.md`, Eligibility and constraints). It never writes
/// hardware and never promotes in the same boot.
pub fn day_step(inputs: &DayInputs<'_>) -> Result<DayDecision, DayStepError> {
    let applied = &inputs.history.current().profile;
    if inputs.firmware == Firmware::Changed && !applied.is_zero() {
        return Ok(DayDecision::FirmwareZero {
            to: zero_like(applied),
            previous: applied.clone(),
        });
    }
    let statuses = event_statuses(
        inputs.topology,
        inputs.history,
        inputs.events,
        inputs.reproductions,
    )?;
    let vetoes = vetoes(inputs.events);
    if let Some(decision) = retreat(inputs, &statuses, &vetoes)? {
        return Ok(decision);
    }
    promotion(inputs, &statuses, &vetoes)
}
