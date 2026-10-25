//! Running one trial: the profile it needs, the wait a struggling backend is owed, the trial itself and the end the
//! evidence adds up to.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::time::Duration;

use togi_journal::{
    BackendRetry, Mce, Payload, RetryTarget, Seq, TrialEnd, TrialId, TrialInstance, TrialSignal,
    TrialStart, WorkloadContent,
};
use togi_machine::{
    Backend, CancellationToken, Condition, ControllerContract, CoreId, CoreSet, CpuId, Error,
    Partial, PerCore, Running, TargetOffset, TrialResult, TrialRole, TrialSpec,
};
use togi_tuner::policy::TrialPlan;

use crate::error::{SessionError, Step};
use crate::evidence::{self, RecordedMce, TrialEvidence};
use crate::execute::is_crash_flow;
use crate::recover::append_trial_end;
use crate::session::{Boundary, Control, Fault, Lifecycle, Session, Stop, Swept, TrialWindow};
use crate::trial::{self, Journaled};
use crate::write::{self, ProfileRecord, ProfileRequest};

const fn retry_target(backend: Backend) -> RetryTarget {
    match backend {
        Backend::Mprime => RetryTarget::Mprime,
        Backend::Ycruncher => RetryTarget::Ycruncher,
    }
}

/// The trial being run, as far as its records are concerned.
struct Trial<'p> {
    plan: &'p TrialPlan,
    id: TrialId,
    since: Duration,
    /// The event the trial's later records cite: its intent, then its start.
    start: Seq,
    /// The cores the trial loads.
    cores: CoreSet,
    roles: Vec<TrialRole>,
    controller: Option<togi_journal::ControllerEvidence>,
}

/// How the wait for a started workload ended, before the workload is joined.
struct Waiting {
    result: Result<TrialResult, Partial<TrialResult>>,
    /// The first report the journal refused, if any.
    reported: Option<crate::error::AppendFailure>,
}

/// What the end of a trial knows of the machine its workload ran on.
enum Joined {
    /// Every workload stopped and the machine stayed up.
    Stopped,
    /// The workload's containment is unconfirmed, for this reason, and the machine stayed up.
    Uncontained(String),
    /// The machine went down; the workload's containment may have been unconfirmed before it did.
    Crashed { uncontained: Option<String> },
}

impl Joined {
    fn of(crashed: bool, uncontained: Option<String>) -> Self {
        match (crashed, uncontained) {
            (true, uncontained) => Self::Crashed { uncontained },
            (false, Some(reason)) => Self::Uncontained(reason),
            (false, None) => Self::Stopped,
        }
    }

    const fn crashed(&self) -> bool {
        matches!(self, Self::Crashed { .. })
    }

    fn uncontained(&self) -> Option<&str> {
        match self {
            Self::Uncontained(reason)
            | Self::Crashed {
                uncontained: Some(reason),
            } => Some(reason),
            Self::Stopped | Self::Crashed { uncontained: None } => None,
        }
    }
}

/// How the workload and its wait went, as far as the trial's end is concerned.
struct Waited {
    result: TrialResult,
    /// What ended the trial early, if anything.
    error: Option<Error>,
    joined: Joined,
}

