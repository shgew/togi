use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use togi_machine::{Backend, BiosContext, BootId, CcdId, CoreId, CpuId, ObservedOffset, PerCore};

use crate::payload::{DeadEndCondition, EventKind, Payload, TrialId};
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct SessionInfo {
    pub id: SessionId,
    pub start: Timestamp,
    pub build: Build,
    pub bios_context: Option<BiosContext>,
    pub baseline: PerCore<ObservedOffset>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct DecisionRef {
    pub seq: Seq,
    pub msg: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct CoreFacts {
    pub core: CoreId,
    pub ccd: CcdId,
    pub cpus: Vec<CpuId>,
    pub baseline: Option<ObservedOffset>,
    pub last_decision: Option<DecisionRef>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct InFlight {
    pub seq: Seq,
    pub kind: EventKind,
    pub msg: String,
    pub boot: BootId,
    pub trial: Option<TrialId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self")]
pub struct DeadEndRef {
    pub condition: DeadEndCondition,
    pub seq: Seq,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self")]
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
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(remote = "Self")]
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

    pub fn core(&self, core: CoreId) -> Option<&CoreFacts> {
        self.cores.iter().find(|facts| facts.core == core)
    }

    pub fn apply(&mut self, event: &Event) -> Result<(), FoldError> {
        let expected = match self.last_seq {
            None => Some(Seq::FIRST),
            Some(last) => last.next(),
        };
        if expected != Some(event.seq) {
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
                let backends = config.config.backends();
                self.backends = BackendPaths {
                    mprime: backends.mprime().map(Path::to_path_buf),
                    ycruncher: backends.ycruncher().map(Path::to_path_buf),
                };
                if let Some(session) = &mut self.session {
                    session.build.version.clone_from(&config.build.version);
                    session.build.rev.clone_from(&config.build.rev);
                    session.build.fixes = config.build.fixes;
                }
            }
            Payload::SmuIntent(_) => {
                self.open.retain(|open| open.kind != EventKind::SmuIntent);
                self.push_open(event, None);
            }
            Payload::SmuWrite(_) | Payload::SmuError(_) => {
                self.open.retain(|open| {
                    !(open.kind == EventKind::SmuIntent && event.cause.contains(&open.seq))
                });
            }
            Payload::TrialIntent(intent) => self.push_open(event, Some(intent.trial.clone())),
            Payload::TrialEnd(end) => {
                self.open.retain(|open| {
                    !(open.kind == EventKind::TrialIntent
                        && open.trial.as_ref() == Some(&end.trial))
                });
            }
            Payload::CrashDetected(crash) => {
                self.open.retain(|open| open.boot != crash.previous_boot);
            }
            _ => {}
        }
        self.last_seq = Some(event.seq);
        Ok(())
    }

    fn push_open(&mut self, event: &Event, trial: Option<TrialId>) {
        self.open.push(InFlight {
            seq: event.seq,
            kind: event.kind(),
            msg: event.msg.clone(),
            boot: event.boot.clone(),
            trial,
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

togi_machine::object_only!(
    SessionInfo,
    DecisionRef,
    CoreFacts,
    InFlight,
    DeadEndRef,
    State,
    BackendPaths,
);
