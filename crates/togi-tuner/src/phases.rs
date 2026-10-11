use std::num::NonZeroU32;

use togi_journal::{CorePhase, Seq};
use togi_machine::{CoreId, CoreSet};

use crate::state::Run;
use crate::{Depth, FoldError, Profile};

#[derive(Clone, Debug, Eq, PartialEq)]
struct Settled {
    phase1_end: Seq,
    first_confirmed: Profile,
    passed: Profile,
    finished: CoreSet,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Stage {
    One,
    Rounds(Settled),
    Confirming {
        settled: Settled,
        start: Seq,
    },
    Concluded {
        settled: Settled,
        seq: Seq,
        clean: u32,
        last_cycle: NonZeroU32,
    },
}

/// The two-phase method's lifecycle. Phase 1 ends at the first passed full cycle after every core
/// left search; phase 2 is the rounds after it and the confirmation cycle that concludes it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Phases {
    stage: Stage,
}

impl Default for Phases {
    fn default() -> Self {
        Self { stage: Stage::One }
    }
}

impl Phases {
    pub(crate) fn reset(&mut self) {
        self.stage = Stage::One;
    }

    fn settled(&self) -> Option<&Settled> {
        match &self.stage {
            Stage::One => None,
            Stage::Rounds(settled)
            | Stage::Confirming { settled, .. }
            | Stage::Concluded { settled, .. } => Some(settled),
        }
    }

    pub(crate) fn phase1_end(&self) -> Option<Seq> {
        self.settled().map(|settled| settled.phase1_end)
    }

    pub(crate) fn first_confirmed(&self) -> Option<&Profile> {
        self.settled().map(|settled| &settled.first_confirmed)
    }

    pub(crate) fn passed(&self) -> Option<&Profile> {
        self.settled().map(|settled| &settled.passed)
    }

    pub(crate) fn finished(&self) -> CoreSet {
        self.settled()
            .map_or_else(CoreSet::default, |settled| settled.finished)
    }

    pub(crate) fn confirm_start(&self) -> Option<Seq> {
        match &self.stage {
            Stage::Confirming { start, .. } => Some(*start),
            Stage::Concluded { .. } | Stage::One | Stage::Rounds(_) => None,
        }
    }

    pub(crate) fn concluded(&self) -> Option<Seq> {
        match &self.stage {
            Stage::Concluded { seq, .. } => Some(*seq),
            Stage::One | Stage::Rounds(_) | Stage::Confirming { .. } => None,
        }
    }

    /// Passed full cycles from the conclusion on, the concluding cycle included.
    pub(crate) fn clean_cycles(&self) -> u32 {
        match &self.stage {
            Stage::Concluded { clean, .. } => *clean,
            Stage::One | Stage::Rounds(_) | Stage::Confirming { .. } => 0,
        }
    }

    pub(crate) fn cycle_start(&mut self, seq: Seq) {
        if let Stage::Rounds(settled) = &self.stage {
            self.stage = Stage::Confirming {
                settled: settled.clone(),
                start: seq,
            };
        }
    }

    /// A passed full cycle ended: the first after every core left search ends phase 1.
    pub(crate) fn passed_full_cycle(
        &mut self,
        seq: Seq,
        cycle: NonZeroU32,
        searching: bool,
        checking: Option<Profile>,
    ) -> Result<(), FoldError> {
        match &mut self.stage {
            Stage::One if !searching => {
                let profile = checking.ok_or(FoldError::NoCheckingProfile)?;
                self.stage = Stage::Rounds(Settled {
                    phase1_end: seq,
                    first_confirmed: profile,
                    passed: profile,
                    finished: CoreSet::default(),
                });
            }
            Stage::One | Stage::Rounds(_) => {}
            Stage::Confirming { settled, .. } => {
                self.stage = Stage::Concluded {
                    settled: settled.clone(),
                    seq,
                    clean: 1,
                    last_cycle: cycle,
                };
            }
            Stage::Concluded {
                clean, last_cycle, ..
            } => {
                *clean += 1;
                *last_cycle = cycle;
            }
        }
        Ok(())
    }
}

/// The profile one round moves to, the cores it moves one count toward their solo limits, and the
/// cores that kept an unverified offset and are checked again in place.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Moves {
    pub(crate) profile: Profile,
    pub(crate) moved: Vec<CoreId>,
    pub(crate) carried: Vec<CoreId>,
}

impl Run {
    /// One round's moves from `from`, given each core's last passed offset. A core that finished
    /// is not moved again; one deeper than it last passed is checked in place; one with no solo
    /// limit or already at it has nothing to move toward; the rest move one count if the profile
    /// so far reaches no failure point or combination.
    pub(crate) fn phase2_moves(&self, from: &Profile, passed: Option<&Profile>) -> Moves {
        let finished = self.phases.finished();
        let mut profile = *from;
        let mut moved = Vec::new();
        let mut carried = Vec::new();
        for &id in self.topo.order() {
            let core = self.core(id);
            let Some(offset) = from.get(id) else {
                continue;
            };
            if core.phase.is_none_or(|phase| phase == CorePhase::Search) || finished.contains(id) {
                continue;
            }
            if passed
                .and_then(|passed| passed.get(id))
                .is_some_and(|last| offset.deeper_than(last))
            {
                carried.push(id);
                continue;
            }
            if !core
                .solo_limit
                .is_some_and(|solo| offset.shallower_than(solo))
            {
                continue;
            }
            let Some(deeper) = offset.deeper_by(1) else {
                continue;
            };
            let trial = profile.with(id, deeper);
            if self.constraints.reaches(&trial) == Ok(None) {
                profile = trial;
                moved.push(id);
            }
        }
        Moves {
            profile,
            moved,
            carried,
        }
    }

    pub(crate) fn any_searching(&self) -> bool {
        self.topo
            .order()
            .iter()
            .any(|&id| self.core(id).phase == Some(CorePhase::Search))
    }

    /// Whether phase 2 owes a round: phase 1 ended, no confirmation began, nothing is searching or
    /// queued for a hunt, and some core can move or must be checked again in place.
    pub(crate) fn round_due(&self) -> bool {
        if self.phases.phase1_end().is_none()
            || self.phases.confirm_start().is_some()
            || self.phases.concluded().is_some()
            || !self.failures.queue_is_empty()
            || self.any_searching()
        {
            return false;
        }
        let moves = self.phase2_moves(&self.offsets(), self.phases.passed());
        !moves.moved.is_empty() || !moves.carried.is_empty()
    }

    pub(crate) fn checking_cycle(
        &mut self,
        event: &togi_journal::Event,
        cycle: &togi_journal::CheckingCycle,
    ) -> Result<(), FoldError> {
        use togi_journal::CycleEvent;
        match cycle.event {
            CycleEvent::Start => {
                self.phases.cycle_start(event.seq);
                Ok(())
            }
            CycleEvent::End if cycle.passed && cycle.full => {
                let searching = self.any_searching();
                self.phases.passed_full_cycle(
                    event.seq,
                    cycle.cycle,
                    searching,
                    self.checking.profile(),
                )
            }
            CycleEvent::End => Ok(()),
        }
    }
}
