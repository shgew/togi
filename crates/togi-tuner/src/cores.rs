use togi_journal::{CorePhase, CoreTransition, Event, Seq, TunerDecision};
use togi_machine::{CoreId, CoreSet, ObservedOffset, Regime, TargetOffset, WorkloadId};

use crate::state::Run;
use crate::{CombinationId, FailurePoint, FoldError};

/// The R1 and R2 workloads a candidate solo limit froze, and the event that began its check.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Check {
    pub(crate) r1: WorkloadId,
    pub(crate) r2: WorkloadId,
    pub(crate) since: Seq,
}

impl Check {
    pub(crate) fn recorded(
        core: CoreId,
        checking: bool,
        workloads: &[WorkloadId],
        since: Seq,
    ) -> Result<Option<Self>, FoldError> {
        if !checking {
            return Ok(None);
        }
        match *workloads {
            [r1, r2] if r1.regime() == Regime::R1 && r2.regime() == Regime::R2 => {
                Ok(Some(Self { r1, r2, since }))
            }
            _ => Err(FoldError::BadCandidate(core)),
        }
    }
}

/// Where a core stands in the search, with what only that phase has: a searching core's
/// candidate check, and the solo limit a core that left the search with a pass keeps.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) enum Phase {
    #[default]
    Unstarted,
    Search {
        check: Option<Check>,
    },
    HasRoom {
        solo_limit: Option<TargetOffset>,
    },
    AtLimit {
        solo_limit: Option<TargetOffset>,
    },
}

impl Phase {
    pub(crate) const fn wire(self) -> Option<CorePhase> {
        match self {
            Self::Unstarted => None,
            Self::Search { .. } => Some(CorePhase::Search),
            Self::HasRoom { .. } => Some(CorePhase::HasRoom),
            Self::AtLimit { .. } => Some(CorePhase::AtLimit),
        }
    }

    pub(crate) const fn searching(self) -> bool {
        matches!(self, Self::Search { .. })
    }

    pub(crate) const fn check(self) -> Option<Check> {
        match self {
            Self::Search { check } => check,
            Self::Unstarted | Self::HasRoom { .. } | Self::AtLimit { .. } => None,
        }
    }

    pub(crate) const fn solo_limit(self) -> Option<TargetOffset> {
        match self {
            Self::HasRoom { solo_limit } | Self::AtLimit { solo_limit } => solo_limit,
            Self::Unstarted | Self::Search { .. } => None,
        }
    }
}

const fn slot(regime: Regime) -> usize {
    match regime {
        Regime::R1 => 0,
        Regime::R2 => 1,
        Regime::R3 => 2,
        Regime::R4 => 3,
        Regime::R5 => 4,
        Regime::R6 => 5,
        Regime::R7 => 6,
    }
}

/// How many conclusive trials of each regime have ended; the next workload of the regime's list
/// is the one at this count.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Rotation([u32; 7]);

impl Rotation {
    pub(crate) fn get(self, regime: Regime) -> usize {
        crate::numbers::index(self.0[slot(regime)])
    }

