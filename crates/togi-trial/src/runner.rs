use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::time::{Duration, Instant};

use tokio::sync::mpsc::UnboundedSender;
use togi_backend::Backend;
use togi_machine::{
    CancellationToken, CoreInfo, OnCancel, Partial, PmTableReader, Reporter, Running, Started,
    Stopped, Swept, TrialConditions, TrialResult, TrialSpec, Trials,
};

use crate::error::Error;
use crate::id::TrialId;
use crate::identity::BackendIdentity;
use crate::plan::InvalidTrial;
use crate::runtime::{Command, Runtime, outside_runtime};
use crate::sensors::Roots;
use crate::supervisor::{Env, Event, Finished, Handle, TrialCommand};
use crate::timing::Timing;

/// The backend adapters by kind.
#[derive(Clone, Default)]
pub struct Backends {
    mprime: Option<Arc<dyn Backend>>,
    ycruncher: Option<Arc<dyn Backend>>,
}

impl Backends {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with(mut self, backend: Arc<dyn Backend>) -> Self {
        match backend.kind() {
            togi_machine::Backend::Mprime => self.mprime = Some(backend),
            togi_machine::Backend::Ycruncher => self.ycruncher = Some(backend),
        }
        self
    }

    pub fn get(&self, kind: togi_machine::Backend) -> Option<&Arc<dyn Backend>> {
        match kind {
            togi_machine::Backend::Mprime => self.mprime.as_ref(),
            togi_machine::Backend::Ycruncher => self.ycruncher.as_ref(),
        }
    }
}

/// The production inputs of a runner.
pub struct Options {
    /// Where trials live: one directory per trial id.
    pub dir: PathBuf,
    pub backends: Backends,
    pub topology: Vec<CoreInfo>,
    /// The unprivileged account backends run as; without it every start is refused.
    pub identity: Option<BackendIdentity>,
    pub pm_table: Option<Arc<dyn PmTableReader + Send + Sync>>,
}

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
        self.ask(
            "the scope probe",
            self.timing.probe + Duration::from_secs(5),
            |reply| Command::Probe { reply },
        )?
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.runtime.shutdown();
    }
}

impl Trials for Runner {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, togi_machine::Error> {
        let bound = self.timing.teardown + Duration::from_secs(5);
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
        let bound = spec.duration
            + Duration::from_secs(30) * instances
            + self.timing.stop_grace
            + Duration::from_secs(20)
            + self.timing.teardown;
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

/// A started trial. Stopping it consumes it, so it can neither be used afterwards nor yield proof
/// without teardown.
///
/// ```compile_fail
/// fn proof(trial: Box<dyn togi_machine::Running>) -> togi_machine::Stopped {
///     togi_machine::Stopped::confirmed()
/// }
/// ```
///
/// ```compile_fail
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
    fn new(handle: Handle, timing: &Timing) -> Self {
        let Handle {
            commands,
            events,
            started,
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
            deadline: Instant::now() + timing.teardown + timing.writer_join + Duration::from_secs(5),
            stop_bound: timing.teardown + Duration::from_secs(2),
        }
    }

    /// Waits for the supervisor's end, handing reports to `report`; false when it never came.
    fn await_end(&mut self, until: Instant, report: &mut dyn Reporter) -> bool {
        while self.ended.is_none() {
            let remaining = until.saturating_duration_since(Instant::now());
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
