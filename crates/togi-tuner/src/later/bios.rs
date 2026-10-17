use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use togi_journal::{Decision, Seq};
use togi_machine::{CoreId, PerCore, TargetOffset};

use super::Strike;
use crate::hunt::plan::GroupOutcome;
use crate::hunt::state::{Active, Recorded};
use crate::r7::Applied;
use crate::state::Run;
use crate::{Action, CombinationId, DecisionPayload, Depth, Profile};

/// The profile of the latest passed full cycle at or after phase 1's end, and the cycle's event.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Confirmed {
    pub profile: Profile,
    pub seq: Seq,
}

/// A core showing an offset other than its confirmed one, since the event that left it so.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Unconfirmed {
    pub core: CoreId,
    pub since: Seq,
}

/// The profile to enter in BIOS: the last confirmed profile with every later step-back applied,
/// and every valid hold's step-back shown at once.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BiosProfile {
    pub confirmed: Option<Confirmed>,
    pub shown: Profile,
    pub unconfirmed: Vec<Unconfirmed>,
    pub first_result: bool,
}

impl BiosProfile {
    /// The earliest event that left a core unconfirmed.
    pub fn since(&self) -> Option<Seq> {
        self.unconfirmed.iter().map(|mark| mark.since).min()
    }
}

impl Run {
    /// What to enter in BIOS. Before phase 1 ends it is the current profile with nothing
    /// unconfirmed. After, each core shows the shallowest of its confirmed and current offsets
    /// and the targets of the valid holds that name it, so a deeper phase-2 move stays hidden
    /// until a passed cycle confirms it.
    pub(crate) fn bios_profile(&self) -> BiosProfile {
        let current = self.offsets();
        let Some(confirmed) = self.later.confirmed() else {
            return BiosProfile {
                confirmed: None,
                shown: current,
                unconfirmed: Vec::new(),
                first_result: false,
            };
        };
        let mut shown = current;
        let mut since: BTreeMap<CoreId, Seq> = BTreeMap::new();
        for (core, at) in current.iter() {
            let Some(base) = confirmed.profile.get(core) else {
                continue;
            };
            let offset = if at.deeper_than(base) { base } else { at };
            shown = shown.with(core, offset);
            if offset != base {
                let moved = self
                    .later
                    .moved_since(core)
                    .or(self.core(core).last_decision);
                since.insert(core, moved.unwrap_or(confirmed.seq));
            }
        }
        let paired = self
            .hunts
            .active
            .as_deref()
            .filter(|hunt| hunt.paired.is_some_and(|strike| self.strike_holds(&strike)));
        let shown_strikes = self
            .later
            .strikes()
            .chain(paired.and_then(|hunt| hunt.paired.as_ref()));
        for strike in shown_strikes {
            for (core, target) in strike.display.iter() {
                let Some(base) = confirmed.profile.get(core) else {
                    continue;
                };
                if !target.shallower_than(base) {
                    continue;
                }
                if shown.get(core).is_some_and(|at| target.shallower_than(at)) {
                    shown = shown.with(core, target);
                }
                since
                    .entry(core)
                    .and_modify(|at| *at = (*at).min(strike.hold))
                    .or_insert(strike.hold);
            }
        }
        if let Some(hunt) = paired {
            self.step_past_observed_failures(hunt, &mut shown, &mut since);
            if let Some((core, to)) = self.paired_hunt_backoff(hunt) {
                let past = confirmed
                    .profile
                    .get(core)
                    .is_some_and(|base| to.shallower_than(base))
                    && shown.get(core).is_some_and(|at| to.shallower_than(at));
                if past {
                    shown = shown.with(core, to);
                    let end = hunt.end.as_ref().map_or(hunt.seq, |end| end.seq);
                    since.entry(core).or_insert(end.max(hunt.seq));
                }
            }
        }
        let unconfirmed: Vec<Unconfirmed> = since
            .into_iter()
            .map(|(core, since)| Unconfirmed { core, since })
            .collect();
        BiosProfile {
            confirmed: Some(confirmed),
            shown,
            first_result: unconfirmed.is_empty(),
            unconfirmed,
        }
    }

    /// Whether every core a strike counts against is still where the strike found it.
    fn strike_holds(&self, strike: &Strike) -> bool {
        strike
            .cores
            .iter()
            .all(|(core, at)| self.core(core).offset == at)
    }

