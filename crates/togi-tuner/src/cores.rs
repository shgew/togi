use togi_journal::{CorePhase, CoreTransition, Event, Seq, TunerDecision};
use togi_machine::{CoreId, CoreSet, ObservedOffset, Regime, TargetOffset, WorkloadId};

use crate::state::Run;
use crate::{CombinationId, FailurePoint, FoldError};

/// The R1 and R2 workloads a candidate solo limit froze.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Check {
    pub(crate) r1: WorkloadId,
    pub(crate) r2: WorkloadId,
}

impl Check {
    pub(crate) fn recorded(
        core: CoreId,
        checking: bool,
        workloads: &[WorkloadId],
    ) -> Result<Option<Self>, FoldError> {
        if !checking {
            return Ok(None);
        }
        match *workloads {
            [r1, r2] if r1.regime() == Regime::R1 && r2.regime() == Regime::R2 => {
                Ok(Some(Self { r1, r2 }))
            }
            _ => Err(FoldError::BadCandidate(core)),
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
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Rotation([u32; 7]);

impl Rotation {
    pub(crate) fn get(self, regime: Regime) -> usize {
        usize::try_from(self.0[slot(regime)]).unwrap_or(usize::MAX)
    }

    pub(crate) fn advance(&mut self, regime: Regime) {
        let count = &mut self.0[slot(regime)];
        *count = count.wrapping_add(1);
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Core {
    pub(crate) phase: Option<CorePhase>,
    pub(crate) offset: TargetOffset,
    pub(crate) baseline: ObservedOffset,
    pub(crate) pass: Option<TargetOffset>,
    pub(crate) solo_limit: Option<TargetOffset>,
    pub(crate) check: Option<Check>,
    pub(crate) checks: u32,
    pub(crate) phase_seq: Option<Seq>,
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
            .is_some_and(|retry| retry.turn.is_some_and(|turn| turn.core == core))
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
        let check = Check::recorded(core, transition.check_solo_limit, &transition.workloads)?;
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
        let seq = event.seq;
        let before = self.core(core).offset;
        let entering_search = transition.to == CorePhase::Search;
        self.update_core(core, |state| {
            if transition.from == Some(CorePhase::Search)
                && !entering_search
                && transition.pass.is_some()
            {
                state.solo_limit = transition.pass;
            }
            if entering_search {
                state.solo_limit = None;
            }
            state.phase = Some(transition.to);
            state.offset = transition.offset;
            state.pass = transition.pass;
            state.check = check;
            state.checks += u32::from(check.is_some());
            state.phase_seq = Some(seq);
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
        )?;
        self.constraints
            .set_failure_point(core, decision.failure_point.map(FailurePoint::new))?;
        let seq = event.seq;
        let before = self.core(core).offset;
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
            state.check = check;
            if check.is_some() {
                state.checks += 1;
                state.phase_seq = Some(seq);
            }
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
