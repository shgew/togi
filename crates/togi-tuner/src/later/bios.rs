use std::collections::BTreeMap;

use togi_journal::Seq;
use togi_machine::CoreId;

use crate::state::Run;
use crate::{Depth, Profile};

/// The profile of the latest passed full cycle at or after phase 1's end, and the cycle's event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Confirmed {
    pub(crate) profile: Profile,
    pub(crate) seq: Seq,
}

/// A core showing an offset other than its confirmed one, since the event that left it so.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Unconfirmed {
    pub(crate) core: CoreId,
    pub(crate) since: Seq,
}

/// The profile to enter in BIOS: the last confirmed profile with every later step-back applied,
/// and every valid hold's step-back shown at once.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BiosProfile {
    pub(crate) confirmed: Option<Confirmed>,
    pub(crate) shown: Profile,
    pub(crate) unconfirmed: Vec<Unconfirmed>,
    pub(crate) first_result: bool,
}

impl BiosProfile {
    /// The earliest event that left a core unconfirmed.
    pub(crate) fn since(&self) -> Option<Seq> {
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
        for strike in self.later.strikes() {
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
}
