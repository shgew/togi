use std::collections::BTreeSet;

use crate::day::{DailyEvent, Firmware, Freeze, HOLD_SECS, Invocation, Pause, Veto};
use crate::unit::{UnitId, UnitProfile};

use super::{
    Campaign, Ceilings, ClimbError, ClimbHistory, Proposal, RestorationKind, RestorationProposal,
    Retreat, SavedProfile, propose, restore, retreat,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Blocker {
    Hold {
        credited_secs: u64,
        required_secs: u64,
    },
    HoldProfile,
    Certificate,
    FailedCertificate,
    PendingQuestion,
    UnresolvedEvent,
    Veto,
    FirmwareInvalid,
    Freeze,
    Pause,
    SameBoot,
    AllowanceExhausted,
    CampaignHistory,
    UnknownEdge {
        unit: UnitId,
    },
    Ceiling {
        unit: UnitId,
    },
    Finished,
    ActivationClosed,
    SafeApply,
}

/// Credit already filtered to checkpointed, verified active intervals after the current hold origin.
#[derive(Clone, Copy, Debug)]
pub struct HoldInput<'a> {
    pub profile: &'a UnitProfile,
    pub credited_secs: u64,
}

/// The applying layer's independent context and hardware prerequisites.
#[derive(Clone, Copy, Debug)]
pub struct ApplyPrerequisites {
    pub firmware_valid: bool,
    pub activation_open: bool,
    pub safe_apply: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct ClimbInputs<'a> {
    pub history: &'a ClimbHistory,
    pub campaign: &'a Campaign,
    pub ceilings: &'a Ceilings,
    pub events: &'a [DailyEvent],
    pub vetoes: &'a [Veto],
    /// Exact full vectors with valid complete certificates in the current context.
    pub qualified: &'a BTreeSet<UnitProfile>,
    /// Includes raw conclusive failures whose derived commitment has not yet been written.
    pub failed: &'a BTreeSet<UnitProfile>,
    pub hold: HoldInput<'a>,
    pub firmware: Firmware,
    pub invocation: Invocation,
    pub freeze: Freeze,
    pub pending_question: bool,
    pub unresolved_event: bool,
    pub apply: ApplyPrerequisites,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClimbAction {
    FirmwareZero {
        saved: SavedProfile,
        profile: UnitProfile,
    },
    Retreat(Retreat),
    Restore(RestorationProposal),
    Promote(Proposal),
    Hold {
        blocked_by: Vec<Blocker>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClimbDecision {
    pub before: UnitProfile,
    pub ceilings: Ceilings,
    pub action: ClimbAction,
}

impl ClimbDecision {
    pub fn target(&self) -> Option<&UnitProfile> {
        match &self.action {
            ClimbAction::FirmwareZero { profile, .. } => Some(profile),
            ClimbAction::Retreat(retreat) => Some(&retreat.profile),
            ClimbAction::Restore(proposal) => Some(&proposal.profile),
            ClimbAction::Promote(proposal) => Some(&proposal.profile),
            ClimbAction::Hold { .. } => None,
        }
    }
}

fn qualified(inputs: &ClimbInputs<'_>, profile: &UnitProfile) -> bool {
    !inputs.failed.contains(profile) && inputs.qualified.contains(profile)
}

fn safety_target(inputs: &ClimbInputs<'_>, mut plan: Retreat) -> Retreat {
    if !plan.wanted.is_zero() && !qualified(inputs, &plan.wanted) {
        plan.profile = UnitProfile::zero(inputs.history.units());
        for (unit, depth) in inputs.history.current().iter() {
            if depth != togi_machine::TargetOffset::ZERO && !plan.stopped.contains(&unit) {
                plan.stopped.push(unit);
            }
        }
    }
    plan
}

fn completion(inputs: &ClimbInputs<'_>) -> Result<Option<Retreat>, ClimbError> {
    let Some(record) = inputs.history.retreats().last() else {
        return Ok(None);
    };
    if !inputs.history.current().is_zero() || !record.after.is_zero() || record.wanted.is_zero() {
        return Ok(None);
    }
    if !record.events.iter().all(|id| {
        inputs
            .events
            .iter()
            .any(|event| event.id == *id && event.class.is_cpu_suspect())
    }) {
        return Ok(None);
    }
    let mut wanted = SavedProfile {
        units: record.units.clone(),
        profile: record.wanted.clone(),
    }
    .for_units(inputs.history.units())?;
    wanted = UnitProfile::try_from(
        wanted
            .iter()
            .map(|(unit, offset)| {
                let offset = inputs
                    .ceilings
                    .get(&unit)
                    .and_then(|ceiling| ceiling.c)
                    .map_or(offset, |c| offset.max(c));
                (unit, offset)
            })
            .collect::<Vec<_>>(),
    )?;
    if wanted.is_zero()
        || !qualified(inputs, &wanted)
        || retreat::forbidden(inputs.history, &wanted, inputs.vetoes)?
    {
        return Ok(None);
    }
    Ok(Some(Retreat {
        profile: wanted.clone(),
        wanted,
        events: record.events.clone(),
        cursors: Vec::new(),
        stopped: Vec::new(),
        adverse_units: Vec::new(),
    }))
}

fn blockers(
    inputs: &ClimbInputs<'_>,
    target: &UnitProfile,
    restoration: Option<RestorationKind>,
) -> Result<Vec<Blocker>, ClimbError> {
    let mut blocked = Vec::new();
    if inputs.hold.profile != inputs.history.current() {
        blocked.push(Blocker::HoldProfile);
    }
    if inputs.hold.credited_secs < HOLD_SECS {
        blocked.push(Blocker::Hold {
            credited_secs: inputs.hold.credited_secs,
            required_secs: HOLD_SECS,
        });
    }
    if inputs.failed.contains(target) {
        blocked.push(Blocker::FailedCertificate);
    } else if !inputs.qualified.contains(target) {
        blocked.push(Blocker::Certificate);
    }
    if inputs.pending_question {
        blocked.push(Blocker::PendingQuestion);
    }
    if inputs.unresolved_event {
        blocked.push(Blocker::UnresolvedEvent);
    }
    if retreat::forbidden(inputs.history, target, inputs.vetoes)? {
        blocked.push(Blocker::Veto);
    }
    if !inputs.apply.firmware_valid {
        blocked.push(Blocker::FirmwareInvalid);
    }
    if inputs.freeze == Freeze::On {
        blocked.push(Blocker::Freeze);
    }
    match inputs.invocation {
        Invocation::SameBoot => blocked.push(Blocker::SameBoot),
        Invocation::ColdBoot {
            pause: Pause::Paused,
        } => blocked.push(Blocker::Pause),
        Invocation::ColdBoot {
            pause: Pause::Running,
        } => {}
    }
    match inputs.campaign.remaining() {
        None => blocked.push(Blocker::CampaignHistory),
        Some(0) => blocked.push(Blocker::AllowanceExhausted),
        Some(_) => {}
    }
    if !inputs.apply.activation_open {
        blocked.push(Blocker::ActivationClosed);
    }
    if !inputs.apply.safe_apply {
        blocked.push(Blocker::SafeApply);
    }
    for (unit, offset) in target.iter() {
        let c = inputs.ceilings.get(&unit).and_then(|ceiling| ceiling.c);
        if c.is_some_and(|c| offset < c) {
            blocked.push(Blocker::Ceiling { unit });
        }
        if c.is_none()
            && restoration != Some(RestorationKind::Firmware)
            && inputs
                .history
                .current()
                .get(unit)
                .is_some_and(|current| offset < current)
        {
            blocked.push(Blocker::UnknownEdge { unit });
        }
    }
    Ok(blocked)
}

/// Firmware zero → mandatory safety → restoration → ordinary promotion → explicit hold.
/// This is a plan: only `ClimbHistory::apply` after successful readback commits a step.
pub fn decide(inputs: &ClimbInputs<'_>) -> Result<ClimbDecision, ClimbError> {
    let action = if inputs.firmware == Firmware::Changed {
        let saved = inputs
            .history
            .saved()
            .cloned()
            .unwrap_or_else(|| SavedProfile {
                units: inputs.history.units().clone(),
                profile: inputs.history.current().clone(),
            });
        ClimbAction::FirmwareZero {
            saved,
            profile: UnitProfile::zero(inputs.history.units()),
        }
    } else if let Some(plan) = retreat::plan(
        inputs.history,
        inputs.events,
        inputs.vetoes,
        inputs.ceilings,
    )? {
        ClimbAction::Retreat(safety_target(inputs, plan))
    } else if let Some(plan) = completion(inputs)? {
        ClimbAction::Retreat(plan)
    } else if let Some(proposal) = restore::candidate(inputs.history, inputs.events)? {
        let blocked_by = blockers(inputs, &proposal.profile, Some(proposal.kind))?;
        if blocked_by.is_empty() {
            ClimbAction::Restore(proposal)
        } else {
            ClimbAction::Hold { blocked_by }
        }
    } else if let Some(proposal) = propose(
        inputs.history.current(),
        inputs.ceilings,
        inputs.history.finishes(),
    ) {
        let blocked_by = blockers(inputs, &proposal.profile, None)?;
        if blocked_by.is_empty() {
            ClimbAction::Promote(proposal)
        } else {
            ClimbAction::Hold { blocked_by }
        }
    } else {
        let mut blocked_by = Vec::new();
        for (unit, _) in inputs.history.current().iter() {
            if !inputs.history.finishes().is_finished(unit)
                && inputs
                    .ceilings
                    .get(&unit)
                    .and_then(|ceiling| ceiling.c)
                    .is_none()
            {
                blocked_by.push(Blocker::UnknownEdge { unit });
            }
        }
        match inputs.campaign.remaining() {
            None => blocked_by.push(Blocker::CampaignHistory),
            Some(0) => blocked_by.push(Blocker::AllowanceExhausted),
            Some(_) => {}
        }
        if blocked_by.is_empty() {
            blocked_by.push(Blocker::Finished);
        }
        ClimbAction::Hold { blocked_by }
    };
    Ok(ClimbDecision {
        before: inputs.history.current().clone(),
        ceilings: inputs.ceilings.clone(),
        action,
    })
}
