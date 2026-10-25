use std::collections::{BTreeMap, BTreeSet};

use togi_machine::{CcdId, CcdProfile, CoreInfo, TargetOffset};

use super::DayParams;
use super::error::DayStepError;
use super::events::{Classification, DailyEvent, EventId, QuestionState};
use super::history::DayHistory;
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
    /// Each CCD's current cap S.
    pub caps: &'a CcdProfile,
    /// The exact profiles night-qualified under current content, epoch and firmware.
    pub qualified: &'a BTreeSet<CcdProfile>,
    pub firmware: Firmware,
    pub freeze: Freeze,
    pub invocation: Invocation,
    pub params: DayParams,
}

/// One CCD's next step toward its cap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Proposal {
    pub ccd: CcdId,
    pub from: TargetOffset,
    pub to: TargetOffset,
    /// `D - S` before the step.
    pub remaining: i64,
    /// Counts moved: `max(1, ceil((D - S) / 2))`.
    pub step: i64,
    /// The whole proposed profile.
    pub profile: CcdProfile,
}

/// The next promotion proposal: among the CCDs with `D > S`, the largest `D - S` (ties to the
/// lower CCD id) moves `max(1, ceil((D - S) / 2))` counts toward S and never past it; other CCDs
/// keep D (`docs/spec/tuner.md`, Hold and next proposal). `None` when no CCD has distance left.
pub fn propose(applied: &CcdProfile, caps: &CcdProfile) -> Result<Option<Proposal>, DayStepError> {
    let mut best: Option<(CcdId, TargetOffset, i64)> = None;
    for (ccd, offset) in applied.iter() {
        let cap = caps.get(ccd).ok_or(DayStepError::Ccd(ccd))?;
        let distance = offset.get() - cap.get();
        if distance <= 0 {
            continue;
        }
        if best.is_none_or(|(_, _, widest)| distance > widest) {
            best = Some((ccd, offset, distance));
        }
    }
    let Some((ccd, from, remaining)) = best else {
        return Ok(None);
    };
    // `remaining` is at least 1, so `(remaining + 1) / 2` is its ceiling half and at least 1, and
    // never more than `remaining`: the step stops at the cap at the furthest.
    let step = (remaining + 1) / 2;
    let to = TargetOffset::new(from.get() - step)?;
    Ok(Some(Proposal {
        ccd,
        from,
        to,
        remaining,
        step,
        profile: applied.with(ccd, to),
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
    /// Every CCD is already at its cap.
    NothingToPromote,
    /// The chosen proposal is vetoed; no other CCD or step is tried.
    Vetoed {
        proposal: Proposal,
        veto: Veto,
    },
    /// The exact proposed profile lacks night qualification.
    NotQualified {
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
        caps: CcdProfile,
    },
    Retreat {
        to: CcdProfile,
        causes: Vec<RetreatCause>,
        /// The retreat profile that lacked qualification when `to` fell back to CO 0 for it.
        unqualified: Option<CcdProfile>,
    },
    FirmwareZero {
        to: CcdProfile,
        /// The retained re-qualification target.
        previous: CcdProfile,
    },
}

fn forbidden(
    vetoes: &[Veto],
    topology: &[CoreInfo],
    profile: &CcdProfile,
) -> Result<Option<Veto>, DayStepError> {
    let core = CoreProfile::of_ccd(profile, topology)?;
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

    let proposal = propose(applied, inputs.caps)?;
    match &proposal {
        None => blockers.push(Blocker::NothingToPromote),
        Some(proposal) => {
            if let Some(veto) = forbidden(vetoes, inputs.topology, &proposal.profile)? {
                blockers.push(Blocker::Vetoed {
                    proposal: proposal.clone(),
                    veto,
                });
            }
            if !inputs.qualified.contains(&proposal.profile) {
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
