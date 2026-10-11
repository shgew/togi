use togi_journal::{HuntStage, Member, Seq};
use togi_machine::CoreSet;

use super::plan::GroupOutcome;
use crate::Profile;
use crate::state::Run;

/// One group of the active hunt as a dashboard shows it, its outcome read from the answer the
/// hunt froze when it moved on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GroupView {
    pub(crate) number: u32,
    pub(crate) seq: Seq,
    pub(crate) cores: CoreSet,
    pub(crate) profile: Profile,
    pub(crate) stage: HuntStage,
    pub(crate) probe: Option<Member>,
    pub(crate) held: Vec<Member>,
    pub(crate) outcome: Option<GroupOutcome>,
    pub(crate) needed: u16,
    pub(crate) passed: u32,
    pub(crate) carried: bool,
}

impl Run {
    pub(crate) fn hunt_groups(&self) -> Vec<GroupView> {
        let Some(hunt) = self.hunts.active.as_ref() else {
            return Vec::new();
        };
        hunt.groups
            .iter()
            .enumerate()
            .map(|(index, group)| {
                let answer = if hunt.settled(index) {
                    group.closing.clone().unwrap_or_default()
                } else {
                    self.answer_of(hunt, group)
                };
                let own = if group.payload.inferred.is_some() {
                    group.own.clone().unwrap_or_default()
                } else {
                    answer.clone()
                };
                let outcome = if group.payload.skipped {
                    Some(GroupOutcome::Skipped)
                } else {
                    answer.outcome
                };
                GroupView {
                    number: group.number(),
                    seq: group.seq,
                    cores: group.payload.cores,
                    profile: group.profile,
                    stage: group.payload.stage,
                    probe: group.payload.probe,
                    held: group.payload.held.clone(),
                    outcome,
                    needed: hunt.needed.get(),
                    passed: if hunt.settled(index) {
                        answer.passes
                    } else {
                        self.passes_of(hunt, group)
                    },
                    carried: own.is_carried(|seq| self.is_carried(seq)),
                }
            })
            .collect()
    }

    pub(crate) fn hunt_cut(&self) -> u32 {
        self.hunts.active.as_ref().map_or(0, |hunt| hunt.cut.count)
    }
}
