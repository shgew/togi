use togi_journal::{
    CommandReset, ConfigLoaded, DeadEnd, DeadEndCondition, Event, Payload, ProfileChange,
    ResetTarget, Seq, SessionBaseline, SessionStart,
};
use togi_machine::{Backend, PerCore, TargetOffset};

use crate::checking::{Checking, Reruns};
use crate::config::TunerConfig;
use crate::cores::{Core, Rotation};
use crate::failures::Failures;
use crate::flight::Flight;
use crate::hunt::Hunts;
use crate::later::Later;
use crate::ledger::PendingWarning;
use crate::phases::Phases;
use crate::r7::state::R7;
use crate::requirement::Retry;
use crate::rounds::Rounds;
use crate::search::Search;
use crate::zero::Zero;
use crate::{
    CombinationId, Constraints, Evidence, FailurePoint, FoldError, Profile, Ranking, Topology,
};

/// The latest host ranking and the event that recorded it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Ranked {
    pub(crate) ranking: Ranking,
    pub(crate) seq: Seq,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Run {
    pub(crate) topo: Topology,
    pub(crate) cores: PerCore<Core>,
    pub(crate) all_rotation: Rotation,
    pub(crate) search: Search,
    pub(crate) constraints: Constraints,
    pub(crate) phases: Phases,
    pub(crate) config: TunerConfig,
    pub(crate) flight: Flight,
    pub(crate) evidence: Evidence,
    pub(crate) retry: Option<Retry>,
    pub(crate) warning: Option<PendingWarning>,
    pub(crate) failures: Failures,
    pub(crate) r7: R7,
    pub(crate) zero: Zero,
    pub(crate) hunts: Hunts,
    pub(crate) ranking: Option<Ranked>,
    pub(crate) last_plan_seq: Option<Seq>,
    pub(crate) rounds: Rounds,
    pub(crate) checking: Checking,
    pub(crate) reruns: Reruns,
    pub(crate) later: Later,
    pub(crate) last_seq: Seq,
    pub(crate) stopped: bool,
}

/// The tuner's state: one fold of the journal, with a substate per rule group. `fold` takes each
/// event once; the decision functions only read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    run: Option<Run>,
    poisoned: bool,
}

impl Run {
    fn start(event: &Event, start: &SessionStart) -> Result<Self, FoldError> {
        let topo = Topology::new(&start.cores)?;
        let cores = topo
            .all()
            .iter()
            .map(|core| (core, Core::default()))
            .collect();
        let mut evidence = Evidence::new(topo.all());
        let config = TunerConfig::initial();
        for backend in [Backend::Mprime, Backend::Ycruncher] {
            evidence.set_backend(backend, config.backend_path(backend));
        }
        Ok(Self {
            constraints: Constraints::new(&topo),
            topo,
            cores,
            all_rotation: Rotation::default(),
            search: Search::default(),
            phases: Phases::default(),
            config,
            flight: Flight::default(),
            evidence,
            retry: None,
            warning: None,
            failures: Failures::default(),
            r7: R7::default(),
            zero: Zero::default(),
            hunts: Hunts::default(),
            ranking: None,
            last_plan_seq: None,
            rounds: Rounds::default(),
            checking: Checking::default(),
            reruns: Reruns::default(),
            later: Later::default(),
            last_seq: event.seq,
            stopped: false,
        })
    }

    #[cfg(test)]
    pub(crate) const fn topo_ref(&self) -> &Topology {
        &self.topo
    }

    fn config_loaded(&mut self, loaded: &ConfigLoaded) -> Result<(), FoldError> {
        let config = TunerConfig::from_loaded(loaded)?;
        self.stopped = false;
        let new_schedule = !self.config.same_schedule(&config);
        self.config = config;
        let mut switched = Vec::new();
        for backend in [Backend::Mprime, Backend::Ycruncher] {
            if self
                .evidence
                .set_backend(backend, self.config.backend_path(backend))
            {
                switched.push(backend);
            }
        }
        if new_schedule {
            self.replan(None);
        } else {
            self.repin();
        }
        if !switched.is_empty() {
            self.refresh_cycle(|class| switched.contains(&class.workload().backend()));
        }
        self.retire_reruns();
        self.revalidate_retry();
        Ok(())
    }

    fn baseline(&mut self, baseline: &SessionBaseline) -> Result<(), FoldError> {
        if let Some((core, _)) = baseline
            .offsets
            .iter()
            .find(|&(core, _)| !self.topo.contains(core))
        {
            return Err(FoldError::UnknownCore(core));
        }
        for (id, offset) in baseline.offsets.iter() {
            self.update_core(id, |core| core.baseline = offset);
        }
        Ok(())
    }

