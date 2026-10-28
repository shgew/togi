use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, sync_channel};
use std::time::{Duration, Instant};

use togi_backend::numbered_cpus;
use togi_machine::{
    CancellationToken, ContentId, Digest, OnCancel, Partial, Reporter, Running, Started, Stopped,
    Swept, TrialConditions, TrialResult, TrialSpec, Trials, WorkloadId,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::clock::Clock;
use crate::content::{bytes_digest, file_digest};
use crate::controller::Controller;
use crate::error::InvalidTrial;
use crate::error::{Error, Shared};
use crate::files::TrialsRoot;
use crate::id::TrialId;
use crate::runner::{Backends, Options};
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
    pub(crate) controller: Arc<dyn Controller>,
    pub(crate) frequency: Arc<dyn togi_machine::CpuFrequency>,
    pub(crate) durability: crate::samples::Durability,
    pub(crate) fs: Option<Arc<dyn togi_journal::fs::Fs>>,
    pub(crate) inputs: crate::procfs::Inputs,
    pub(crate) memory: Option<crate::watch::Memory>,
    pub(crate) clock: Clock,
}

/// Runs trials on one dedicated runtime thread; every launch happens there.
pub struct Runner {
    dir: PathBuf,
    timing: Timing,
    clock: Clock,
    runtime: Runtime,
    durability: crate::samples::Durability,
    fs: Option<Arc<dyn togi_journal::fs::Fs>>,
    backends: Backends,
    /// The digest of each executable already read, by path.
    executables: HashMap<PathBuf, Digest>,
}

struct MemoryCompression<'a>(&'a dyn togi_journal::fs::Fs);

impl togi_trialfiles::Storage for MemoryCompression<'_> {
    type Source = Box<dyn std::io::Read + Send>;
    type Destination = crate::files::MemorySink;

    fn instance_dirs(&mut self, dir: &std::path::Path) -> std::io::Result<Vec<PathBuf>> {
        let mut paths = self
            .0
            .read_dir(dir)?
            .into_iter()
            .filter(|entry| entry.kind == togi_journal::fs::FileKind::Dir)
            .map(|entry| dir.join(entry.name))
            .collect::<Vec<_>>();
        paths.sort();
        Ok(paths)
    }

    fn open_regular(
        &mut self,
        path: &std::path::Path,
    ) -> Result<Option<Self::Source>, togi_trialfiles::Error> {
        let error = |source| togi_trialfiles::Error::Open {
            path: path.to_owned(),
            source,
        };
        match self.0.kind(path).map_err(error)? {
            Some(togi_journal::fs::FileKind::File) => {
                self.0.open_read(path).map(Some).map_err(error)
            }
            Some(
                togi_journal::fs::FileKind::Dir
                | togi_journal::fs::FileKind::Symlink
                | togi_journal::fs::FileKind::Other,
            )
            | None => Ok(None),
        }
    }

    fn create(&mut self, path: &std::path::Path) -> std::io::Result<Self::Destination> {
        self.0.create_new(path).map(crate::files::MemorySink)
    }

    fn sync_file(&mut self, _: &mut Self::Destination) -> std::io::Result<()> {
        Ok(())
    }

    fn rename(&mut self, from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
        self.0.rename(from, to)
    }

    fn remove(&mut self, path: &std::path::Path) -> std::io::Result<()> {
        self.0.remove_file(path)
    }

    fn sync_dir(&mut self, _: &std::path::Path) -> std::io::Result<()> {
        Ok(())
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the explicitly supplied memory filesystem compresses disposable trial fixtures"
)]
fn compress_memory(
    fs: &dyn togi_journal::fs::Fs,
    dir: &std::path::Path,
) -> Result<(), togi_trialfiles::Errors> {
    togi_trialfiles::compress_with(dir, &mut MemoryCompression(fs))
}

impl Runner {
    pub fn new(options: Options) -> Result<Self, Error> {
        Self::new_with_durability(options, crate::samples::Durability::Durable)
    }

    /// Constructs a runner for disposable output without filesystem durability syscalls.
    ///
    /// This retains descriptor-relative safety and process containment, but a crash may lose
    /// completed samples, compressed files and the frequency recovery intent. Live tuning must use
    /// [`Self::new`].
    pub fn new_unsynced(options: Options) -> Result<Self, Error> {
        Self::new_with_durability(options, crate::samples::Durability::Unsynced)
    }

