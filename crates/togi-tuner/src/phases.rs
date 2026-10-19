use std::collections::VecDeque;
use std::num::NonZeroU32;

use togi_journal::Seq;
use togi_machine::{CoreId, PerCore, TargetOffset};

use crate::cores::Phase;
use crate::rounds::{Blame, Shape};
use crate::state::Run;
use crate::{Depth, FoldError, Profile};

/// The origin of a core's unverified offset, retained when a yield retries that same offset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Mark {
    pub(crate) round: NonZeroU32,
    pub(crate) seq: Seq,
    pub(crate) offset: TargetOffset,
    pub(crate) attempt: NonZeroU32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Settled {
    phase1_end: Seq,
    first_confirmed: Profile,
    passed: Profile,
    moved: PerCore<Option<Mark>>,
    blames: VecDeque<Blame>,
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

    fn settled_mut(&mut self) -> Option<&mut Settled> {
        match &mut self.stage {
            Stage::One => None,
            Stage::Rounds(settled)
            | Stage::Confirming { settled, .. }
            | Stage::Concluded { settled, .. } => Some(settled),
        }
    }

    /// Phase 1 ended and phase 2 has not concluded.
    pub(crate) fn in_phase2(&self) -> bool {
        matches!(self.stage, Stage::Rounds(_) | Stage::Confirming { .. })
    }

    pub(crate) fn mark(&self, core: CoreId) -> Option<Mark> {
        self.settled()
            .and_then(|settled| settled.moved.get(core).flatten())
    }

    /// The oldest phase-2 failure still owed a blame.
    pub(crate) fn blame(&self) -> Option<&Blame> {
        self.settled().and_then(|settled| settled.blames.front())
    }

    pub(crate) fn push_blame(&mut self, blame: Blame) {
        if let Some(settled) = self.settled_mut() {
            settled.blames.push_back(blame);
        }
    }

    /// Keeps the blames whose failure is still unresolved.
    pub(crate) fn prune_blames(&mut self, unresolved: impl Fn(Seq) -> bool) {
        if let Some(settled) = self.settled_mut() {
            settled.blames.retain(|blame| unresolved(blame.failure()));
        }
    }

    /// A round began from `offsets`: a core its profile deepens, still standing at the round's
    /// baseline, is marked as moved by this round.
    pub(crate) fn round_started(&mut self, round: &Shape, offsets: &Profile) {
        let Some(settled) = self.settled_mut() else {
            return;
        };
        for core in round.cores() {
            if round.deepened(core) && offsets.get(core) == round.baseline().get(core) {
                let Some(offset) = round.profile().get(core) else {
                    continue;
                };
                match settled.moved.get_mut(core).and_then(Option::as_mut) {
                    Some(mark) if mark.offset == offset => mark.attempt = round.id(),
                    _ => {
                        settled.moved.set(
                            core,
                            Some(Mark {
                                round: round.id(),
                                seq: round.start(),
                                offset,
                                attempt: round.id(),
                            }),
                        );
                    }
                }
            }
        }
    }

    /// A round closed: a passed one verified its profile for its cores.
    pub(crate) fn round_ended(&mut self, round: &Shape, passed: bool) {
        let Some(settled) = self.settled_mut().filter(|_| passed) else {
            return;
        };
        for core in round.cores() {
            if let Some(offset) = round.profile().get(core) {
                settled.passed = settled.passed.with(core, offset);
            }
            settled.moved.set(core, None);
        }
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

    /// The most recent clean cycle's number; none before the conclusion.
    pub(crate) fn last_clean_cycle(&self) -> Option<NonZeroU32> {
        match &self.stage {
            Stage::Concluded { last_cycle, .. } => Some(*last_cycle),
            Stage::One | Stage::Rounds(_) | Stage::Confirming { .. } => None,
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
                    moved: PerCore::default(),
                    blames: VecDeque::new(),
                });
            }
            Stage::One | Stage::Rounds(_) => {}
            Stage::Confirming { settled, .. } => {
                let mut settled = settled.clone();
                settled.blames.clear();
                self.stage = Stage::Concluded {
                    settled,
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
    /// One round's moves from `from`, given each core's last passed offset. A core deeper than
    /// it last passed is checked in place; one with no solo limit or already at it has nothing
    /// to move toward; the rest move one count if the profile so far reaches no constraint.
    pub(crate) fn phase2_moves(&self, from: &Profile, passed: Option<&Profile>) -> Option<Moves> {
        let mut profile = *from;
        let mut moved = Vec::new();
        let mut carried = Vec::new();
        for &id in self.topo.order() {
            let core = self.core(id);
            let offset = from.get(id)?;
            if matches!(core.phase, Phase::Unstarted | Phase::Search { .. }) {
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
                .phase
                .solo_limit()
                .is_some_and(|solo| offset.shallower_than(solo))
            {
                continue;
            }
            let trial = profile.with(id, offset.deeper_or_floor(1));
            if self.constraints.reaches(&trial) == Ok(None) {
                profile = trial;
                moved.push(id);
            }
        }
        Some(Moves {
            profile,
            moved,
            carried,
        })
    }

    pub(crate) fn any_searching(&self) -> bool {
        self.topo
            .order()
            .iter()
            .any(|&id| self.core(id).phase.searching())
    }

    /// Whether a hunt is open. Phase 2 owes no round while one runs.
    pub(crate) const fn hunt_open(&self) -> bool {
        self.hunts.active.is_some()
    }

    /// Whether phase 2 owes a round: phase 1 ended, no confirmation began, no cycle is open,
    /// nothing is searching, queued for a hunt or owed a rerun, and some core can move or must be
    /// checked again in place.
    pub(crate) fn round_due(&self) -> bool {
        if self.phases.phase1_end().is_none()
            || self.phases.confirm_start().is_some()
            || self.phases.concluded().is_some()
            || self.checking.open().is_some()
            || self.rounds.active().is_some()
            || self.hunt_open()
            || !self.failures.queue_is_empty()
            || !self.reruns.is_empty()
            || self.any_searching()
        {
            return false;
        }
        self.phase2_moves(&self.offsets(), self.phases.passed())
            .is_some_and(|moves| !moves.moved.is_empty() || !moves.carried.is_empty())
    }
}