impl Session<'_> {
    pub(crate) fn run(&mut self, plan: &TrialPlan) -> Step<Control> {
        if self.cancel.is_cancelled() {
            return Ok(self.shut_down(Stop::Signal));
        }
        self.ensure_profile(plan)?;
        let mut backends: Vec<Backend> = Vec::new();
        for workload in plan.workloads() {
            let backend = workload.backend();
            if !backends.contains(&backend) {
                backends.push(backend);
            }
        }
        for backend in backends {
            if let stop @ Control::Stop(_) = self.wait_for_backend(backend)? {
                return Ok(stop);
            }
        }
        self.trial(plan)?;
        Ok(match self.check_dead_end()? {
            Some(stop) => Control::Stop(stop),
            None => Control::Continue,
        })
    }

    /// Puts the machine on the whole profile the plan runs at. Nothing is written when that profile is applied
    /// already.
    pub(crate) fn ensure_profile(&mut self, plan: &TrialPlan) -> Step<()> {
        let target: PerCore<TargetOffset> = plan
            .profile
            .expand(&self.topology)
            .map_err(togi_tuner::policy::PlanError::from)
            .map_err(SessionError::Plan)?;
        let applied = self
            .appender
            .recovery()
            .applied(&self.boot)
            .map(|(_, condition)| condition);
        if applied == Some(Condition::Together)
            && matches!(
                &self.lifecycle,
                Lifecycle::Swept(Swept {
                    confirmed: Some(_),
                    ..
                })
            )
            && self.hardware.profile().as_ref() == Some(&target)
        {
            return Ok(());
        }
        let Lifecycle::Swept(Swept {
            stopped: Some(stopped),
            confirmed,
            ..
        }) = &mut self.lifecycle
        else {
            return Err(SessionError::Unproven.into());
        };
        *confirmed = None;
        *confirmed = Some(write::apply_profile(
            &mut self.appender,
            &mut *self.smu,
            stopped,
            &mut self.hardware,
            ProfileRequest {
                target: &target,
                causes: &plan.causes,
                record: ProfileRecord::Applied(Condition::Together),
            },
        )?);
        Ok(())
    }

    /// Waits out the retry a backend is owed after its inconclusive trials in a row, once `backend.retry` records
    /// it: a wait the journal already records is sat out in full again without a second record. A cancelled wait
    /// ends the run, and no trial follows it.
    fn wait_for_backend(&mut self, backend: Backend) -> Step<Control> {
        let threshold = self.inconclusive_threshold();
        let Some(due) = self
            .appender
            .recovery()
            .backend_retry_due(backend, threshold)
        else {
            return Ok(Control::Continue);
        };
        if due.record {
            let reason = self
                .appender
                .recovery()
                .last_inconclusive_reason(backend)
                .unwrap_or_default()
                .to_owned();
            let retry = BackendRetry {
                backend: retry_target(backend),
                attempt: due.attempt,
                wait_s: due.wait_s,
                reason,
            };
            self.appender.append(Payload::BackendRetry(retry), &[])?;
        }
        match self.appender.clock().sleep(&self.cancel, due.wait) {
            Ok(()) => Ok(Control::Continue),
            Err(Error::Cancelled) => Ok(self.shut_down(Stop::Signal)),
            Err(error) => Err(SessionError::Machine(error).into()),
        }
    }

    fn trial(&mut self, plan: &TrialPlan) -> Step<()> {
        if self.lifecycle.stopped().is_none() {
            return Err(SessionError::Unproven.into());
        }
        let mut content = Vec::new();
        for workload in plan.workloads() {
            content.push(WorkloadContent {
                workload,
                content: self.content(workload)?,
            });
        }
        let id = self.appender.recovery().next_trial_id();
        let boundary = self.kernel_boundary(None, false, &[])?;
        let mut intent = plan
            .intent(id.clone(), &self.topology, content)
            .map_err(SessionError::Plan)?;
        intent.kernel_cursor = boundary.cursor;
        intent.kernel_error = boundary.error;
        let workload = intent.workload;
        let intent = self
            .appender
            .append(Payload::TrialIntent(intent), &plan.causes)?;
        let since = self
            .appender
            .clock()
            .monotonic()
            .map_err(SessionError::Machine)?;
        let window_start_ns = u64::try_from(since.as_nanos()).map_err(SessionError::Monotonic)?;
        let roles = plan
            .roles(intent.get(), &self.topology)
            .map_err(SessionError::Plan)?;
        let mut cores: Vec<CoreId> = Vec::new();
        let mut cpus: Vec<CpuId> = Vec::new();
        for role in &roles {
            if !cores.contains(&role.core) {
                cores.push(role.core);
            }
            if !cpus.contains(&role.cpu) {
                cpus.push(role.cpu);
            }
        }
        let spec = TrialSpec {
            id: id.as_str().to_owned(),
            regime: plan.cell.regime(),
            workload,
            roles,
            condition: Condition::Together,
            cores,
            cpus,
            duration: Duration::from_secs(u64::from(plan.cell.duration.get())),
            index: 0,
            seed: intent.get(),
            offsets: self.hardware.confirmed(),
            controller: ControllerContract::Managed
                .for_trial(self.appender.recovery().controller_rotation()),
        };
        let controller = Some(togi_journal::ControllerEvidence {
            pid: std::process::id(),
            threads: Vec::new(),
            workers: std::collections::BTreeMap::default(),
            tested_cpus: self
                .topology
                .iter()
                .filter(|info| spec.cores.contains(&info.core))
                .flat_map(|info| info.cpus.iter().copied())
                .collect(),
        });
        let mut trial = Trial {
            plan,
            id,
            since,
            start: intent,
            cores: spec.cores.iter().copied().collect(),
            roles: spec.roles.clone(),
            controller,
        };
        let Lifecycle::Swept(Swept {
            stopped,
            confirmed: Some(confirmed),
            ..
        }) = &mut self.lifecycle
        else {
            return Err(SessionError::Unproven.into());
        };
        let Some(ready) = stopped.take() else {
            return Err(SessionError::Unproven.into());
        };
        let token = self.cancel.child_token();
        let authorized = trial::authorize(confirmed);
        let mut cites = vec![intent];
        cites.extend_from_slice(authorized.readbacks());
        let running = match trial::start(&mut *self.trials, &token, authorized, spec, ready) {
            Ok(running) => running,
            Err(Partial { value, error }) => {
                let crashed = matches!(error, Error::Crashed);
                let uncontained =
                    (value.is_none() && !crashed).then(|| togi_render::error_chain(&error));
                if let Some(stopped) = value {
                    self.lifecycle.hold(stopped);
                }
                let joined = Joined::of(crashed, uncontained);
                if let Joined::Uncontained(_) = joined {
                    self.lifecycle.latch(Fault::Containment);
                }
                let waited = Waited {
                    result: TrialResult::default(),
                    error: Some(error),
                    joined,
                };
                return self.end_trial(&trial, &token, waited, "setup failed");
            }
        };
        self.appender.watch_trial(Some(token.clone()));
        let mut running = running;
        let window = catch_unwind(AssertUnwindSafe(|| {
            self.watch_started(&mut *running, &mut trial, &token, &cites, window_start_ns)
        }));
        let (scheduled, waiting) = match window {
            Ok(Ok(window)) => window,
            Ok(Err(flow)) => {
                self.reap(running);
                return Err(flow);
            }
            Err(payload) => {
                self.reap(running);
                resume_unwind(payload);
            }
        };
        let waited = self.join(running, waiting)?;
        if scheduled && waited.error.is_none() {
            let signal = TrialSignal {
                trial: trial.id.clone(),
                schedule: None,
                stops: waited.result.stops,
                conts: waited.result.conts,
            };
            self.appender
                .append(Payload::TrialSignal(signal), &[trial.start])?;
        }
        self.end_trial(&trial, &token, waited, "trial runner failed")
    }

    /// Everything between the workload's start and its join that can unwind or fail: the start record, the signal
    /// schedule and the wait while its reports are journaled. The caller holds the workload, so a panic or a failure
    /// here is followed by its join. Returns whether a signal schedule was recorded, and how the wait went.
    fn watch_started(
        &mut self,
        running: &mut dyn Running,
        trial: &mut Trial<'_>,
        token: &CancellationToken,
        cites: &[Seq],
        window_start_ns: u64,
    ) -> Step<(bool, Waiting)> {
        let started = running.started();
        let mut controller = trial.controller.take();
        if let Some(evidence) = &mut controller {
            evidence.threads = self
                .kernel
                .process_threads(evidence.pid)
                .map_err(SessionError::Machine)?;
            for pid in std::iter::once(started.pid)
                .chain(started.instances.iter().map(|instance| instance.pid))
            {
                if evidence.workers.contains_key(&pid) {
                    continue;
                }
                evidence.workers.insert(pid, pid);
                for tid in self
                    .kernel
                    .process_threads(pid)
                    .map_err(SessionError::Machine)?
                {
                    evidence.workers.insert(tid, pid);
                }
            }
        }
        let scheduled = started.schedule.is_some();
        let record = TrialStart {
            trial: trial.id.clone(),
            window_start_ns: Some(window_start_ns),
            scope: started.scope,
            pid: started.pid,
            cpus: started.cpus,
            argv: started.argv,
            files: started.files,
            instances: if started.instances.len() > 1 || controller.is_some() {
                started
                    .instances
                    .into_iter()
                    .map(|instance| TrialInstance {
                        core: instance.core,
                        cpus: instance.cpus,
                        pid: instance.pid,
                        scope: instance.scope,
                    })
                    .collect()
            } else {
                Vec::new()
            },
            roles: std::mem::take(&mut trial.roles),
            controller,
        };
        trial.start = self.appender.append(Payload::TrialStart(record), cites)?;
        if let Some(schedule) = started.schedule {
            let signal = TrialSignal {
                trial: trial.id.clone(),
                schedule: Some(schedule),
                stops: 0,
                conts: 0,
            };
            self.appender
                .append(Payload::TrialSignal(signal), &[trial.start])?;
        }
        let mut report = Journaled::new(&mut self.appender, &trial.id, trial.start, token);
        let result = running.wait(token, &mut report);
        let reported = report.into_failure();
        Ok((scheduled, Waiting { result, reported }))
    }

    /// Joins the workload and takes the proof that nothing runs from it, unless its containment is unconfirmed. A
    /// report that could not be journaled ends the trial here: the journal accepts nothing more.
    fn join(&mut self, running: Box<dyn Running>, waiting: Waiting) -> Step<Waited> {
        let Waiting {
            result: waited,
            reported,
        } = waiting;
        self.appender.watch_trial(None);
        let stopping = running.stop();
        let (result, mut error) = match waited {
            Ok(result) => (result, None),
            Err(Partial { value, error }) => (value, Some(error)),
        };
        let mut crashed = matches!(error, Some(Error::Crashed));
        let mut uncontained = match &error {
            Some(error @ Error::Containment(_)) => Some(togi_render::error_chain(error)),
            _ => None,
        };
        match stopping {
            Ok(stopped) if uncontained.is_none() => self.lifecycle.hold(stopped),
            Ok(stopped) => drop(stopped),
            Err(Error::Crashed) => {
                crashed = true;
                error.get_or_insert(Error::Crashed);
            }
            Err(unconfirmed) => {
                uncontained.get_or_insert_with(|| togi_render::error_chain(&unconfirmed));
            }
        }
        let joined = Joined::of(crashed, uncontained);
        match joined {
            Joined::Stopped => {}
            Joined::Uncontained(_) => self.lifecycle.latch(Fault::Containment),
            Joined::Crashed { .. } => self.lifecycle.crash(),
        }
        if let Some(failure) = reported {
            return Err(failure.into());
        }
        Ok(Waited {
            result,
            error,
            joined,
        })
    }

    /// Joins a workload a failed record or a panic left running, and keeps the proof if it gives one. A join that
    /// cannot confirm the stop is kept for the close to report, beside the error that ended the run.
    fn reap(&mut self, running: Box<dyn Running>) {
        self.appender.watch_trial(None);
        match running.stop() {
            Ok(stopped) => self.lifecycle.hold(stopped),
            Err(error) => {
                let fault = if matches!(error, Error::Crashed) {
                    Fault::Crashed
                } else {
                    Fault::Containment
                };
                self.lifecycle.latch(fault);
                self.unconfirmed.push(error);
            }
        }
    }

    /// Ends the trial: reads the kernel log for machine checks of its window, adds the
    /// evidence up and records the end. A trial the machine crashed in with no evidence of its own is left open for
    /// the recovery to close.
    fn end_trial(
        &mut self,
        trial: &Trial<'_>,
        token: &CancellationToken,
        waited: Waited,
        what: &str,
    ) -> Step<()> {
        let Waited {
            mut result,
            mut error,
            mut joined,
        } = waited;
        let evident = !result.escaped.is_empty() || result.failure.is_some();
        if matches!(joined, Joined::Crashed { uncontained: None }) && !evident {
            return Err(SessionError::Machine(Error::Crashed).into());
        }
        let mut boundary = Boundary::default();
        if let Joined::Stopped = joined {
            match self.teardown(trial) {
                Ok(read) => boundary = read,
                Err(flow) if is_crash_flow(&flow) && evident => {
                    self.lifecycle.crash();
                    joined = Joined::Crashed { uncontained: None };
                    error.get_or_insert(Error::Crashed);
                }
                Err(flow) => {
                    if matches!(
                        &flow,
                        crate::error::Flow::Fatal(
                            SessionError::Monotonic(_)
                                | SessionError::Machine(Error::InvalidMonotonic { .. })
                        )
                    ) {
                        self.keep_containment(trial, &result, joined.uncontained())?;
                    }
                    return Err(flow);
                }
            }
        }
        let crashed = joined.crashed();
        let containment = joined.uncontained();
        let open = self
            .appender
            .recovery()
            .open_trial()
            .filter(|open| open.intent.trial == trial.id);
        let kernel_error = evidence::join_diagnostic(
            open.and_then(|open| open.kernel_error.as_deref()),
            boundary.error.as_deref(),
        );
        let machine_checks: Vec<Seq> = open.map(|open| open.mces.clone()).unwrap_or_default();
        let mces: Vec<RecordedMce> = machine_checks
            .iter()
            .map(|&seq| RecordedMce {
                seq,
                corrected: self.mce_corrected(seq),
            })
            .collect();
        let fault_causes: Vec<_> = open
            .into_iter()
            .flat_map(|open| open.faults.iter().map(|fault| fault.seq))
            .collect();
        let process_failure = open.and_then(|open| {
            let controller = open.started.as_ref()?.controller.as_ref()?;
            let failure = evidence::process_faults_failure(
                result.failure.map(evidence::FailureSignal::from),
                &open.faults,
                &controller.tested_cpus,
            );
            Some(failure)
        });
        if let Some(failure) = process_failure {
            if result.failure.is_some() && failure.is_none() {
                result.inconclusive = Some("worker fault executed outside tested CPUs".into());
            }
            result.failure = failure.and_then(|failure| {
                failure.core.map(|core| togi_machine::TrialFailure {
                    signal: failure.signal,
                    core,
                    pid: failure.pid.and_then(std::num::NonZeroU32::new),
                })
            });
        }
        let runner = error
            .as_ref()
            .filter(|error| !matches!(error, Error::Cancelled))
            .map(|error| format!("{what}: {}", togi_render::error_chain(error)));
        let missing = evidence::join_diagnostic(kernel_error.as_deref(), runner.as_deref());
        let mut adjudication = match evidence::adjudicate(&TrialEvidence {
            result: &result,
            mces: &mces,
            reset: None,
            missing: missing.as_deref(),
            duration: Duration::from_secs(u64::from(trial.plan.cell.duration.get())),
            containment,
        }) {
            Ok(adjudication) => adjudication,
            Err(error) => {
                self.keep_containment(trial, &result, containment)?;
                return Err(error.into());
            }
        };
        if result.escaped.is_empty()
            && containment.is_none()
            && let Some(Some(failure)) = process_failure
        {
            adjudication.outcome = togi_journal::Outcome::Failure;
            adjudication.signal = Some(failure.signal);
            adjudication.core = failure.core;
        }
        let interrupted = token.is_cancelled();
        let reason = if interrupted {
            evidence::join_diagnostic(adjudication.reason.as_deref(), Some("stopped by signal"))
        } else {
            adjudication.reason
        };
        let end = TrialEnd {
            kernel_cursor: boundary.cursor,
            kernel_error,
            signal: adjudication.signal,
            core: adjudication.core,
            tctl_max_c: adjudication.tctl_max_c,
            reason,
            interrupted: interrupted || crashed,
            escaped: adjudication.escaped,
            excluded: result.excluded.into_iter().collect(),
            backend_missing: matches!(error, Some(Error::BackendMissing)),
            containment_error: adjudication.containment_error,
            ..evidence::trial_end(
                trial.id.clone(),
                adjudication.outcome,
                adjudication.duration_s,
            )
        };
        let topology = self.appender.recovery().topology();
        let summary = evidence::summarize_samples(
            self.trials.samples(trial.id.as_str()),
            trial.cores,
            trial.plan.cell.regime(),
            |core| topology.get(core).flatten(),
        );
        let mut causes = vec![trial.start];
        causes.extend(fault_causes);
        causes.extend(machine_checks);
        append_trial_end(
            &mut self.appender,
            &mut *self.trials,
            end,
            summary,
            crashed,
            &causes,
        )?;
        if crashed {
            return Err(SessionError::Machine(Error::Crashed).into());
        }
        Ok(())
    }

    fn keep_containment(
        &mut self,
        trial: &Trial<'_>,
        result: &TrialResult,
        uncontained: Option<&str>,
    ) -> Result<(), SessionError> {
        let allowed = self
            .appender
            .journal()
            .event(trial.start)
            .and_then(|event| {
                if let Payload::TrialStart(start) = &event.payload {
                    Some(start.cpus.as_slice())
                } else {
                    None
                }
            })
            .unwrap_or_default();
        let Some(detail) =
            crate::recovery::containment_detail(&result.escaped, allowed, uncontained)
        else {
            return Ok(());
        };
        let stop = self.dead_end(
            togi_journal::DeadEnd {
                condition: togi_journal::DeadEndCondition::Containment,
                core: None,
                detail,
                action: None,
            },
            &[trial.start],
        )?;
        self.record_shutdown(&stop);
        Ok(())
    }

    fn mce_corrected(&self, seq: Seq) -> bool {
        self.appender.journal().event(seq).is_some_and(|event| {
            matches!(
                &event.payload,
                Payload::Mce(Mce {
                    corrected: true,
                    ..
                })
            )
        })
    }

    /// Reads the kernel log: machine checks since the trial's window began are the trial's.
    fn teardown(&mut self, trial: &Trial<'_>) -> Step<Boundary> {
        self.kernel_boundary(
            Some(TrialWindow {
                trial: &trial.id,
                since: trial.since,
            }),
            false,
            &[trial.start],
        )
    }
}
