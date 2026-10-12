use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::Serialize;
use togi_machine::object_only;
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

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    pub struct SessionInfo {
        pub id: SessionId,
        pub start: Timestamp,
        pub build: Build,
        pub bios_context: Option<BiosContext>,
        pub baseline: PerCore<ObservedOffset>,
    }
}

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    pub struct DecisionRef {
        pub seq: Seq,
        pub msg: String,
    }
}

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    pub struct CoreFacts {
        pub core: CoreId,
        pub ccd: CcdId,
        pub cpus: Vec<CpuId>,
        pub baseline: Option<ObservedOffset>,
        pub last_decision: Option<DecisionRef>,
    }
}

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    pub struct InFlight {
        pub seq: Seq,
        pub kind: EventKind,
        pub msg: String,
        pub boot: BootId,
        pub trial: Option<TrialId>,
    }
}

object_only! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    pub struct DeadEndRef {
        pub condition: DeadEndCondition,
        pub seq: Seq,
    }
}

object_only! {
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    pub struct State {
        pub schema: u32,
        pub session: Option<SessionInfo>,
        pub cores: Vec<CoreFacts>,
        pub last_seq: Option<Seq>,
        pub open: Vec<InFlight>,
        pub dead_end: Option<DeadEndRef>,
        pub backends: BackendPaths,
    }
}

object_only! {
    /// The backend store paths the latest `config.loaded` recorded; a pass counts only under the path in force.
    #[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
    pub struct BackendPaths {
        pub mprime: Option<PathBuf>,
        pub ycruncher: Option<PathBuf>,
    }
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
