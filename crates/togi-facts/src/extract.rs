use std::collections::{BTreeMap, HashMap, HashSet};

use togi_journal::{
    Attribution, Build, ConfigLoaded, Event, Failure, FailureCarried, Outcome, Payload,
    ResetTarget, Seq, SessionId, SessionStart, TrialCarried, TrialEnd, TrialId, TrialIntent,
};
use togi_machine::{Backend, BiosContext, Condition, CoreSet};

use crate::error::FactsError;
use crate::fact::{
    BackendIdentity, Decisive, Fact, FactSource, Observation, Telemetry, TrialClass,
};
use crate::session::{Effect, Reset, ResetCutoffs, SessionFacts, TrialActivity};

pub fn extract<E>(
    session: SessionId,
    events: impl IntoIterator<Item = Result<Event, E>>,
) -> Result<SessionFacts, FactsError<E>> {
    let mut extractor: Option<Extractor> = None;
    for event in events {
        let event = event.map_err(FactsError::Read)?;
        match &mut extractor {
            None => extractor = Some(Extractor::start(session, &event)?),
            Some(extractor) => extractor.fold(&event)?,
        }
    }
    extractor
        .ok_or(FactsError::NotStarted)
        .map(Extractor::finish)
}

#[derive(Clone, Debug, Default)]
struct Backends {
    mprime: Option<BackendIdentity>,
    ycruncher: Option<BackendIdentity>,
}

impl Backends {
    fn recorded(loaded: &ConfigLoaded) -> Self {
        let paths = &loaded.config.backends;
        let identity = |backend| {
            paths.path(backend).map(|path| BackendIdentity {
                backend,
                path: path.to_string_lossy().into_owned(),
            })
        };
        Self {
            mprime: identity(Backend::Mprime),
            ycruncher: identity(Backend::Ycruncher),
        }
    }

    fn get(&self, backend: Backend) -> Option<&BackendIdentity> {
        match backend {
            Backend::Mprime => self.mprime.as_ref(),
            Backend::Ycruncher => self.ycruncher.as_ref(),
        }
    }
}

struct Open {
    activity: TrialActivity,
    build: Build,
    backend: Option<BackendIdentity>,
}

pub(crate) struct Extractor {
    id: SessionId,
    start: Build,
    build: Build,
    backends: Backends,
    topology: Vec<togi_machine::CoreInfo>,
    cores: CoreSet,
    context: Option<BiosContext>,
    facts: Vec<Fact>,
    carried: Vec<Fact>,
    resets: Vec<Reset>,
    reset_index: HashMap<Seq, usize>,
    cutoffs: ResetCutoffs,
    open: BTreeMap<Seq, Open>,
    open_by_trial: HashMap<TrialId, Seq>,
    decided: HashSet<TrialId>,
    carry_commit: Option<Seq>,
    shutdown: Option<Seq>,
}

impl Extractor {
    pub(crate) fn start<E>(session: SessionId, event: &Event) -> Result<Self, FactsError<E>> {
        let Payload::SessionStart(SessionStart {
            build,
            session: recorded,
            cores,
        }) = &event.payload
        else {
            return Err(FactsError::NotStarted);
        };
        if *recorded != session {
            return Err(FactsError::SessionMismatch {
                expected: session,
                recorded: *recorded,
            });
        }
        if cores.is_empty() {
            return Err(FactsError::EmptyTopology);
        }
        Ok(Self {
            id: session,
            start: build.clone(),
            build: build.clone(),
            backends: Backends::default(),
            topology: cores.clone(),
            cores: cores.iter().map(|info| info.core).collect(),
            context: None,
            facts: Vec::new(),
            carried: Vec::new(),
            resets: Vec::new(),
            reset_index: HashMap::new(),
            cutoffs: ResetCutoffs::default(),
            open: BTreeMap::new(),
            open_by_trial: HashMap::new(),
            decided: HashSet::new(),
            carry_commit: None,
            shutdown: None,
        })
    }

