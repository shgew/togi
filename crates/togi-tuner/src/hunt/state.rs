use std::collections::{BTreeMap, HashMap};
use std::num::{NonZeroU16, NonZeroU32};

use togi_journal::{HuntEnd, HuntGroup, HuntOutcome, HuntStart, Seq};
use togi_machine::{CoreId, CoreSet, PerCore, TargetOffset};

use super::plan::{GroupOutcome, Plan};
use crate::r7::failed::OpenFailure;
use crate::{ClassKey, CombinationId, Profile, Ranking, Secs};

/// What established a group's outcome at some moment: the outcome (none while the group runs),
/// the newest admitted failure that rejected it and the carried passes among the first `needed`
/// that cover it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Answer {
    pub(crate) outcome: Option<GroupOutcome>,
    pub(crate) failure: Option<Seq>,
    pub(crate) carried: Vec<Seq>,
    pub(crate) passes: u32,
}

impl Answer {
    pub(crate) fn is_carried(&self, carried: impl Fn(Seq) -> bool) -> bool {
        self.failure.is_some_and(&carried) || !self.carried.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GroupRecord {
    pub(crate) seq: Seq,
    pub(crate) payload: HuntGroup,
    pub(crate) profile: Profile,
    pub(crate) class: ClassKey,
    pub(crate) inherited: Vec<Seq>,
    pub(crate) own: Option<Answer>,
    pub(crate) closing: Option<Answer>,
}

impl GroupRecord {
    pub(crate) fn is_probe(&self) -> bool {
        self.payload.probe.is_some()
    }

    pub(crate) fn number(&self) -> u32 {
        self.payload.group.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Repeated {
    pub(crate) core: CoreId,
    pub(crate) failures: [Seq; 2],
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Recorded {
    pub(crate) seq: Seq,
    pub(crate) id: CombinationId,
    pub(crate) members: PerCore<TargetOffset>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Ended {
    pub(crate) seq: Seq,
    pub(crate) payload: HuntEnd,
    pub(crate) direct_failure: Option<Seq>,
    pub(crate) combination: Option<Recorded>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Cut {
    pub(crate) count: u32,
    pub(crate) signature: Option<(CoreSet, u32, u32)>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Active {
    pub(crate) id: NonZeroU32,
    pub(crate) seq: Seq,
    pub(crate) start: HuntStart,
    pub(crate) failing: Profile,
    pub(crate) parked: Profile,
    pub(crate) class: ClassKey,
    pub(crate) needed: NonZeroU16,
    pub(crate) short: Secs,
    pub(crate) recent: CoreSet,
    pub(crate) ranking: Ranking,
    pub(crate) located: bool,
    pub(crate) prior: Vec<Seq>,
    pub(crate) repeated: Option<Repeated>,
    pub(crate) plan: Plan,
    pub(crate) groups: Vec<GroupRecord>,
    pub(crate) end: Option<Ended>,
    pub(crate) cut: Cut,
}

impl Active {
    pub(crate) fn class_at(&self, duration: Secs) -> ClassKey {
        ClassKey::new(self.class.workload(), self.class.loaded(), duration)
    }

    pub(crate) fn first_probe(&self) -> usize {
        self.groups
            .iter()
            .position(GroupRecord::is_probe)
            .unwrap_or(self.groups.len())
    }

    pub(crate) fn settled(&self, index: usize) -> bool {
        let Some(group) = self.groups.get(index) else {
            return false;
        };
        self.groups
            .get(index + 1)
            .is_some_and(|next| !next.is_probe())
            && !group.is_probe()
            && group.payload.stage != togi_journal::HuntStage::Locate
    }

    pub(crate) fn latest(&self) -> Option<&GroupRecord> {
        self.groups.last()
    }
}

/// How a located hunt ended: the failure that rejected its last group and the loaded core that
/// failure named, with the failed trial the charge reads.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LocatedEnd {
    pub(crate) seq: Seq,
    pub(crate) result: HuntOutcome,
    pub(crate) failure: Option<Seq>,
    pub(crate) named: Option<Named>,
    pub(crate) all_zero: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Named {
    pub(crate) core: CoreId,
    pub(crate) failure: Box<OpenFailure>,
}

/// A multi-core R7 failure answered by a located hunt: open while `end` is empty, consumed by any
/// end but `loaded` and charged to the loaded cores after one.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Located {
    pub(crate) hunt: NonZeroU32,
    pub(crate) source: Seq,
    pub(crate) end: Option<LocatedEnd>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Rejecting {
    pub(crate) ledger: Seq,
    pub(crate) open: OpenFailure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ParkedFailure {
    pub(crate) seq: Seq,
    pub(crate) core: CoreId,
    pub(crate) offset: TargetOffset,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Newest {
    live: [Option<ParkedFailure>; 2],
    carried: [Option<ParkedFailure>; 2],
}

fn push_newest(slots: &mut [Option<ParkedFailure>; 2], failure: ParkedFailure) {
    slots[1] = slots[0];
    slots[0] = Some(failure);
}

impl Newest {
    pub(crate) fn add(&mut self, failure: ParkedFailure, carried: bool) {
        if carried {
            push_newest(&mut self.carried, failure);
        } else {
            push_newest(&mut self.live, failure);
        }
    }

    pub(crate) fn clear_live(&mut self) {
        self.live = [None; 2];
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.live.iter().chain(&self.carried).all(Option::is_none)
    }

    pub(crate) fn newest_two(&self, before: Seq) -> [Option<ParkedFailure>; 2] {
        let mut all: Vec<ParkedFailure> = self
            .live
            .iter()
            .chain(&self.carried)
            .flatten()
            .filter(|failure| failure.seq < before)
            .copied()
            .collect();
        all.sort_by_key(|failure| std::cmp::Reverse(failure.seq));
        [all.first().copied(), all.get(1).copied()]
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Hunts {
    pub(crate) active: Option<Box<Active>>,
    pub(crate) next_id: u32,
    pub(crate) next_combination: u32,
    pub(crate) last_start: Option<Seq>,
    pub(crate) reset: Option<Seq>,
    pub(crate) located: BTreeMap<Seq, Located>,
    pub(crate) passed_full: Vec<(Seq, Profile)>,
    pub(crate) parked_failures: HashMap<ClassKey, Newest>,
    pub(crate) rejecting: Option<Box<Rejecting>>,
}

impl Hunts {
    pub(crate) fn passed_full_cycle(&mut self, seq: Seq, profile: Profile) {
        self.passed_full.retain(|(_, known)| *known != profile);
        self.passed_full.push((seq, profile));
    }

    pub(crate) fn passed_newest_first(&self) -> impl Iterator<Item = (Seq, &Profile)> {
        self.passed_full
            .iter()
            .rev()
            .map(|(seq, profile)| (*seq, profile))
    }

    pub(crate) fn note_parked_failure(
        &mut self,
        class: ClassKey,
        failure: ParkedFailure,
        carried: bool,
    ) {
        self.parked_failures
            .entry(class)
            .or_default()
            .add(failure, carried);
    }

    pub(crate) fn reset_core(&mut self, seq: Seq, core: CoreId) {
        self.reset = Some(seq);
        self.parked_failures.retain(|class, newest| {
            newest.clear_live();
            if class.loaded().contains(core) {
                return false;
            }
            !newest.is_empty()
        });
    }

    pub(crate) fn active_located(&self) -> bool {
        self.active.as_ref().is_some_and(|hunt| hunt.located)
    }

    /// The failure a hunt commitment answers and the direct failure that ended it, for the event
    /// that cites `first` as its first cause, while the hunt is still open.
    pub(crate) fn commitment_source(&self, first: Seq) -> Option<(Seq, Option<Seq>)> {
        let active = self.active.as_ref()?;
        let end = active.end.as_ref()?;
        let cites = first == end.seq || end.combination.as_ref().is_some_and(|c| c.seq == first);
        cites.then_some((active.start.failure, end.direct_failure))
    }

    pub(crate) fn is_located(&self, failure: Seq) -> bool {
        self.located.contains_key(&failure)
    }
}
