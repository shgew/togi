use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::time::{Duration, Instant};

use togi_machine::{
    CancellationToken, OnCancel, Partial, Reporter, Running, Started, Stopped, Swept,
    TrialConditions, TrialResult, TrialSpec, Trials,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::error::Error;
use crate::error::InvalidTrial;
use crate::id::TrialId;
use crate::runner::Options;
use crate::runtime::{Command, Runtime, outside_runtime};
use crate::sensors::Roots;
use crate::supervisor::{Env, Event, Finished, Handle, TrialCommand};
use crate::timing::Timing;

/// Test construction: everything production fixes.
pub(crate) struct Settings {
    pub(crate) timing: Timing,
    pub(crate) roots: Roots,
    pub(crate) unscoped: bool,
    pub(crate) user_manager: bool,
}

/// Runs trials on one dedicated runtime thread; every launch happens there.
pub struct Runner {
    dir: PathBuf,
    timing: Timing,
    runtime: Runtime,
}

impl Runner {
    pub fn new(options: Options) -> Result<Self, Error> {
        #[cfg(target_os = "linux")]
        {
            let user_manager = !rustix::process::geteuid().is_root();
            let settings = Settings {
                timing: Timing::default(),
                roots: Roots::default(),
                unscoped: false,
                user_manager,
            };
            let proc = settings.roots.proc.clone();
            Self::build(options, settings, move || {
                crate::os::linux::Linux::new(
                    crate::procfs::Proc::new(proc, crate::procfs::ticks_per_second()),
                    user_manager,
                )
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = options;
            Err(Error::Unsupported {
                operation: "running trials",
            })
        }
    }

    pub(crate) fn build<O: crate::os::Os>(
        options: Options,
        settings: Settings,
        make_os: impl FnOnce() -> O + Send + 'static,
    ) -> Result<Self, Error> {
        let Options {
            dir,
            backends,
            topology,
            identity,
            pm_table,
        } = options;
        let timing = settings.timing;
        let env_dir = dir.clone();
        let runtime = Runtime::spawn(move || Env {
            os: std::rc::Rc::new(make_os()),
            timing: settings.timing,
            roots: settings.roots,
            dir: env_dir,
            backends,
            topology,
            identity,
            unscoped: settings.unscoped,
            user_manager: settings.user_manager,
            pm_table,
            storage: None,
            log_sink: None,
            supervisors: std::cell::RefCell::default(),
        })?;
        Ok(Self {
            dir,
            timing,
            runtime,
        })
    }

    fn ask<T>(
        &self,
        operation: &'static str,
        bound: Duration,
        command: impl FnOnce(std::sync::mpsc::SyncSender<T>) -> Command,
    ) -> Result<T, Error> {
        outside_runtime(operation)?;
        let (reply, answer) = sync_channel(1);
        self.runtime.send(command(reply))?;
        match answer.recv_timeout(bound) {
            Ok(value) => Ok(value),
            Err(RecvTimeoutError::Timeout) => Err(Error::Stuck { operation, bound }),
            Err(RecvTimeoutError::Disconnected) => Err(Error::RuntimeGone),
        }
    }

    /// The preflight check: a scope pinned to CPU 0 is created and ends, within 30 s.
    pub fn probe_scope(&self) -> Result<String, Error> {
        self.ask("the scope probe", self.timing.probe_bound(), |reply| {
            Command::Probe { reply }
        })?
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.runtime.shutdown();
    }
}

impl Trials for Runner {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, togi_machine::Error> {
        let bound = self.timing.sweep_bound();
        self.ask("the sweep", bound, |reply| Command::Sweep {
            cancel: cancel.clone(),
            reply,
        })?
        .map_err(Into::into)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        if let Err(error) = outside_runtime("start") {
            return Err(Partial {
                value: Some(ready),
                error: error.into(),
            });
        }
        let instances = u32::try_from(spec.cores.len()).unwrap_or(u32::MAX).max(1);
        let bound = self.timing.start_bound(instances);
        let (reply, answer) = sync_channel(1);
        let command = Command::Start {
            cancel: cancel.clone(),
            spec: Box::new(spec),
            ready,
            reply,
        };
        if let Err(error) = self.runtime.send(command) {
            return Err(Partial {
                value: None,
                error: error.into(),
            });
        }
        match answer.recv_timeout(bound) {
            Ok(Ok(handle)) => Ok(Box::new(Trial::new(handle, &self.timing))),
            Ok(Err(partial)) => Err(partial),
            Err(_) => Err(Partial {
                value: None,
                error: Error::Stuck {
                    operation: "the start",
                    bound,
                }
                .into(),
            }),
        }
    }

    fn ended(&mut self, id: &str) -> Result<(), togi_machine::Error> {
        outside_runtime("ended")?;
        let id = TrialId::parse(id).map_err(|error| Error::Invalid(InvalidTrial::Id(error)))?;
        togi_trialfiles::compress(&self.dir.join(id.as_str()))
            .map_err(|errors| Error::Compress(errors).into())
    }

    fn samples<'a>(&'a self, id: &'a str) -> Box<dyn Iterator<Item = TrialConditions> + 'a> {
        match TrialId::parse(id) {
            Ok(id) => Box::new(togi_machine::read_samples(&self.dir.join(id.as_str()))),
            Err(_) => Box::new(std::iter::empty()),
        }
    }
}

/// A started trial. Only `stop`, which consumes it, yields the proof that its workload stopped, and
/// the proof cannot come from a borrowed trial:
///
/// ```compile_fail,E0507
/// fn proof(trial: &mut Box<dyn togi_machine::Running>) -> Result<togi_machine::Stopped, togi_machine::Error> {
///     trial.stop()
/// }
/// ```
///
/// A stopped trial cannot be used again:
///
/// ```compile_fail,E0382
/// fn reuse(trial: Box<dyn togi_machine::Running>) {
///     let _ = trial.stop();
///     let _ = trial.started();
/// }
/// ```
pub struct Trial {
    started: Started,
    commands: UnboundedSender<TrialCommand>,
    events: std::sync::mpsc::Receiver<Event>,
    cancelled: Arc<AtomicBool>,
    _registration: OnCancel,
    ended: Option<Finished>,
    deadline: Instant,
    stop_bound: Duration,
}

impl Trial {
    pub(crate) fn new(handle: Handle, timing: &Timing) -> Self {
        let Handle {
            commands,
            events,
            started,
            duration,
            cancelled,
            registration,
        } = handle;
        Self {
            started,
            commands,
            events,
            cancelled,
            _registration: registration,
            ended: None,
            deadline: Instant::now() + timing.wait_bound(duration),
            stop_bound: timing.stop_bound(),
        }
    }

    #[cfg(test)]
    pub(crate) fn wait_deadline(&self) -> Instant {
        self.deadline
    }

    /// Waits for the supervisor's end, handing reports to `report`; false when it never came.
    fn await_end(&mut self, until: Instant, report: &mut dyn Reporter) -> bool {
        while self.ended.is_none() {
            let now = Instant::now();
            if now >= until {
                return false;
            }
            let remaining = until.saturating_duration_since(now);
            match self.events.recv_timeout(remaining) {
                Ok(Event::Progress(detail)) => report.progress(&detail),
                Ok(Event::Sample(sample)) => report.sample(sample),
                Ok(Event::Signal {
                    core,
                    signal,
                    detail,
                }) => report.signal(core, signal, &detail),
                Ok(Event::Finished(finished)) => self.ended = Some(*finished),
                Err(_) => return false,
            }
        }
        true
    }
}

struct Discard;

impl Reporter for Discard {
    fn progress(&mut self, _: &str) {}
    fn sample(&mut self, _: togi_machine::Sample) {}
    fn signal(&mut self, _: togi_machine::CoreId, _: togi_machine::Signal, _: &str) {}
}

fn unobserved(error: &Error) -> TrialResult {
    TrialResult {
        ran: Duration::ZERO,
        failure: None,
        inconclusive: Some(error.to_string()),
        escaped: Vec::new(),
        tctl_max_c: None,
        stops: 0,
        conts: 0,
    }
}

impl Running for Trial {
    fn started(&self) -> Started {
        self.started.clone()
    }

    fn wait(
        &mut self,
        cancel: &CancellationToken,
        report: &mut dyn Reporter,
    ) -> Result<TrialResult, Partial<TrialResult>> {
        if let Err(error) = outside_runtime("wait") {
            return Err(Partial {
                value: unobserved(&error),
                error: error.into(),
            });
        }
        let _registration = {
            let (commands, flag) = (self.commands.clone(), Arc::clone(&self.cancelled));
            cancel.on_cancel(move || {
                flag.store(true, Ordering::Release);
                let _ = commands.send(TrialCommand::Cancel);
            })
        };
        let deadline = self.deadline;
        if !self.await_end(deadline, report) {
            let error = Error::Stuck {
                operation: "the trial supervisor",
                bound: deadline.saturating_duration_since(Instant::now()),
            };
            return Err(Partial {
                value: unobserved(&error),
                error: error.into(),
            });
        }
        let Some(finished) = self.ended.as_mut() else {
            let error = Error::RuntimeGone;
            return Err(Partial {
                value: unobserved(&error),
                error: error.into(),
            });
        };
        let result = finished.result.clone();
        let mut errors = Vec::new();
        if let Err(containment) = &finished.teardown {
            errors.push(Error::Containment(Arc::clone(containment)));
        }
        errors.append(&mut finished.errors);
        match Error::join(errors) {
            None => Ok(result),
            Some(error) => Err(Partial {
                value: result,
                error: error.into(),
            }),
        }
    }

    fn stop(mut self: Box<Self>) -> Result<Stopped, togi_machine::Error> {
        outside_runtime("stop")?;
        if self.ended.is_none() {
            let _ = self.commands.send(TrialCommand::Stop);
            let until = Instant::now() + self.stop_bound;
            if !self.await_end(until, &mut Discard) {
                return Err(togi_machine::Error::Containment);
            }
        }
        match self.ended.take() {
            Some(Finished {
                teardown: Ok(stopped),
                ..
            }) => Ok(stopped),
            Some(Finished {
                teardown: Err(containment),
                ..
            }) => Err(Error::Containment(containment).into()),
            None => Err(togi_machine::Error::Containment),
        }
    }
}

impl Drop for Trial {
    fn drop(&mut self) {
        if self.ended.is_none() {
            let _ = self.commands.send(TrialCommand::Abandon);
        }
    }
}