    /// While a paired hunt's hold shows, no failing group the hunt observed stays reached: each
    /// one the shown profile reaches steps the cores it held deeper than parked one count
    /// shallower than its failing offsets, unconfirmed since the failure, until none is reached.
    fn step_past_observed_failures(
        &self,
        hunt: &Active,
        shown: &mut Profile,
        since: &mut BTreeMap<CoreId, Seq>,
    ) {
        let failed: Vec<(Seq, &Profile)> = hunt
            .groups
            .iter()
            .filter_map(|group| {
                let answer = self.answer_of(hunt, group);
                (answer.outcome == Some(GroupOutcome::Failed))
                    .then(|| (answer.failure.unwrap_or(group.seq), &group.profile))
            })
            .collect();
        let mut changed = true;
        while changed {
            changed = false;
            for &(seq, profile) in &failed {
                if !shown.covers(profile) {
                    continue;
                }
                for (core, failing) in profile.iter() {
                    let held = failing.deeper_than(hunt.parked.at(core));
                    if !held || shown.at(core).shallower_than(failing) {
                        continue;
                    }
                    *shown = shown.with(core, failing.shallower_by(1));
                    changed = true;
                    since
                        .entry(core)
                        .and_modify(|at| *at = (*at).min(seq))
                        .or_insert(seq);
                }
            }
        }
    }

    /// The core a paired hunt's next decision moves and where, as `next` decides it: the
    /// backoff of the attributed failure that ends it direct, or once it ended, a pending
    /// ordinary backoff, its combination's backoff (also before the combination is recorded) or
    /// its culprit's.
    fn paired_hunt_backoff(&self, hunt: &Active) -> Option<(CoreId, TargetOffset)> {
        let action = match self.ordinary_decision() {
            Some(action) => self.later_decision(action),
            None => Action::Decide(self.hunt_commitment(hunt)?),
        };
        let Action::Decide(decision) = action else {
            return None;
        };
        let ended = hunt.end.as_ref();
        match &decision.payload {
            DecisionPayload::TunerDecision(backoff) => (ended.is_some()
                && backoff.decision == Decision::Backoff)
                .then_some((backoff.core, backoff.to_offset)),
            DecisionPayload::Combination(combination) => {
                ended?;
                let recorded = Recorded {
                    seq: hunt.seq,
                    id: CombinationId::new(combination.combination),
                    members: combination
                        .members
                        .iter()
                        .map(|member| (member.core, member.offset))
                        .collect::<PerCore<TargetOffset>>(),
                };
                let backoff = self.combination_commitment(hunt, &recorded)?;
                if let DecisionPayload::TunerDecision(backoff) = backoff.payload
                    && backoff.decision == Decision::Backoff
                {
                    Some((backoff.core, backoff.to_offset))
                } else {
                    None
                }
            }
            DecisionPayload::HuntEnd(end) if end.result == togi_journal::HuntOutcome::Direct => {
                let core = end.cores.first().filter(|_| end.cores.len() == 1)?;
                let failing = self
                    .failures
                    .pending(*decision.cause.first()?)?
                    .failure
                    .offset?;
                let offset = self.core(core).offset;
                let target = failing.shallower_by(1);
                Some((
                    core,
                    if offset.shallower_than(target) {
                        offset
                    } else {
                        target
                    },
                ))
            }
            DecisionPayload::HuntEnd(_)
            | DecisionPayload::Failure(_)
            | DecisionPayload::TunerWarning(_)
            | DecisionPayload::DeadEnd(_)
            | DecisionPayload::CorePhase(_)
            | DecisionPayload::ProfileChange(_)
            | DecisionPayload::CheckingCycle(_)
            | DecisionPayload::CheckingStep(_)
            | DecisionPayload::CheckingChain(_)
            | DecisionPayload::HuntStart(_)
            | DecisionPayload::HuntGroup(_)
            | DecisionPayload::HuntSkipped(_)
            | DecisionPayload::DeepeningRound(_) => None,
        }
    }

    /// The BIOS profile is confirmed and no core has been stepped back or held since.
    pub(crate) fn first_result(&self) -> bool {
        self.bios_profile().first_result
    }
}
