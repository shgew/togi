use std::path::{Path, PathBuf};

use jiff::Timestamp;
use togi_machine::{
    Backend, BiosContext, BootId, CcdId, CoreId, CoreSet, CpuId, ObservedOffset, PerCore,
};

use crate::payload::{DeadEndCondition, EventKind, Payload, SmuCommand, TrialId};
use crate::{Build, Event, SCHEMA, Seq, SessionId};

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum FoldError {
    #[error("event #{seq} is {kind} before session.start")]
    BeforeStart { seq: Seq, kind: EventKind },
    #[error("event #{seq} is a second session.start")]
    SecondStart { seq: Seq },
    #[error("event #{seq} does not follow #{after}", after = display_last(*.after))]
    Gap { seq: Seq, after: Option<Seq> },
    #[error("event #{seq} ({kind}) names core {core}, which the session does not have")]
    UnknownCore {
        seq: Seq,
        kind: EventKind,
        core: CoreId,
    },
}

fn display_last(last: Option<Seq>) -> String {
    last.map_or_else(
        || "the start of the journal".to_owned(),
        |seq| seq.to_string(),
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionInfo {
    pub id: SessionId,
    pub start: Timestamp,
    pub build: Build,
    pub bios_context: Option<BiosContext>,
    pub baseline: PerCore<ObservedOffset>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecisionRef {
    pub seq: Seq,
    pub msg: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoreFacts {
    pub core: CoreId,
    pub ccd: CcdId,
    pub cpus: Vec<CpuId>,
    pub baseline: Option<ObservedOffset>,
    pub last_decision: Option<DecisionRef>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InFlight {
    pub seq: Seq,
    pub msg: String,
    pub boot: BootId,
    intent: PendingIntent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum PendingIntent {
    Set(CoreId),
    SetAll { write: Option<Seq>, unread: CoreSet },
    Trial(TrialId),
}

impl InFlight {
    pub const fn kind(&self) -> EventKind {
        match self.intent {
            PendingIntent::Set(_) | PendingIntent::SetAll { .. } => EventKind::SmuIntent,
            PendingIntent::Trial(_) => EventKind::TrialIntent,
        }
    }

    pub const fn trial(&self) -> Option<&TrialId> {
        match &self.intent {
            PendingIntent::Trial(trial) => Some(trial),
            PendingIntent::Set(_) | PendingIntent::SetAll { .. } => None,
        }
    }

    /// Whether a startup readback of this core on `boot` should cite this intent.
    pub fn covers_smu_core(&self, core: CoreId, boot: &BootId) -> bool {
        self.boot == *boot
            && match self.intent {
                PendingIntent::Set(target) => target == core,
                PendingIntent::SetAll { .. } => true,
                PendingIntent::Trial(_) => false,
            }
    }

    fn resolved_by_smu(&mut self, event: &Event) -> bool {
        if self.boot != event.boot {
            return false;
        }
        match &mut self.intent {
            PendingIntent::Set(_) => event.cause.contains(&self.seq),
            PendingIntent::SetAll { write, unread } => {
                let cites_intent = event.cause.contains(&self.seq);
                if let Payload::SmuWrite(written) = &event.payload
                    && cites_intent
                    && matches!(written.command(), SmuCommand::SetAll { .. })
                {
                    *write = Some(event.seq);
                }
                if let Payload::SmuReadback(readback) = &event.payload
                    && (cites_intent || write.is_some_and(|seq| event.cause.contains(&seq)))
                {
                    return unread.remove(readback.core) && unread.is_empty();
                }
                false
            }
            PendingIntent::Trial(_) => false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeadEndRef {
    pub condition: DeadEndCondition,
    pub seq: Seq,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct State {
    pub schema: u32,
    pub session: Option<SessionInfo>,
    pub cores: Vec<CoreFacts>,
    pub last_seq: Option<Seq>,
    pub open: Vec<InFlight>,
    pub dead_end: Option<DeadEndRef>,
    pub backends: BackendPaths,
}

/// The backend store paths the latest `config.loaded` recorded; a pass counts only under the path in force.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BackendPaths {
    pub mprime: Option<PathBuf>,
    pub ycruncher: Option<PathBuf>,
}

impl BackendPaths {
    pub fn path(&self, backend: Backend) -> Option<&Path> {
        match backend {
            Backend::Mprime => self.mprime.as_deref(),
            Backend::Ycruncher => self.ycruncher.as_deref(),
        }
    }
}

impl Default for State {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            session: None,
            cores: Vec::new(),
            last_seq: None,
            open: Vec::new(),
            dead_end: None,
            backends: BackendPaths::default(),
        }
    }
}

impl State {
    pub fn replay<'a>(events: impl IntoIterator<Item = &'a Event>) -> Result<Self, FoldError> {
        let mut state = Self::default();
        for event in events {
            state.apply(event)?;
        }
        Ok(state)
    }

    pub fn in_flight(&self) -> Option<&InFlight> {
        self.open.last()
    }

    /// The newest outstanding intent of a particular boot, for crash attribution.
    pub fn in_flight_on(&self, boot: &BootId) -> Option<&InFlight> {
        self.open.iter().rfind(|open| open.boot == *boot)
    }

    pub fn core(&self, core: CoreId) -> Option<&CoreFacts> {
        self.cores.iter().find(|facts| facts.core == core)
    }

    /// Folds the next event. Its seq must be greater than the last folded one: the readers guarantee that seqs
    /// count up from 1 without a hole, and a lenient read leaves holes where it skipped events of unknown kinds
    /// (`journal.md`, Event format), which a read-only fold passes over.
    pub fn apply(&mut self, event: &Event) -> Result<(), FoldError> {
        let follows = match self.last_seq {
            None => event.seq == Seq::FIRST,
            Some(last) => event.seq > last,
        };
        if !follows {
            return Err(FoldError::Gap {
                seq: event.seq,
                after: self.last_seq,
            });
        }
        let kind = event.kind();
        if self.session.is_none() && !matches!(event.payload, Payload::SessionStart(_)) {
            return Err(FoldError::BeforeStart {
                seq: event.seq,
                kind,
            });
        }
        match &event.payload {
            Payload::SessionStart(start) => {
                if self.session.is_some() {
                    return Err(FoldError::SecondStart { seq: event.seq });
                }
                self.session = Some(SessionInfo {
                    id: start.session,
                    start: event.time,
                    build: start.build.clone(),
                    bios_context: None,
                    baseline: PerCore::default(),
                });
                let mut cores: Vec<_> = start
                    .cores
                    .iter()
                    .map(|info| CoreFacts {
                        core: info.core,
                        ccd: info.ccd,
                        cpus: info.cpus.clone(),
                        baseline: None,
                        last_decision: None,
                    })
                    .collect();
                cores.sort_by_key(|facts| facts.core);
                self.cores = cores;
            }
            Payload::SessionContext(context) => {
                if let Some(session) = &mut self.session {
                    session.bios_context = Some(context.bios_context());
                }
            }
            Payload::SessionBaseline(baseline) => {
                for (core, _) in baseline.offsets.iter() {
                    self.known(event, core)?;
                }
                if let Some(session) = &mut self.session {
                    session.baseline = baseline.offsets;
                }
                for facts in &mut self.cores {
                    facts.baseline = baseline.offsets.get(facts.core);
                }
            }
            Payload::TunerDecision(decision) => self.decided(event, decision.core)?,
            Payload::CorePhase(transition) => self.decided(event, transition.core)?,
            Payload::DeadEnd(dead_end) => {
                if let Some(core) = dead_end.core {
                    self.decided(event, core)?;
                }
                self.dead_end = Some(DeadEndRef {
                    condition: dead_end.condition,
                    seq: event.seq,
                });
            }
            Payload::ConfigLoaded(config) => {
                self.dead_end = None;
                self.backends = BackendPaths {
                    mprime: config.config.backends.mprime.clone(),
                    ycruncher: config.config.backends.ycruncher.clone(),
                };
                if let Some(session) = &mut self.session {
                    session.build.version.clone_from(&config.build.version);
                    session.build.rev.clone_from(&config.build.rev);
                    session.build.fixes = config.build.fixes;
                }
            }
            Payload::SmuIntent(intent) => {
                let pending = match intent.command() {
                    SmuCommand::Set { core, .. } => PendingIntent::Set(core),
                    SmuCommand::SetAll { .. } => PendingIntent::SetAll {
                        write: None,
                        unread: self.cores.iter().map(|facts| facts.core).collect(),
                    },
                };
                self.push_open(event, pending);
            }
            Payload::SmuWrite(_) | Payload::SmuError(_) | Payload::SmuReadback(_) => {
                self.open.retain_mut(|open| !open.resolved_by_smu(event));
            }
            Payload::DayBootStart(start) => {
                if !event.cause.is_empty()
                    && start.observes_cores(self.cores.iter().map(|facts| facts.core))
                {
                    self.open.retain(|open| {
                        !(open.kind() == EventKind::SmuIntent && event.cause.contains(&open.seq))
                    });
                }
            }
            Payload::TrialIntent(intent) => {
                self.push_open(event, PendingIntent::Trial(intent.trial.clone()));
            }
            Payload::TrialEnd(end) => {
                self.open.retain(|open| open.trial() != Some(&end.trial));
            }
            Payload::CrashDetected(crash) => {
                self.open.retain(|open| open.boot != crash.previous_boot);
            }
            Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::SessionArchived(_)
            | Payload::SessionCarried(_)
            | Payload::PreflightCheck(_)
            | Payload::FrequencyIntent(_)
            | Payload::FrequencyReadback(_)
            | Payload::FrequencyDiscarded(_)
            | Payload::ProfileApplied(_)
            | Payload::ProfileChange(_)
            | Payload::ProfileRestored(_)
            | Payload::TrialStart(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::TrialCarried(_)
            | Payload::Failure(_)
            | Payload::FailureCarried(_)
            | Payload::Mce(_)
            | Payload::CheckingCycle(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntStart(_)
            | Payload::HuntGroup(_)
            | Payload::HuntEnd(_)
            | Payload::HuntSkipped(_)
            | Payload::Combination(_)
            | Payload::DeepeningRound(_)
            | Payload::TunerWarning(_)
            | Payload::BackendRetry(_)
            | Payload::CommandReset(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::Shutdown(_)
            | Payload::JournalTorn(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
            | Payload::DayUncleanEnd(_)
            | Payload::DayAnswer(_)
            | Payload::DayRecord(_)
            | Payload::DayFault(_)
            | Payload::SessionImported(_)
            | Payload::ExposureObserved(_)
            | Payload::CcdEdge(_)
            | Payload::CcdCap(_)
            | Payload::NightPlan(_)
            | Payload::TaskStart(_)
            | Payload::NightQualification(_)
            | Payload::NightReproduction(_)
            | Payload::DayStep(_)
            | Payload::DayApplied(_)
            | Payload::DayEvent(_)
            | Payload::DayVeto(_)
            | Payload::DayHold(_)
            | Payload::DayFreeze(_)
            | Payload::DayInterval(_)
            | Payload::DayPause(_)
            | Payload::StockAttested(_)
            | Payload::DayImported(_)
            | Payload::UnitMapping(_)
            | Payload::UnitEdge(_)
            | Payload::ClimbDecision(_)
            | Payload::UnitStop(_)
            | Payload::CampaignStart(_)
            | Payload::CampaignCharge(_)
            | Payload::CampaignReclassified(_)
            | Payload::FirmwareRestoration(_)
            | Payload::SuccessorImported(_)
            | Payload::StateRebuilt(_) => {}
        }
        self.last_seq = Some(event.seq);
        Ok(())
    }

    fn push_open(&mut self, event: &Event, intent: PendingIntent) {
        self.open.push(InFlight {
            seq: event.seq,
            msg: event.msg.clone(),
            boot: event.boot.clone(),
            intent,
        });
    }

    fn known(&self, event: &Event, core: CoreId) -> Result<(), FoldError> {
        if self.cores.iter().any(|facts| facts.core == core) {
            Ok(())
        } else {
            Err(FoldError::UnknownCore {
                seq: event.seq,
                kind: event.kind(),
                core,
            })
        }
    }

    fn decided(&mut self, event: &Event, core: CoreId) -> Result<(), FoldError> {
        self.known(event, core)?;
        if let Some(facts) = self.cores.iter_mut().find(|facts| facts.core == core) {
            facts.last_decision = Some(DecisionRef {
                seq: event.seq,
                msg: event.msg.clone(),
            });
        }
        Ok(())
    }
}