    pub(crate) fn fold<E>(&mut self, event: &Event) -> Result<(), FactsError<E>> {
        match &event.payload {
            Payload::SessionStart(_) => return Err(FactsError::DuplicateStart { seq: event.seq }),
            Payload::SessionContext(context) => self.context = Some(context.bios_context()),
            Payload::ConfigLoaded(loaded) => {
                let (start, loaded_build) = (&self.start, &loaded.build);
                let field = if loaded_build.schema != start.schema {
                    Some("schema")
                } else if loaded_build.ruleset != start.ruleset {
                    Some("ruleset")
                } else if loaded_build.evidence != start.evidence {
                    Some("evidence")
                } else {
                    None
                };
                if let Some(field) = field {
                    return Err(FactsError::StampChanged {
                        seq: event.seq,
                        field,
                    });
                }
                self.build = loaded.build.clone();
                self.backends = Backends::recorded(loaded);
            }
            Payload::TrialIntent(intent) => self.intent(event, intent)?,
            Payload::TrialStart(start) => {
                if let Some(open) = self.open_in_boot(&start.trial, event) {
                    open.activity.started = true;
                    open.activity.last_evidence = event.time;
                }
            }
            Payload::TrialProgress(progress) => self.evidence(&progress.trial, event),
            Payload::TrialSignal(signal) => self.evidence(&signal.trial, event),
            Payload::TrialSample(sample) => self.evidence(&sample.trial, event),
            Payload::CrashDetected(crash) => {
                if let Some(seq) = crash.in_flight
                    && let Some(open) = self.open.get_mut(&seq)
                    && open.activity.boot == crash.previous_boot
                {
                    open.activity.crashed = true;
                }
            }
            Payload::TrialEnd(end) => self.end(event, end)?,
            Payload::Failure(failure) => self.idle_failure(event, failure),
            Payload::TrialCarried(carried) => {
                let fact = carried_trial(event.seq, carried)?;
                self.carried.push(fact);
            }
            Payload::FailureCarried(carried) => {
                let fact = carried_failure(event.seq, carried)?;
                self.carried.push(fact);
            }
            Payload::CommandReset(reset) => {
                self.reset_index.insert(event.seq, self.resets.len());
                self.resets.push(Reset {
                    seq: event.seq,
                    target: reset.target,
                    effects: Vec::new(),
                    applied: None,
                });
                self.cutoffs.record(event.seq, reset.target);
            }
            Payload::SessionCarried(_) => {
                self.carry_commit.get_or_insert(event.seq);
            }
            Payload::Shutdown(_) => self.shutdown = Some(event.seq),
            Payload::SessionBaseline(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::PreflightCheck(_)
            | Payload::SmuIntent(_)
            | Payload::SmuWrite(_)
            | Payload::SmuReadback(_)
            | Payload::SmuError(_)
            | Payload::ProfileApplied(_)
            | Payload::ProfileChange(_)
            | Payload::ProfileRestored(_)
            | Payload::Mce(_)
            | Payload::TunerDecision(_)
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
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::DeadEnd(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::JournalTorn(_)
            | Payload::StateRebuilt(_)
            | Payload::CorePhase(_)
            | Payload::SessionArchived(_) => {}
        }
        self.record_reset_effects(event);
        Ok(())
    }

    fn open_in_boot(&mut self, trial: &TrialId, event: &Event) -> Option<&mut Open> {
        let seq = self.open_by_trial.get(trial)?;
        self.open
            .get_mut(seq)
            .filter(|open| open.activity.boot == event.boot)
    }

    fn evidence(&mut self, trial: &TrialId, event: &Event) {
        if let Some(open) = self.open_in_boot(trial, event)
            && open.activity.started
        {
            open.activity.last_evidence = event.time;
        }
    }

    fn intent<E>(&mut self, event: &Event, intent: &TrialIntent) -> Result<(), FactsError<E>> {
        if self.open_by_trial.contains_key(&intent.trial) || self.decided.contains(&intent.trial) {
            return Err(FactsError::DuplicateIntent {
                trial: intent.trial.clone(),
                seq: event.seq,
            });
        }
        self.open_by_trial.insert(intent.trial.clone(), event.seq);
        self.open.insert(
            event.seq,
            Open {
                activity: TrialActivity {
                    seq: event.seq,
                    time: event.time,
                    boot: event.boot.clone(),
                    intent: intent.clone(),
                    started: false,
                    last_evidence: event.time,
                    crashed: false,
                },
                build: self.build.clone(),
                backend: self.backends.get(intent.workload.backend()).cloned(),
            },
        );
        Ok(())
    }

    fn end<E>(&mut self, event: &Event, end: &TrialEnd) -> Result<(), FactsError<E>> {
        let decisive = end.outcome != Outcome::Inconclusive;
        let Some(open) = self
            .open_by_trial
            .remove(&end.trial)
            .and_then(|seq| self.open.remove(&seq))
        else {
            return if decisive && self.decided.contains(&end.trial) {
                Err(FactsError::DuplicateEnd {
                    trial: end.trial.clone(),
                    seq: event.seq,
                })
            } else {
                Ok(())
            };
        };
        let intent = &open.activity.intent;
        let outcome = match end.outcome {
            Outcome::Inconclusive => return Ok(()),
            Outcome::Pass => Decisive::Pass,
            Outcome::Failure => Decisive::Failure {
                signal: end.signal.ok_or(FactsError::Incomplete {
                    seq: event.seq,
                    field: "signal",
                })?,
                core: end.core,
                stalled: end.stalled_core,
            },
        };
        let backend = match outcome {
            Decisive::Pass => open.backend.clone(),
            Decisive::Failure { .. } => None,
        };
        let fact = Fact {
            source: FactSource {
                session: self.id,
                seq: event.seq,
                time: event.time,
                build: open.build.clone(),
                epoch: self.start.evidence,
                boot: open.activity.boot.clone(),
                trial: Some(end.trial.clone()),
                backend,
            },
            local: event.seq,
            observation: Observation::Trial {
                class: TrialClass {
                    workload: intent.workload,
                    loaded: intent
                        .core
                        .map_or(intent.cores, |core| CoreSet::from_iter([core])),
                    duration_s: intent.duration_s,
                },
                condition: intent.condition,
                activity: intent.activity,
                rerun: intent.rerun,
                profile: intent.profile,
                outcome,
                duration_s: end.duration_s,
                telemetry: telemetry(&end.voltage_requests_v, end.top_requesters, &end.ccd_mhz),
            },
        };
        self.decided.insert(end.trial.clone());
        self.facts.push(fact);
        Ok(())
    }

    fn idle_failure(&mut self, event: &Event, failure: &Failure) {
        let (Some(condition @ (Condition::Together | Condition::Parked)), Some(profile)) =
            (failure.condition, failure.profile)
        else {
            return;
        };
        let complete = profile.iter().map(|(core, _)| core).collect::<CoreSet>() == self.cores;
        if failure.known_failure.is_some()
            || failure.trial.is_some()
            || failure.attribution != Attribution::Unattributed
            || !complete
        {
            return;
        }
        self.facts.push(Fact {
            source: FactSource {
                session: self.id,
                seq: event.seq,
                time: event.time,
                build: self.build.clone(),
                epoch: self.start.evidence,
                boot: event.boot.clone(),
                trial: None,
                backend: None,
            },
            local: event.seq,
            observation: Observation::IdleFailure {
                loaded: self.cores,
                condition,
                profile,
                signal: failure.signal,
                core: failure.core,
                offset: failure.offset,
                regime: failure.regime,
                reason: failure.reason.clone(),
                round: failure.round,
            },
        });
    }

    fn record_reset_effects(&mut self, event: &Event) {
        for cause in &event.cause {
            let Some(&index) = self.reset_index.get(cause) else {
                continue;
            };
            let reset = &mut self.resets[index];
            reset.effects.push(Effect {
                seq: event.seq,
                kind: event.kind(),
            });
            if let Payload::CorePhase(phase) = &event.payload
                && reset.target == ResetTarget::Core(phase.core)
            {
                reset.applied = Some(event.seq);
            }
        }
        if matches!(event.payload, Payload::SessionArchived(_)) {
            for reset in &mut self.resets {
                if reset.target == ResetTarget::All && reset.applied.is_none() {
                    reset.applied = Some(event.seq);
                    if !reset.effects.iter().any(|effect| effect.seq == event.seq) {
                        reset.effects.push(Effect {
                            seq: event.seq,
                            kind: event.kind(),
                        });
                    }
                }
            }
        }
    }

    pub(crate) fn finish(self) -> SessionFacts {
        SessionFacts {
            id: self.id,
            build: self.start,
            context: self.context,
            topology: self.topology,
            cores: self.cores,
            facts: self.facts,
            carried: self.carried,
            resets: self.resets,
            cutoffs: self.cutoffs,
            open_trials: self.open.into_values().map(|open| open.activity).collect(),
            carry_commit: self.carry_commit,
            shutdown: self.shutdown,
        }
    }

    #[cfg(test)]
    pub(crate) fn retained(&self) -> usize {
        self.facts.len()
            + self.carried.len()
            + self.resets.len()
            + self.open.len()
            + self.open_by_trial.len()
            + self.decided.len()
    }
}

fn telemetry(
    voltage_requests_v: &togi_machine::PerCore<f64>,
    top_requesters: CoreSet,
    ccd_mhz: &BTreeMap<togi_machine::CcdId, u32>,
) -> Option<Telemetry> {
    (!voltage_requests_v.is_empty() || !top_requesters.is_empty() || !ccd_mhz.is_empty()).then(
        || Telemetry {
            voltage_requests_v: *voltage_requests_v,
            top_requesters,
            ccd_mhz: ccd_mhz.clone(),
        },
    )
}

fn copied_source(source: &togi_journal::FactSource) -> FactSource {
    FactSource {
        session: source.session,
        seq: source.seq,
        time: source.time,
        build: source.build.clone(),
        epoch: source.build.evidence,
        boot: source.boot.clone(),
        trial: source.trial.clone(),
        backend: source.backend.clone(),
    }
}

fn carried_trial<E>(seq: Seq, carried: &TrialCarried) -> Result<Fact, FactsError<E>> {
    let workload = carried.class.workload.ok_or(FactsError::Incomplete {
        seq,
        field: "class.workload",
    })?;
    let outcome = match carried.outcome {
        Outcome::Pass => Decisive::Pass,
        Outcome::Failure => Decisive::Failure {
            signal: carried.signal.ok_or(FactsError::Incomplete {
                seq,
                field: "signal",
            })?,
            core: carried.core,
            stalled: carried.stalled_core,
        },
        Outcome::Inconclusive => return Err(FactsError::NotDecisive { seq }),
    };
    Ok(Fact {
        source: copied_source(&carried.source),
        local: seq,
        observation: Observation::Trial {
            class: TrialClass {
                workload,
                loaded: carried.class.cores,
                duration_s: carried.class.duration_s,
            },
            condition: carried.condition,
            activity: carried.activity,
            rerun: carried.rerun,
            profile: carried.profile,
            outcome,
            duration_s: carried.duration_s,
            telemetry: telemetry(
                &carried.voltage_requests_v,
                carried.top_requesters,
                &carried.ccd_mhz,
            ),
        },
    })
}

fn carried_failure<E>(seq: Seq, carried: &FailureCarried) -> Result<Fact, FactsError<E>> {
    let failure = &carried.failure;
    if failure.trial.is_some()
        || failure.known_failure.is_some()
        || failure.attribution != Attribution::Unattributed
    {
        return Err(FactsError::NotIdle { seq });
    }
    let (Some(condition @ (Condition::Together | Condition::Parked)), Some(profile)) =
        (failure.condition, failure.profile)
    else {
        return Err(if failure.profile.is_none() {
            FactsError::Incomplete {
                seq,
                field: "failure.profile",
            }
        } else if failure.condition.is_none() {
            FactsError::Incomplete {
                seq,
                field: "failure.condition",
            }
        } else {
            FactsError::NotIdle { seq }
        });
    };
    let profiled = profile.iter().map(|(core, _)| core).collect::<CoreSet>();
    if profiled.is_empty() || profiled != carried.class.cores {
        return Err(FactsError::NotIdle { seq });
    }
    Ok(Fact {
        source: copied_source(&carried.source),
        local: seq,
        observation: Observation::IdleFailure {
            loaded: carried.class.cores,
            condition,
            profile,
            signal: failure.signal,
            core: failure.core,
            offset: failure.offset,
            regime: failure.regime,
            reason: failure.reason.clone(),
            round: failure.round,
        },
    })
}
