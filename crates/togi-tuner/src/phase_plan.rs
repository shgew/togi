use serde::{Deserialize, Serialize};
use togi_journal::Seq;
use togi_machine::{CoreId, CoreSet, MIN_OFFSET, PerCore, TargetOffset};

pub use crate::later::bios::{BiosProfile, Confirmed, Unconfirmed};
use crate::rounds::Shape;
use crate::state::Run;
use crate::{Profile, State};

pub(crate) const ROUND_CAP: i64 = 2 - MIN_OFFSET;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PhaseNo {
    One,
    Two,
    Concluded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub core: CoreId,
    pub offset: TargetOffset,
    pub solo_limit: Option<TargetOffset>,
    pub gap: u32,
    pub carried: bool,
    pub moving: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PhasePlan {
    pub phase: PhaseNo,
    pub phase1_end: Option<Seq>,
    pub concluded: Option<Seq>,
    pub confirming: bool,
    pub rounds_left: u32,
    pub round: Option<u32>,
    pub candidates: Vec<Candidate>,
}

#[derive(Debug, Default)]
pub(crate) struct Outlook {
    pub(crate) rounds: u32,
    open: Option<Shape>,
    gaps: PerCore<u32>,
    carried: CoreSet,
    moving: CoreSet,
    #[cfg(test)]
    pub(crate) iterations: u32,
}

impl Run {
    pub(crate) fn outlook(&self) -> Outlook {
        let mut outlook = Outlook::default();
        let mut from = self.offsets();
        let mut passed = self.phases.passed().copied();
        if let Some(round) = self.rounds.active() {
            let shape = round.shape();
            outlook.rounds = 1;
            outlook.open = Some(shape);
            for core in shape.cores() {
                if self
                    .phases
                    .mark(core)
                    .is_some_and(|mark| mark.round == shape.id())
                {
                    outlook
                        .gaps
                        .set(core, outlook.gaps.get(core).unwrap_or(0) + 1);
                    outlook.moving.insert(core);
                } else {
                    outlook.carried.insert(core);
                }
            }
            from = *shape.profile();
            passed = Some(from);
        }
        for _ in 0..ROUND_CAP {
            #[cfg(test)]
            {
                outlook.iterations += 1;
            }
            let Some(moves) = self.phase2_moves(&from, passed.as_ref()) else {
                break;
            };
            if moves.moved.is_empty() && moves.carried.is_empty() {
                break;
            }
            outlook.rounds += 1;
            for core in moves.moved {
                outlook
                    .gaps
                    .set(core, outlook.gaps.get(core).unwrap_or(0) + 1);
            }
            outlook.carried.extend(moves.carried);
            from = moves.profile;
            passed = Some(from);
        }
        outlook
    }

    pub(crate) fn phase_plan(&self) -> PhasePlan {
        let mut plan = PhasePlan {
            phase: PhaseNo::Two,
            phase1_end: self.phases.phase1_end(),
            concluded: self.phases.concluded(),
            confirming: false,
            rounds_left: 0,
            round: None,
            candidates: Vec::new(),
        };
        if plan.phase1_end.is_none() {
            plan.phase = PhaseNo::One;
            return plan;
        }
        if plan.concluded.is_some() {
            plan.phase = PhaseNo::Concluded;
            return plan;
        }
        if self.phases.confirm_start().is_some() {
            plan.confirming = true;
            return plan;
        }
        let outlook = self.outlook();
        plan.rounds_left = outlook.rounds;
        plan.confirming = outlook.rounds == 0;
        plan.round = outlook.open.map(|shape| shape.id().get());
        plan.candidates = self
            .topo
            .all()
            .iter()
            .filter_map(|id| {
                let gap = outlook.gaps.get(id).unwrap_or(0);
                let carried = outlook.carried.contains(id);
                if gap == 0 && !carried {
                    return None;
                }
                let moving = outlook.moving.contains(id);
                let core = self.core(id);
                let offset = match outlook.open.filter(|_| moving) {
                    Some(shape) => shape.baseline().get(id)?,
                    None => core.offset,
                };
                Some(Candidate {
                    core: id,
                    offset,
                    solo_limit: core.solo_limit,
                    gap,
                    carried,
                    moving,
                })
            })
            .collect();
        plan
    }
}

fn standing(state: &State) -> Option<&Run> {
    if state.poisoned() { None } else { state.run() }
}

impl State {
    pub fn phase_plan(&self) -> Option<PhasePlan> {
        standing(self).map(Run::phase_plan)
    }

    pub fn profile(&self) -> Option<Profile> {
        standing(self)?.checking.profile()
    }

    pub fn profile_seq(&self) -> Option<Seq> {
        standing(self)?.checking.seq()
    }

    pub fn clean_cycles(&self) -> u32 {
        standing(self).map_or(0, |run| run.phases.clean_cycles())
    }

    pub fn first_result(&self) -> bool {
        standing(self).is_some_and(Run::first_result)
    }

    pub fn bios_profile(&self) -> Option<BiosProfile> {
        standing(self).map(Run::bios_profile)
    }
}
