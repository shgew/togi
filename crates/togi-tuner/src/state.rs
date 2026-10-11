use togi_journal::{Seq,
    CommandReset, ConfigLoaded, DeadEnd, DeadEndCondition, Event, Payload, ProfileChange, ResetTarget,
    SessionBaseline, SessionStart,
};
use togi_machine::{Backend, PerCore, TargetOffset};

use crate::cores::{Core, Rotation};
use crate::config::TunerConfig;
use crate::flight::Flight;
use crate::failures::Failures;
use crate::ledger::PendingWarning;
use crate::phases::Phases;
use crate::requirement::Retry;
use crate::search::Search;
use crate::{
    CombinationId, Constraints, Evidence, FailurePoint, FoldError, Profile, Topology,
};

/// The checking profile the last `profile.change` set.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Checking {
    profile: Option<Profile>,
    seq: Option<Seq>,
}

impl Checking {
    pub(crate) fn profile(&self) -> Option<Profile> {
        self.profile
    }

    pub(crate) fn seq(&self) -> Option<Seq> {
        self.seq
    }
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
    pub(crate) checking: Checking,
    pub(crate) last_seq: Seq,
}

/// The tuner's state: one fold of the journal, with a substate per rule group. `fold` takes each
/// event once; the decision functions only read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State {
    run: Option<Run>,
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
            checking: Checking::default(),
            last_seq: event.seq,
        })
    }

    fn config_loaded(&mut self, loaded: &ConfigLoaded) {
        self.config = TunerConfig::from_loaded(loaded);
        for backend in [Backend::Mprime, Backend::Ycruncher] {
            self.evidence
                .set_backend(backend, self.config.backend_path(backend));
        }
        self.revalidate_retry();
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
        self.update_core(core, |state| state.queued_reset = Some(event.seq));
        self.retry = None;
        Ok(())
    }

    fn dead_end(&mut self, event: &Event, dead_end: &DeadEnd) -> Result<(), FoldError> {
        if dead_end.condition != DeadEndCondition::FailureAtZero {
            return Ok(());
        }
        let Some(core) = dead_end.core else {
            return Ok(());
        };
        self.require_core(core)?;
        self.constraints
            .set_failure_point(core, Some(FailurePoint::new(TargetOffset::ZERO)))?;
        self.update_core(core, |state| {
            state.zero_seq = Some(event.seq);
            state.pending = None;
        });
        Ok(())
    }

    fn combination(&mut self, combination: &togi_journal::Combination) -> Result<(), FoldError> {
        let members: PerCore<TargetOffset> = combination
            .members
            .iter()
            .map(|member| (member.core, member.offset))
            .collect();
        self.constraints
            .insert_combination(CombinationId::new(combination.combination), members)?;
        Ok(())
    }

    fn profile_change(&mut self, event: &Event, change: &ProfileChange) -> Result<(), FoldError> {
        let profile = Profile::new(&self.topo, change.to)?;
        self.checking = Checking {
            profile: Some(profile),
            seq: Some(event.seq),
        };
        self.evidence.set_checking_profile(profile);
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

    /// Folds the next journal event. A structural impossibility, such as a core outside the
    /// topology or an event that does not follow the last, is an error and leaves the state as it
    /// was.
    pub fn fold(&mut self, event: &Event) -> Result<(), FoldError> {
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
            Payload::ConfigLoaded(loaded) => self.run_mut()?.config_loaded(loaded),
            Payload::SessionBaseline(baseline) => self.run_mut()?.baseline(baseline)?,
            Payload::CommandReset(reset) => self.run_mut()?.command_reset(event, *reset)?,
            Payload::CorePhase(transition) => self.run_mut()?.core_phase(event, transition)?,
            Payload::TunerDecision(decision) => self.run_mut()?.tuner_decision(event, decision)?,
            Payload::TrialIntent(intent) => self.run_mut()?.trial_intent(event, intent)?,
            Payload::TrialProgress(progress) => self.run_mut()?.trial_progress(progress)?,
            Payload::TrialEnd(end) => self.run_mut()?.trial_end(event, end)?,
            Payload::Mce(mce) => self.run_mut()?.mce(mce)?,
            Payload::Failure(failure) => self.run_mut()?.failure(event, failure)?,
            Payload::TrialCarried(carried) => self.run_mut()?.trial_carried(event, carried)?,
            Payload::FailureCarried(carried) => self.run_mut()?.failure_carried(event, carried)?,
            Payload::Combination(combination) => self.run_mut()?.combination(combination)?,
            Payload::DeadEnd(dead_end) => self.run_mut()?.dead_end(event, dead_end)?,
            Payload::CrashDetected(_) => self.run_mut()?.crash_detected(),
            Payload::ProfileChange(change) => self.run_mut()?.profile_change(event, change)?,
            Payload::CheckingCycle(cycle) => self.run_mut()?.checking_cycle(event, cycle)?,
            Payload::TunerWarning(_) => self.run_mut()?.warning = None,
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
            Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntStart(_)
            | Payload::HuntGroup(_)
            | Payload::HuntEnd(_)
            | Payload::HuntSkipped(_)
            | Payload::DeepeningRound(_)
            | Payload::Shutdown(_) => {
                self.run_mut()?;
            }
        }
        self.run_mut()?.last_seq = event.seq;
        Ok(())
    }

    /// The next decision or trial the folded state calls for, from the rules this state holds.
    pub(crate) fn decide(&self) -> Option<crate::Action> {
        self.run()?.decide()
    }

    #[cfg(test)]
    pub(crate) fn visits(&self) -> u64 {
        self.run().map_or(0, |run| run.evidence.take_visits())
    }
}