    fn command_reset(&mut self, event: &Event, reset: CommandReset) -> Result<(), FoldError> {
        let ResetTarget::Core(core) = reset.target else {
            return Ok(());
        };
        self.require_core(core)?;
        self.evidence.reset_core(core);
        self.failures.forget_core(core);
        self.prune_blames();
        self.r7_reset(core);
        self.hunt_reset(event.seq, core);
        self.update_core(core, |state| state.queued_reset = Some(event.seq));
        self.retry = None;
        self.reruns.consume_citations();
        self.cycle_evidence_reset();
        Ok(())
    }

    fn dead_end(&mut self, event: &Event, dead_end: &DeadEnd) -> Result<(), FoldError> {
        self.stopped = true;
        self.failures.retire_all(&event.cause);
        self.prune_blames();
        if dead_end.condition != DeadEndCondition::FailureAtZero {
            return Ok(());
        }
        let Some(core) = dead_end.core else {
            return Ok(());
        };
        self.require_core(core)?;
        self.constraints
            .set_failure_point(core, Some(FailurePoint::new(TargetOffset::ZERO)))?;
        self.retire_pending(core);
        self.update_core(core, |state| {
            state.zero_seq = Some(event.seq);
            state.pending = None;
        });
        Ok(())
    }

    fn combination(
        &mut self,
        event: &Event,
        combination: &togi_journal::Combination,
    ) -> Result<(), FoldError> {
        let members: PerCore<TargetOffset> = combination
            .members
            .iter()
            .map(|member| (member.core, member.offset))
            .collect();
        self.constraints
            .insert_combination(CombinationId::new(combination.combination), members)?;
        self.hunt_combination(event, combination);
        Ok(())
    }

    fn host_ranking(
        &mut self,
        event: &Event,
        ranking: &togi_journal::HostRanking,
    ) -> Result<(), FoldError> {
        self.ranking = Some(Ranked {
            ranking: Ranking::new(&self.topo, ranking.ranking.clone())?,
            seq: event.seq,
        });
        Ok(())
    }

    /// Whether the host ranking predates the plan the last round or hunt began, or was never read.
    pub(crate) fn ranking_stale(&self) -> bool {
        match (&self.ranking, self.last_plan_seq) {
            (None, _) => true,
            (Some(ranked), Some(plan)) => ranked.seq <= plan,
            (Some(_), None) => false,
        }
    }