    fn new_with_durability(
        options: Options,
        durability: crate::samples::Durability,
    ) -> Result<Self, Error> {
        #[cfg(target_os = "linux")]
        {
            let user_manager = !rustix::process::geteuid().is_root();
            let settings = Settings {
                timing: Timing::default(),
                roots: Roots::default(),
                unscoped: false,
                user_manager,
                controller: Arc::new(crate::controller::Threads::over(crate::controller::Proc)),
                frequency: Arc::new(crate::frequency::Sysfs::default()),
                durability,
                fs: None,
                inputs: crate::procfs::Inputs::default(),
                memory: None,
                clock: Clock::System,
            };
            let proc = settings.roots.proc.clone();
            let inputs = settings.inputs.clone();
            Self::build(options, settings, move || {
                crate::os::linux::Linux::new(
                    crate::procfs::Proc::with_inputs(
                        proc,
                        crate::procfs::ticks_per_second(),
                        inputs,
                    ),
                    user_manager,
                )
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (options, durability);
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
        let clock = settings.clock.clone();
        let env_dir = dir.clone();
        let configured = backends.clone();
        let durability = settings.durability;
        let fs = settings.fs.clone();
        let timer_only = fs.is_some();
        let mut preparer = crate::prepare::Preparer::default();
        preparer.fs = settings.fs;
        let frequency = Arc::new(crate::frequency::Limits::new(
            settings.frequency,
            dir.clone(),
            fs.clone(),
            durability,
        ));
        let make_env = move || Env {
            os: std::rc::Rc::new(make_os()),
            timing: settings.timing,
            roots: settings.roots,
            inputs: settings.inputs,
            memory: settings.memory,
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
            preparer,
            controller: settings.controller,
            frequency,
            durability,
        };
        let runtime = if timer_only {
            Runtime::spawn_timer_only(make_env, clock.clone())?
        } else {
            Runtime::spawn(make_env, clock.clone())?
        };
        Ok(Self {
            backends: configured,
            executables: HashMap::new(),
            dir,
            timing,
            clock,
            runtime,
            durability,
            fs,
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
        match self.clock.recv_timeout(&answer, bound) {
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
        let instances = match crate::plan::checked_count(spec.cores.len()) {
            Ok(instances) => instances.max(1),
            Err(error) => {
                return Err(Partial {
                    value: Some(ready),
                    error: Error::Invalid(error).into(),
                });
            }
        };
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
        match self.clock.recv_timeout(&answer, bound) {
            Ok(Ok(delivered)) => Ok(Box::new(Trial::new(
                delivered.claim(),
                &self.timing,
                self.clock.clone(),
            ))),
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
        if let Some(fs) = &self.fs {
            return compress_memory(&**fs, &self.dir.join(id.as_str()))
                .map_err(|errors| Error::Compress(errors).into());
        }
        let root = TrialsRoot::open(&self.dir, false).map_err(Error::Prepare)?;
        let trial = root.trial(&id).map_err(Error::Prepare)?;
        let compress = match self.durability {
            crate::samples::Durability::Durable => compress_pinned,
            crate::samples::Durability::Unsynced => compress_pinned_unsynced,
        };
        compress(&trial).map_err(|errors| Error::Compress(errors).into())
    }

    /// Synchronous like `ended`, which also reads the disk on the calling thread. The read is
    /// bounded: each distinct executable is hashed once for the runner's life and every later
    /// call is answered from memory.
    fn content(&mut self, workload: WorkloadId) -> Result<ContentId, togi_machine::Error> {
        outside_runtime("content")?;
        let workload = workload.runs().workload();
        let kind = workload.backend;
        let backend = self.backends.get(kind).ok_or(Error::BackendMissing(kind))?;
        let configuration = backend.configuration(workload).map_err(Error::Backend)?;
        // The program is the same whatever the directory and CPUs, so any plan names it.
        let program = backend
            .plan(
                workload,
                std::path::Path::new("/"),
                &numbered_cpus(workload.threads),
            )
            .map_err(Error::Backend)?
            .program;
        let executable = if let Some(digest) = self.executables.get(&program) {
            digest.clone()
        } else {
            let digest =
                file_digest(self.fs.as_deref(), &program).map_err(|source| Error::Executable {
                    path: program.clone(),
                    source,
                })?;
            self.executables.insert(program, digest.clone());
            digest
        };
        Ok(ContentId {
            backend: kind,
            executable,
            configuration: bytes_digest(&configuration),
        })
    }

    /// The managed controller is this process: placement confines its own threads.
    fn controller_pid(&self) -> u32 {
        std::process::id()
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<dyn Iterator<Item = Result<TrialConditions, togi_machine::SampleError>> + 'a> {
        let reader = (|| {
            let id = TrialId::parse(id).map_err(std::io::Error::other)?;
            if let Some(fs) = &self.fs {
                let path = self.dir.join(id.as_str());
                return match fs.open_read(&path.join(togi_trialfiles::SAMPLES)) {
                    Ok(reader) => Ok(reader as Box<dyn std::io::Read>),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs
                        .open_read(&path.join(format!("{}.gz", togi_trialfiles::SAMPLES)))
                        .map(|reader| {
                            Box::new(flate2::read::MultiGzDecoder::new(reader))
                                as Box<dyn std::io::Read>
                        }),
                    Err(error) => Err(error),
                };
            }
            let root = TrialsRoot::open(&self.dir, false).map_err(|error| {
                let kind = if let crate::error::PrepareError::Directory { source, .. } = &error {
                    source.kind()
                } else {
                    std::io::ErrorKind::Other
                };
                std::io::Error::new(kind, error)
            })?;
            root.samples(&id)
        })();
        Box::new(togi_machine::ReadSamples::from_open(reader))
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
    clock: Clock,
    bound: Duration,
    stop_bound: Duration,
}

impl Trial {
    pub(crate) fn new(handle: Handle, timing: &Timing, clock: Clock) -> Self {
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
            created: clock.now(),
            clock,
            bound: timing.wait_bound(duration),
            stop_bound: timing.stop_bound(),
        }
    }

    #[cfg(test)]
    pub(crate) fn wait_budget(&self) -> Duration {
        self.bound
            .saturating_sub(self.clock.now().duration_since(self.created))
    }

    /// Waits up to `budget` for the supervisor's end, handing reports to `report`; with `block`
    /// false it takes only what is already queued. False when the end never came.
    fn await_end(&mut self, budget: Duration, block: bool, report: &mut dyn Reporter) -> bool {
        let begun = self.clock.now();
        while self.ended.is_none() {
            let elapsed = self.clock.now().duration_since(begun);
            if elapsed >= budget {
                return false;
            }
            let event = if block {
                self.clock
                    .recv_timeout(&self.events, budget.saturating_sub(elapsed))
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
                    pid,
                    detail,
                }) => report.signal(core, signal, pid, &detail),
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

/// Compresses the pinned directory without syncing disposable output.
#[expect(
    clippy::disallowed_methods,
    reason = "explicit unsynced runner construction selects disposable compressed output"
)]
pub(crate) fn compress_pinned_unsynced(
    trial: &std::os::fd::OwnedFd,
) -> Result<(), togi_trialfiles::Errors> {
    use std::os::fd::AsRawFd as _;
    togi_trialfiles::compress_unsynced(&PathBuf::from(format!(
        "/proc/self/fd/{}",
        trial.as_raw_fd()
    )))
}

struct Discard;

impl Reporter for Discard {
    fn progress(&mut self, _: &str) {}
    fn sample(&mut self, _: togi_machine::Sample) {}
    fn signal(
        &mut self,
        _: togi_machine::CoreId,
        _: togi_machine::Signal,
        _: Option<u32>,
        _: &str,
    ) {
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
        let remaining = self
            .bound
            .saturating_sub(self.clock.now().duration_since(self.created));
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

#[cfg(test)]
mod tests {
    #[test]
    fn the_disallowed_unsynced_runner_constructor_is_never_executed() {
        #[expect(
            clippy::disallowed_methods,
            reason = "pins the disposable runner tripwire without creating a runtime or files"
        )]
        fn unsynced(options: super::Options) -> Result<super::Runner, super::Error> {
            super::Runner::new_unsynced(options)
        }
        let _ = unsynced;
    }
}
