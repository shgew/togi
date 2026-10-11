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

use crate::error::InvalidTrial;
use crate::error::{Error, Shared};
use crate::files::TrialsRoot;
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
        let attempt = cancel.child_token();
        let asked = self.ask("the sweep", bound, |reply| Command::Sweep {
            cancel: attempt.clone(),
            reply,
        });
        if matches!(asked, Err(Error::Stuck { .. })) {
            attempt.cancel();
        }
        asked?.map_err(Into::into)
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
        if !self.timing.representable(spec.duration, instances) {
            return Err(Partial {
                value: Some(ready),
                error: Error::Invalid(InvalidTrial::UnrepresentableDuration).into(),
            });
        }
        let bound = self.timing.start_bound(instances);
        // This start's own token: fired when the caller gives up waiting, so the runtime rolls
        // the start back instead of finishing a trial nobody holds, and when the runner ends.
        let attempt = cancel.child_token();
        let (reply, answer) = sync_channel(1);
        let command = Command::Start {
            cancel: attempt.clone(),
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
            Ok(Ok(delivered)) => Ok(Box::new(Trial::new(delivered.claim(), &self.timing))),
            Ok(Err(partial)) => Err(partial),
            Err(_) => {
                attempt.cancel();
                Err(Partial {
                    value: None,
                    error: Error::Stuck {
                        operation: "the start",
                        bound,
                    }
                    .into(),
                })
            }
        }
    }

    fn ended(&mut self, id: &str) -> Result<(), togi_machine::Error> {
        outside_runtime("ended")?;
        let id = TrialId::parse(id).map_err(|error| Error::Invalid(InvalidTrial::Id(error)))?;
        let root = TrialsRoot::open(&self.dir, false).map_err(Error::Prepare)?;
        let trial = root.trial(&id).map_err(Error::Prepare)?;
        compress_pinned(&trial).map_err(|errors| Error::Compress(errors).into())
    }

    fn samples<'a>(&'a self, id: &'a str) -> Box<dyn Iterator<Item = TrialConditions> + 'a> {
        let reader = TrialId::parse(id).ok().and_then(|id| {
            TrialsRoot::open(&self.dir, false)
                .ok()
                .and_then(|root| root.samples(&id))
        });
        match reader {
            Some(reader) => Box::new(togi_machine::ReadSamples::from_reader(reader)),
            None => Box::new(std::iter::empty()),
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
    /// What `wait` found beyond the result, built by the first `wait` that reached the end.
    failure: Option<Arc<Error>>,
    reported: bool,
    created: Instant,
    bound: Duration,
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
            failure: None,
            reported: false,
            created: Instant::now(),
            bound: timing.wait_bound(duration),
            stop_bound: timing.stop_bound(),
        }
    }

    #[cfg(test)]
    pub(crate) fn wait_budget(&self) -> Duration {
        self.bound.saturating_sub(self.created.elapsed())
    }

    /// Waits up to `budget` for the supervisor's end, handing reports to `report`; with `block`
    /// false it takes only what is already queued. False when the end never came.
    fn await_end(&mut self, budget: Duration, block: bool, report: &mut dyn Reporter) -> bool {
        let begun = Instant::now();
        while self.ended.is_none() {
            let elapsed = begun.elapsed();
            if elapsed >= budget {
                return false;
            }
            let event = if block {
                self.events
                    .recv_timeout(budget.saturating_sub(elapsed))
                    .ok()
            } else {
                self.events.try_recv().ok()
            };
            match event {
                Some(Event::Progress(detail)) => report.progress(&detail),
                Some(Event::Sample(sample)) => report.sample(sample),
                Some(Event::Signal {
                    core,
                    signal,
                    detail,
                }) => report.signal(core, signal, &detail),
                Some(Event::Finished(finished)) => self.ended = Some(*finished),
                None => return false,
            }
        }
        true
    }
}

/// Compresses the trial directory `trial` holds open. The files are reached through the
/// descriptor, not through the trial's pathname, so renaming or replacing a directory above it
/// cannot redirect the compression.
pub(crate) fn compress_pinned(trial: &std::os::fd::OwnedFd) -> Result<(), togi_trialfiles::Errors> {
    use std::os::fd::AsRawFd as _;
    togi_trialfiles::compress(&PathBuf::from(format!(
        "/proc/self/fd/{}",
        trial.as_raw_fd()
    )))
}

struct Discard;

impl Reporter for Discard {
    fn progress(&mut self, _: &str) {}
    fn sample(&mut self, _: togi_machine::Sample) {}
    fn signal(&mut self, _: togi_machine::CoreId, _: togi_machine::Signal, _: &str) {}
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
                value: TrialResult::default(),
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
        let remaining = self.bound.saturating_sub(self.created.elapsed());
        // A wait after the supervisor's bound has passed still collects what already finished:
        // it reads what is queued, bounded by the stop bound for reports, and never blocks.
        let (budget, block) = if remaining.is_zero() {
            (self.stop_bound, false)
        } else {
            (remaining, true)
        };
        if !self.await_end(budget, block, report) {
            let error = Error::Stuck {
                operation: "the trial supervisor",
                bound: self.bound,
            };
            return Err(Partial {
                value: TrialResult::default(),
                error: error.into(),
            });
        }
        let Some(finished) = self.ended.as_mut() else {
            let error = Error::RuntimeGone;
            return Err(Partial {
                value: TrialResult::default(),
                error: error.into(),
            });
        };
        let result = finished.result.clone();
        // Built once, so a repeated wait reports what the first one did.
        if !self.reported {
            let mut errors = Vec::new();
            if let Err(containment) = &finished.teardown {
                errors.push(Error::Containment(Arc::clone(containment)));
            }
            errors.append(&mut finished.errors);
            self.failure = Error::join(errors).map(Arc::new);
            self.reported = true;
        }
        match &self.failure {
            None => Ok(result),
            Some(error) => Err(Partial {
                value: result,
                error: Shared::seam(error),
            }),
        }
    }

    fn stop(mut self: Box<Self>) -> Result<Stopped, togi_machine::Error> {
        outside_runtime("stop")?;
        if self.ended.is_none() {
            let _ = self.commands.send(TrialCommand::Stop);
            if !self.await_end(self.stop_bound, true, &mut Discard) {
                return Err(Error::Stuck {
                    operation: "the stop",
                    bound: self.stop_bound,
                }
                .into());
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
            None => Err(Error::RuntimeGone.into()),
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