    fn profile_change(&mut self, event: &Event, change: &ProfileChange) -> Result<(), FoldError> {
        let profile = Profile::new(&self.topo, change.to)?;
        if let Some(from) = change.from {
            Profile::new(&self.topo, from)?;
        }
        self.evidence.set_checking_profile(profile)?;
        self.checking_profile(event, profile);
        self.retire_reruns();
        self.revalidate_retry();
        Ok(())
    }
}

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    fn run_mut(&mut self) -> Result<&mut Run, FoldError> {
        self.run.as_mut().ok_or(FoldError::NotStarted)
    }

    pub(crate) fn run(&self) -> Option<&Run> {
        self.run.as_ref()
    }

    /// A copy for a simulation to fold forecast events into and discard.
    pub(crate) fn speculative_copy(&self) -> Self {
        let mut copy = self.clone();
        if let Some(run) = copy.run.as_mut() {
            run.evidence.speculate();
        }
        copy
    }

    /// Folds the next journal event. A structural impossibility, such as a core outside the
    /// topology or an event that does not follow the last, is an error, and the state is
    /// poisoned: every later `fold` fails and `next` has nothing to say, because a state that
    /// part-applied an event no longer stands for the journal.
    pub fn fold(&mut self, event: &Event) -> Result<(), FoldError> {
        if self.poisoned {
            return Err(FoldError::Poisoned);
        }
        let folded = self.fold_event(event);
        self.poisoned = folded.is_err();
        folded
    }

    fn fold_event(&mut self, event: &Event) -> Result<(), FoldError> {
        if let Some(run) = &self.run {
            if event.seq <= run.last_seq {
                return Err(FoldError::OutOfOrder {
                    last: run.last_seq,
                    seq: event.seq,
                });
            }
        }
        match &event.payload {
            Payload::SessionStart(start) => {
                if self.run.is_some() {
                    return Err(FoldError::DuplicateStart);
                }
                self.run = Some(Run::start(event, start)?);
                return Ok(());
            }
            Payload::ConfigLoaded(loaded) => self.run_mut()?.config_loaded(loaded)?,
            Payload::SessionBaseline(baseline) => self.run_mut()?.baseline(baseline)?,
            Payload::CommandReset(reset) => self.run_mut()?.command_reset(event, *reset)?,
            Payload::CorePhase(transition) => self.run_mut()?.core_phase(event, transition)?,
            Payload::TunerDecision(decision) => self.run_mut()?.tuner_decision(event, decision)?,
            Payload::TrialIntent(intent) => self.run_mut()?.trial_intent(event, intent)?,
            Payload::TrialProgress(progress) => self.run_mut()?.trial_progress(progress)?,
            Payload::TrialEnd(end) => self.run_mut()?.trial_end(event, end)?,
            Payload::Mce(mce) => self.run_mut()?.mce(event, mce)?,
            Payload::Failure(failure) => self.run_mut()?.failure(event, failure)?,
            Payload::TrialCarried(carried) => self.run_mut()?.trial_carried(event, carried)?,
            Payload::FailureCarried(carried) => self.run_mut()?.failure_carried(event, carried)?,
            Payload::Combination(combination) => {
                self.run_mut()?.combination(event, combination)?;
            }
            Payload::DeadEnd(dead_end) => self.run_mut()?.dead_end(event, dead_end)?,
            Payload::CrashDetected(_) => self.run_mut()?.crash_detected(),
            Payload::ProfileChange(change) => self.run_mut()?.profile_change(event, change)?,
            Payload::CheckingCycle(cycle) => self.run_mut()?.checking_cycle(event, cycle)?,
            Payload::CheckingStep(step) => self.run_mut()?.checking_step(event, step)?,
            Payload::CheckingChain(chain) => self.run_mut()?.checking_chain(event, chain)?,
            Payload::HuntSkipped(skipped) => self.run_mut()?.hunt_skipped(event, skipped),
            Payload::TunerWarning(_) => self.run_mut()?.warning = None,
            Payload::HostRanking(ranking) => self.run_mut()?.host_ranking(event, ranking)?,
            Payload::SessionContext(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::SessionArchived(_)
            | Payload::SessionCarried(_)
            | Payload::PreflightCheck(_)
            | Payload::SmuIntent(_)
            | Payload::SmuWrite(_)
            | Payload::SmuReadback(_)
            | Payload::SmuError(_)
            | Payload::ProfileApplied(_)
            | Payload::ProfileRestored(_)
            | Payload::TrialStart(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::BackendRetry(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::JournalTorn(_)
            | Payload::StateRebuilt(_) => {
                self.run_mut()?;
            }
            Payload::DeepeningRound(round) => self.run_mut()?.deepening_round(event, round)?,
            Payload::HuntStart(start) => self.run_mut()?.hunt_start(event, start)?,
            Payload::HuntGroup(group) => self.run_mut()?.hunt_group(event, group)?,
            Payload::HuntEnd(end) => self.run_mut()?.hunt_end(event, end)?,
            Payload::Shutdown(_) => self.run_mut()?.stopped = true,
        }
        self.run_mut()?.last_seq = event.seq;
        Ok(())
    }

    pub(crate) const fn poisoned(&self) -> bool {
        self.poisoned
    }

    /// Per R7 workload, CCD and core, whether the core has passed as a top requester at its
    /// current offset or deeper under the current backend identity.
    pub fn r7_status(&self) -> Vec<crate::R7CoreStatus> {
        self.run()
            .filter(|_| !self.poisoned())
            .map_or_else(Vec::new, Run::r7_status)
    }

    /// The machine's cores, CCDs and walk order, once the session started and the state stands.
    pub fn topology(&self) -> Option<&Topology> {
        if self.poisoned {
            return None;
        }
        self.run().map(|run| &run.topo)
    }

    /// The failure points and combinations folded so far, once the session started and the state
    /// stands.
    pub fn constraints(&self) -> Option<&Constraints> {
        if self.poisoned {
            return None;
        }
        self.run().map(|run| &run.constraints)
    }

    /// The evidence ledger folded so far, once the session started and the state stands: an
    /// analysis counts passes with it under the tuner's own classes, backends, windows and
    /// failure invalidation.
    pub fn evidence(&self) -> Option<&Evidence> {
        if self.poisoned {
            return None;
        }
        self.run().map(|run| &run.evidence)
    }

    #[cfg(test)]
    pub(crate) fn visits(&self) -> u64 {
        self.run().map_or(0, |run| {
            run.evidence.take_visits() + run.r7.targets.take_visits()
        })
    }
}