    pub(crate) fn advance(&mut self, regime: Regime) {
        let count = &mut self.0[slot(regime)];
        *count = count.wrapping_add(1);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Core {
    pub(crate) phase: Phase,
    pub(crate) offset: TargetOffset,
    pub(crate) baseline: ObservedOffset,
    pub(crate) pass: Option<TargetOffset>,
    pub(crate) checks: u32,
    pub(crate) queued_reset: Option<Seq>,
    pub(crate) pending: Option<Seq>,
    pub(crate) zero_seq: Option<Seq>,
    pub(crate) last_decision: Option<Seq>,
    pub(crate) last_seq: Option<Seq>,
    pub(crate) rotation: Rotation,
}

impl Run {
    pub(crate) fn core(&self, core: CoreId) -> Core {
        self.cores.get(core).unwrap_or_default()
    }

    pub(crate) fn update_core(&mut self, id: CoreId, change: impl FnOnce(&mut Core)) {
        if let Some(mut core) = self.cores.get(id) {
            change(&mut core);
            self.cores.set(id, core);
        }
    }

    pub(crate) fn require_cores(&self, cores: CoreSet) -> Result<(), FoldError> {
        cores.iter().try_for_each(|core| self.require_core(core))
    }

    pub(crate) fn require_core(&self, core: CoreId) -> Result<(), FoldError> {
        if self.topo.contains(core) {
            Ok(())
        } else {
            Err(FoldError::UnknownCore(core))
        }
    }

    pub(crate) fn failure_point(&self, core: CoreId) -> Option<TargetOffset> {
        self.constraints
            .failure_points()
            .get(core)
            .map(FailurePoint::offset)
    }

    /// A decision answered the core: its step and pending failure are settled, and a saved alone
    /// retry of it is stale.
    pub(crate) fn retire_pending(&mut self, core: CoreId) {
        if let Some(seq) = self.core(core).pending {
            self.failures.retire(seq);
        }
    }

    fn decided(&mut self, core: CoreId, seq: Seq, answers_pending: bool) {
        self.search.clear(core);
        if answers_pending {
            self.retire_pending(core);
        }
        self.update_core(core, |state| {
            if answers_pending {
                state.pending = None;
            }
            state.last_seq = Some(seq);
            state.last_decision = Some(seq);
        });
        if self
            .retry
            .as_ref()
            .is_some_and(|retry| retry.turn().is_some_and(|turn| turn.core == core))
        {
            self.retry = None;
        }
    }

    pub(crate) fn core_phase(
        &mut self,
        event: &Event,
        transition: &CoreTransition,
    ) -> Result<(), FoldError> {
        let core = transition.core;
        self.require_core(core)?;
        let seq = event.seq;
        let check = Check::recorded(
            core,
            transition.check_solo_limit,
            &transition.workloads,
            seq,
        )?;
        self.constraints
            .set_failure_point(core, transition.failure_point.map(FailurePoint::new))?;
        self.hunt_phase_reset(core);
        let cleared: Vec<CombinationId> = transition
            .cleared_combination
            .iter()
            .copied()
            .map(CombinationId::new)
            .collect();
        self.constraints.remove_combinations(&cleared);
        let before = self.core(core).offset;
        self.r7_offset_changed(core, before, transition.offset);
        let entering_search = transition.to == CorePhase::Search;
        self.update_core(core, |state| {
            let solo_limit = match transition.pass {
                Some(pass) if transition.from == Some(CorePhase::Search) => Some(pass),
                Some(_) | None => state.phase.solo_limit(),
            };
            state.phase = match transition.to {
                CorePhase::Search => Phase::Search { check },
                CorePhase::HasRoom => Phase::HasRoom { solo_limit },
                CorePhase::AtLimit => Phase::AtLimit { solo_limit },
            };
            state.offset = transition.offset;
            state.pass = transition.pass;
            state.checks += u32::from(check.is_some());
            state.queued_reset = None;
            if transition.failure_point == Some(TargetOffset::ZERO) {
                state.zero_seq = Some(seq);
            }
        });
        if entering_search {
            self.phases.reset();
            self.later.reset();
            self.rounds_reset();
        }
        self.failures.retire_all(&event.cause);
        self.prune_blames();
        self.decided(core, seq, true);
        self.later_moved(core, seq, before);
        Ok(())
    }

    pub(crate) fn tuner_decision(
        &mut self,
        event: &Event,
        decision: &TunerDecision,
    ) -> Result<(), FoldError> {
        let core = decision.core;
        self.require_core(core)?;
        let check = Check::recorded(
            core,
            decision.decision == togi_journal::Decision::CheckSoloLimit,
            &decision.workloads,
            event.seq,
        )?;
        self.constraints
            .set_failure_point(core, decision.failure_point.map(FailurePoint::new))?;
        let seq = event.seq;
        let before = self.core(core).offset;
        self.r7_offset_changed(core, before, decision.to_offset);
        let hold = self.hold_strike(event, decision);
        self.r7_decided(event, decision);
        let yielded = self.phase2_yield(decision);
        if decision.decision == togi_journal::Decision::Backoff
            && decision.activity != togi_journal::Activity::Search
            && decision.to_offset != decision.from_offset
        {
            self.queue_rerun(&event.cause);
        }
        self.hunt_decided(event, decision);
        self.update_core(core, |state| {
            state.offset = decision.to_offset;
            state.pass = decision.pass;
            if let Phase::Search { check: current } = &mut state.phase {
                *current = check;
            }
            state.checks += u32::from(check.is_some());
            if decision.failure_point == Some(TargetOffset::ZERO) {
                state.zero_seq = Some(seq);
            }
        });
        if !yielded {
            self.failures.retire_all(&event.cause);
        }
        self.decided(core, seq, !yielded);
        self.phase2_backoff(decision, &event.cause);
        self.prune_blames();
        self.later_decided(event, decision, before, hold.as_ref());
        self.retire_reruns();
        self.revalidate_retry();
        Ok(())
    }
}
