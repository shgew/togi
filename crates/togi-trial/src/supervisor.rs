use std::any::Any;
use std::cell::RefCell;
use std::future::{Future, pending, poll_fn};
use std::iter::Peekable;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::pin::pin;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver as StdReceiver, SendError, Sender as StdSender, channel};
use std::task::Poll;
use std::time::Duration;

use togi_backend::Backend;
use togi_machine::{
    CancellationToken, CoreId, CoreInfo, OnCancel, Partial, PerCore, PmTableReader, Sample, Signal,
    Started, Stopped, Toggles, TrialConditions, TrialResult, TrialSpec, error_chain,
};
use tokio::sync::mpsc::{
    Receiver, Sender, UnboundedReceiver, UnboundedSender, channel as bounded, unbounded_channel,
};
use tokio::time::{Instant, sleep, sleep_until};

use crate::controller::{self, Controller, Moved};
use crate::error::{CleanupFailure, Containment, Error, InvalidTrial, OutputFailure, StorageError};
use crate::evidence::{Observation, Selector, Stream};
use crate::files::{self, Container};
use crate::id::ScopeName;
use crate::identity::{BackendIdentity, IdentityError};
use crate::instance::{Flow, Inbound, LiveInstance, Observed, Reader, Run, dispatch, poll_watched};
use crate::judge::Judge;
use crate::launch::{self, Confinement};
use crate::logs::{LogWriter, LogsEnd, Wrap};
use crate::os::{Child as _, GroupSignal, LaunchFailure, Os, SignalError};
use crate::plan::{InstancePlan, Load, TrialPlan};
use crate::prepare::Preparer;
use crate::procfs::ProcError;
use crate::runner::Backends;
use crate::samples::{Open, SamplesWriter};
use crate::schedule::{Action, Step, timeline};
use crate::sensors::{Roots, Sensors};
use crate::stall::Stall;
use crate::teardown::{Budget, Failures, Live, Teardown};
use crate::timing::Timing;
use crate::watch::Watched;

const READINESS_POLL: Duration = Duration::from_millis(10);
const INBOUND_CAPACITY: usize = 1024;
const EVENT_BATCH: usize = 64;

/// Everything a trial needs from the runner, owned by the runtime thread.
pub(crate) struct Env<O: Os> {
    pub(crate) os: Rc<O>,
    pub(crate) timing: Timing,
    pub(crate) roots: Roots,
    pub(crate) inputs: crate::procfs::Inputs,
    pub(crate) dir: PathBuf,
    pub(crate) backends: Backends,
    pub(crate) topology: Vec<CoreInfo>,
    pub(crate) identity: Option<BackendIdentity>,
    pub(crate) unscoped: bool,
    pub(crate) user_manager: bool,
    pub(crate) pm_table: Option<Arc<dyn PmTableReader + Send + Sync>>,
    /// Test construction: where samples go instead of the trial's `samples.jsonl`.
    pub(crate) storage: Option<Rc<dyn Fn() -> Result<Open, StorageError>>>,
    pub(crate) durability: crate::samples::Durability,
    /// The shared watched-file metadata of a descriptor-free fixture.
    pub(crate) memory: Option<crate::watch::Memory>,
    /// Test construction: what the output log thread writes to instead of the log files.
    pub(crate) log_sink: Option<Rc<dyn Fn() -> Wrap>>,
    /// Every supervisor started on this thread, so shutdown can have each tear its trial down.
    pub(crate) supervisors: RefCell<Registry>,
    /// How a start's file preparation runs, off this thread.
    pub(crate) preparer: Preparer,
    /// Moves togi's own threads off the cores a trial loads, and back.
    pub(crate) controller: Arc<dyn Controller>,
    pub(crate) frequency: Arc<crate::frequency::Limits>,
}

impl<O: Os> Env<O> {
    pub(crate) fn confinement(&self) -> Result<Confinement, Error> {
        if self.unscoped {
            return Ok(Confinement::Unscoped);
        }
        let identity = self.identity.ok_or(IdentityError::NotConfigured)?;
        Ok(Confinement::Scoped {
            identity,
            user_manager: self.user_manager,
        })
    }
}

/// A supervisor task the runtime thread keeps track of.
pub(crate) struct Supervised {
    pub(crate) trial: String,
    pub(crate) commands: UnboundedSender<TrialCommand>,
    pub(crate) task: tokio::task::JoinHandle<()>,
}

/// The supervisors still running, and whether shutdown has begun, so a supervisor a pending
/// start registers during shutdown is abandoned the moment it exists.
#[derive(Default)]
pub(crate) struct Registry {
    supervised: Vec<Supervised>,
    closed: bool,
}

impl Registry {
    fn register(&mut self, supervised: Supervised) {
        self.supervised
            .retain(|supervised| !supervised.task.is_finished());
        if self.closed {
            let _ = supervised.commands.send(TrialCommand::Abandon);
        }
        self.supervised.push(supervised);
    }

    /// Abandons every supervisor still running and takes them all, including those registered
    /// since an earlier close, which `register` already abandoned.
    pub(crate) fn close(&mut self) -> Vec<Supervised> {
        let already = std::mem::replace(&mut self.closed, true);
        let live: Vec<Supervised> = self
            .supervised
            .drain(..)
            .filter(|supervised| !supervised.task.is_finished())
            .collect();
        if !already {
            for supervised in &live {
                let _ = supervised.commands.send(TrialCommand::Abandon);
            }
        }
        live
    }
}

/// What the handle tells its supervisor.
#[derive(Debug)]
pub(crate) enum TrialCommand {
    /// A cancellation token fired.
    Cancel,
    Stop,
    /// The handle was dropped before a stop.
    Abandon,
}

/// What the supervisor tells the thread blocked in `wait`.
#[derive(Debug)]
pub(crate) enum Event {
    Progress(String),
    Sample(Sample),
    Signal {
        core: CoreId,
        signal: Signal,
        pid: Option<u32>,
        detail: String,
    },
    Finished(Box<Finished>),
}

/// The end of supervision and of teardown, sent exactly once.
#[derive(Debug)]
pub(crate) struct Finished {
    pub(crate) result: TrialResult,
    /// Runner errors that are not containment: cancellation, a failed load step, storage, output.
    pub(crate) errors: Vec<Error>,
    pub(crate) teardown: Result<Stopped, Arc<Containment>>,
    /// Whether the handle was dropped before this end was published, however far the teardown had
    /// got; the supervisor reported it on stderr because no caller is left to read it.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "the stderr report is its only consumer outside tests"
        )
    )]
    pub(crate) abandoned: bool,
}

/// The `Trial` side of one supervisor task.
pub(crate) struct Handle {
    pub(crate) commands: UnboundedSender<TrialCommand>,
    pub(crate) events: StdReceiver<Event>,
    pub(crate) started: Started,
    pub(crate) duration: Duration,
    pub(crate) cancelled: Arc<AtomicBool>,
    pub(crate) registration: OnCancel,
}

#[derive(Default)]
struct Launched {
    stops: u32,
    files: Vec<String>,
    argv: Vec<String>,
}

enum Readiness {
    Ready,
    TimedOut,
    Cancelled,
}

fn early(error: Error, ready: Stopped) -> Partial<Option<Stopped>> {
    Partial {
        value: Some(ready),
        error: error.into(),
    }
}

/// Placement failures withhold passing exposure without hiding stronger workload evidence.
#[derive(Default)]
struct PlacementEvidence {
    inconclusive: Option<String>,
    excluded: togi_machine::CoreSet,
}

fn confine<O: Os>(env: &Env<O>, spec: &TrialSpec, events: &StdSender<Event>) -> PlacementEvidence {
    let managed = matches!(
        spec.controller,
        togi_machine::ControllerMode::Managed { .. }
    );
    let placement = if managed {
        controller::placement(&env.topology, spec)
    } else {
        controller::select(&env.topology, &spec.cores, &spec.cpus, &spec.offsets).map(|cpus| {
            controller::Placement {
                cpus,
                host: None,
                capped: false,
                excluded: None,
            }
        })
    };
    let Some(placement) = placement else {
        return PlacementEvidence {
            inconclusive: managed
                .then(|| "managed controller placement could not select a host".to_owned()),
            excluded: togi_machine::CoreSet::default(),
        };
    };
    let mut evidence = PlacementEvidence::default();
    if placement.capped {
        let excluded_cpus = placement
            .excluded
            .and_then(|host| env.topology.iter().find(|info| info.core == host));
        let workers: Vec<_> = spec
            .cpus
            .iter()
            .copied()
            .filter(|cpu| !excluded_cpus.is_some_and(|info| info.cpus.contains(cpu)))
            .collect();
        let mut reports = Vec::new();
        let outcome = env.frequency.cap(&placement.cpus, &workers, &mut reports);
        for line in reports {
            let _ = events.send(Event::Progress(line));
        }
        if let Err(error) = outcome {
            evidence.inconclusive = Some(error_chain(&error));
        }
    }
    let moved = env.controller.confine(&placement.cpus);
    if managed && !moved.is_some_and(Moved::complete) {
        evidence.inconclusive.get_or_insert_with(|| {
            "managed controller placement did not confine every controller thread".to_owned()
        });
    }
    if let Some(moved) = moved {
        let line = if managed {
            format!(
                "togi controller CPUs {} while the trial loads {}; host {:?}; capped {}; {}",
                launch::cpus_list(&placement.cpus),
                launch::cpus_list(&spec.cpus),
                placement.host,
                placement.capped,
                moved.describe(),
            )
        } else if moved.complete() {
            format!(
                "togi's own threads run on CPUs {} while the trial loads {}: {}",
                launch::cpus_list(&placement.cpus),
                launch::cpus_list(&spec.cpus),
                moved.describe(),
            )
        } else {
            format!(
                "togi's own threads were not all confined to CPUs {} and may still run on a CPU \
                 the trial loads ({}): {}",
                launch::cpus_list(&placement.cpus),
                launch::cpus_list(&spec.cpus),
                moved.describe(),
            )
        };
        let _ = events.send(Event::Progress(line));
    }
    if let Some(reason) = &evidence.inconclusive {
        let _ = events.send(Event::Progress(format!(
            "controller placement inconclusive: {reason}"
        )));
    } else if let Some(host) = placement.excluded {
        evidence.excluded.insert(host);
        let _ = events.send(Event::Progress(format!(
            "controller host core {host} is excluded from passing exposure; its failure evidence remains"
        )));
    }
    evidence
}

/// Gives togi's threads back the affinity they had, once the teardown confirmed nothing runs.
fn restore<O: Os>(env: &Env<O>, events: &StdSender<Event>) -> Result<(), crate::frequency::Error> {
    let mut reports = Vec::new();
    let outcome = env.frequency.restore(&mut reports);
    for line in reports {
        let _ = events.send(Event::Progress(line));
    }
    if let Some(moved) = env.controller.restore() {
        let _ = events.send(Event::Progress(restored_line(moved)));
    }
    outcome
}

/// What a restoration reports; one that was not complete says the threads stay confined until the
/// next confirmed teardown or sweep retries it.
pub(crate) fn restored_line(moved: Moved) -> String {
    if moved.complete() {
        format!(
            "togi's own threads run on their own CPUs again: {}",
            moved.describe()
        )
    } else {
        format!(
            "togi's own threads were not all given their own CPUs back and stay confined until \
             a later confirmed teardown or sweep retries: {}",
            moved.describe()
        )
    }
}

/// Prepares, launches and begins supervising a trial. Timing starts when this returns.
/// On failure the same teardown ran; `Some` in the partial value proves nothing runs.
pub(crate) async fn start<O: Os>(
    env: Rc<Env<O>>,
    cancel: CancellationToken,
    spec: TrialSpec,
    ready: Stopped,
) -> Result<Handle, Partial<Option<Stopped>>> {
    // Every distinct backend the roles name must be configured and usable before anything starts.
    let mut checked: Vec<togi_machine::Backend> = Vec::new();
    for role in &spec.roles {
        let kind = role.workload.backend();
        if checked.contains(&kind) {
            continue;
        }
        let Some(backend) = env.backends.get(kind) else {
            return Err(early(Error::BackendMissing(kind), ready));
        };
        if let Err(error) = backend.check() {
            return Err(early(error.into(), ready));
        }
        checked.push(kind);
    }
    let confinement = match env.confinement() {
        Ok(confinement) => confinement,
        Err(error) => return Err(early(error, ready)),
    };
    let plan = match TrialPlan::build(&spec, &env.topology) {
        Ok(plan) => plan,
        Err(error) => return Err(early(error.into(), ready)),
    };
    let instances = plan.instance_count();
    // One backend per instance, indexed like the plan: each classifies its own instance's output.
    let backends: Vec<Arc<dyn Backend>> = match plan
        .instances
        .iter()
        .map(|instance| {
            env.backends
                .get(instance.workload.backend)
                .cloned()
                .ok_or(Error::BackendMissing(instance.workload.backend))
        })
        .collect::<Result<Vec<_>, Error>>()
    {
        Ok(backends) => backends,
        Err(error) => return Err(early(error, ready)),
    };
    if !env.timing.representable(plan.duration, instances) {
        return Err(early(InvalidTrial::UnrepresentableDuration.into(), ready));
    }
    let identity = match confinement {
        Confinement::Scoped { identity, .. } => Some(identity),
        Confinement::Unscoped => None,
    };
    let prepare_until = Instant::now() + env.timing.prepare;
    let (commands_tx, mut commands_rx) = unbounded_channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let registration = {
        let (commands, flag) = (commands_tx.clone(), Arc::clone(&cancelled));
        cancel.on_cancel(move || {
            flag.store(true, Ordering::Release);
            let _ = commands.send(TrialCommand::Cancel);
        })
    };
    let container = {
        let (dir, id) = (env.dir.clone(), plan.id.clone());
        let chown = env.preparer.chown.clone();
        let fs = env.preparer.fs.clone();
        let prepared = env
            .preparer
            .run(
                "the trial directory",
                prepare_until,
                env.timing.prepare,
                &mut commands_rx,
                &cancelled,
                move || {
                    let mut container = files::recreate_in(&dir, &id, identity, fs)?;
                    container.chown = chown;
                    Ok(container)
                },
            )
            .await;
        match prepared {
            Ok(container) => Arc::new(container),
            Err(error) => return Err(early(error, ready)),
        }
    };
    let logs = match LogWriter::spawn(env.log_sink.as_ref().map(|make| make())) {
        Ok(logs) => logs,
        Err(error) => return Err(early(Error::LogWriter(error), ready)),
    };
    drop(ready);

    let (events_tx, events_rx) = channel();
    let placement = confine(&env, &spec, &events_tx);
    let (inbound_tx, mut inbound_rx) = bounded(INBOUND_CAPACITY);
    let selector = Selector::mixed(
        &plan
            .instances
            .iter()
            .map(|instance| (instance.core, instance.cpus[0], instance.workload.backend))
            .collect::<Vec<_>>(),
    );
    let judge = Judge::new(selector, backends.clone(), events_tx.clone());
    let mut live = Live {
        instances: Vec::with_capacity(plan.instances.len()),
        scopes: Vec::new(),
        scoped: identity.is_some(),
        failures: Failures::default(),
    };
    let mut launched = Launched::default();
    let outcome = launch_all(
        &env,
        &plan,
        &container,
        confinement,
        &backends,
        prepare_until,
        &logs,
        &mut live,
        &inbound_tx,
        &mut commands_rx,
        &cancelled,
        &mut launched,
    )
    .await;
    if let Err(cause) = outcome {
        return Err(roll_back(&env, live, judge, &mut inbound_rx, logs, cause).await);
    }

    let started_at = Instant::now();
    let sensors =
        match Sensors::with_inputs(&env.roots, &env.topology, started_at, env.inputs.clone()) {
            Ok(sensors) => sensors,
            Err(error) => {
                return Err(
                    roll_back(&env, live, judge, &mut inbound_rx, logs, error.into()).await,
                );
            }
        };
    for instance in &mut live.instances {
        instance.observed.resumed = started_at;
        instance.observed.active = Duration::ZERO;
    }
    let started = describe(&plan, &live, launched.argv, launched.files);
    let duration = plan.duration;
    let (failed_tx, storage_failed) = unbounded_channel();
    let open = match &env.storage {
        Some(make) => make(),
        None => Ok(container.samples(env.durability)),
    };
    let notify = failed_tx.clone();
    let mut errors = Vec::new();
    let writer = match open.and_then(|open| SamplesWriter::spawn(open, failed_tx)) {
        Ok(writer) => Some(writer),
        Err(error) => {
            errors.push(Error::Storage(error.into()));
            let _ = notify.send(());
            None
        }
    };
    let toggles = Toggles {
        stops: launched.stops,
        conts: 0,
    };
    if plan.bursts() {
        let _ = events_tx.send(Event::Progress(
            "first half idle, then 100ms bursts every 2s, one core at a time".to_owned(),
        ));
    }
    let supervisor = Supervisor {
        sensors,
        timeline: timeline(&plan.loads(), plan.duration).peekable(),
        env: Rc::clone(&env),
        plan,
        live,
        judge,
        inbound: inbound_rx,
        commands: commands_rx,
        events: events_tx,
        cancelled: Arc::clone(&cancelled),
        writer,
        logs,
        storage_failed,
        started: started_at,
        toggles,
        tctl_max: None,
        errors,
        placement,
        rotation: 0,
        inbound_open: true,
    };
    let trial = supervisor.plan.id.to_string();
    let task = tokio::task::spawn_local(supervisor.run());
    env.supervisors.borrow_mut().register(Supervised {
        trial,
        commands: commands_tx.clone(),
        task,
    });
    Ok(Handle {
        commands: commands_tx,
        events: events_rx,
        started,
        duration,
        cancelled,
        registration,
    })
}

fn describe<O: Os>(
    plan: &TrialPlan,
    live: &Live<O>,
    argv: Vec<String>,
    files: Vec<String>,
) -> Started {
    let first = &live.instances[0];
    Started {
        pid: first.pid,
        scope: first.scope.to_string(),
        cpus: first.plan.cpus.clone(),
        argv,
        files,
        instances: live
            .instances
            .iter()
            .map(|instance| togi_machine::Instance {
                core: instance.plan.core,
                cpus: instance.plan.cpus.clone(),
                pid: instance.pid,
                scope: instance.scope.to_string(),
            })
            .collect(),
        // The first seeded step schedule: R3, R4 and R8's random light core have one.
        schedule: plan
            .instances
            .iter()
            .find_map(|instance| match instance.load {
                Load::Steps(schedule) => Some(schedule),
                Load::Continuous | Load::Burst { .. } | Load::Phases { .. } => None,
            }),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one launch pass over the shared start state"
)]
async fn launch_all<O: Os>(
    env: &Rc<Env<O>>,
    plan: &TrialPlan,
    container: &Arc<Container>,
    confinement: Confinement,
    backends: &[Arc<dyn Backend>],
    prepare_until: Instant,
    logs: &LogWriter,
    live: &mut Live<O>,
    inbound: &Sender<Inbound>,
    commands: &mut UnboundedReceiver<TrialCommand>,
    cancelled: &AtomicBool,
    launched: &mut Launched,
) -> Result<(), Error> {
    let identity = match confinement {
        Confinement::Scoped { identity, .. } => Some(identity),
        Confinement::Unscoped => None,
    };
    let scoped = identity.is_some();
    let owner = identity.map_or_else(
        || {
            if env.memory.is_some() {
                65534
            } else {
                rustix::process::geteuid().as_raw()
            }
        },
        BackendIdentity::uid,
    );
    let suspend_first = |instance: &InstancePlan| matches!(instance.load, Load::Burst { .. });
    for (index, instance) in plan.instances.iter().enumerate() {
        if cancelled.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        let label = instance.name.dir_name();
        let scope = ScopeName::for_instance(&plan.id, instance.name);
        let files = env
            .preparer
            .run(
                &label,
                prepare_until,
                env.timing.prepare,
                commands,
                cancelled,
                {
                    let (container, backend) =
                        (Arc::clone(container), Arc::clone(&backends[index]));
                    let (instance, workload) = (instance.clone(), instance.workload.clone());
                    move || container.instance(&instance, backend.as_ref(), &workload, identity)
                },
            )
            .await?;
        let watched = if let Some(dir) = &files.dir {
            files
                .plan
                .watched
                .iter()
                .map(|name| Watched::new(Arc::clone(dir), name.clone(), owner))
                .collect()
        } else {
            let memory =
                env.memory
                    .as_ref()
                    .ok_or_else(|| crate::error::PrepareError::Directory {
                        operation: "prepare watched output in",
                        path: files.path.clone(),
                        source: std::io::Error::other("memory preparation has no watched metadata"),
                    })?;
            files
                .plan
                .watched
                .iter()
                .map(|name| memory.watched(&files.path, name, owner))
                .collect()
        };
        let command = launch::command_for(
            confinement,
            &scope,
            &files.path,
            &instance.cpus,
            &files.plan,
        );
        if scoped {
            live.scopes.push(scope.clone());
        }
        let launch_error = |source| Error::Launch {
            instance: label.clone(),
            scope: scope.to_string(),
            source,
        };
        let (mut child, mut failed) = match env.os.launch(&command) {
            Ok(child) => (child, None),
            Err(LaunchFailure {
                child: Some(child),
                source,
            }) => (child, Some(launch_error(source))),
            Err(LaunchFailure {
                child: None,
                source,
            }) => return Err(launch_error(source)),
        };
        let pid = child.pid();
        let pipes = child.take_pipes();
        let child = Rc::new(child);
        let waiter = {
            let (child, inbound) = (Rc::clone(&child), inbound.clone());
            tokio::task::spawn_local(async move {
                let _ = child.exited().await;
                let _ = inbound.send(Inbound::Exited { instance: index }).await;
            })
        };
        let readers = match pipes {
            Ok(pipes) => [
                Reader::spawn(
                    index,
                    Stream::Stdout,
                    pipes.stdout,
                    logs.open(index, Stream::Stdout, files.stdout),
                    inbound.clone(),
                ),
                Reader::spawn(
                    index,
                    Stream::Stderr,
                    pipes.stderr,
                    logs.open(index, Stream::Stderr, files.stderr),
                    inbound.clone(),
                ),
            ],
            Err(source) => {
                live.failures.record(CleanupFailure::ReadPipe {
                    instance: label.clone(),
                    stream: "stdout.log and stderr.log",
                    source,
                });
                failed.get_or_insert(Error::PipeSetup {
                    instance: label.clone(),
                });
                [Reader::absent(), Reader::absent()]
            }
        };
        let cpus = instance.cpu_count;
        live.instances.push(LiveInstance {
            index,
            plan: instance.clone(),
            scope,
            pid,
            child: Some(child),
            waiter,
            readers,
            watched,
            writers: None,
            observed: Observed {
                run: Run::Running,
                ready: !scoped,
                exited: false,
                active: Duration::ZERO,
                resumed: Instant::now(),
                stall: Stall::new(env.timing.stall_grace, env.timing.stall_window, cpus),
            },
        });
        if let Some(error) = failed {
            return Err(error);
        }
        if index == 0 {
            launched.argv = std::iter::once(files.plan.program.as_os_str())
                .chain(files.plan.args.iter().map(std::ffi::OsString::as_os_str))
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
        }
        launched.files.extend(files.inputs);
        if suspend_first(instance) {
            let readiness = await_scope(env, &live.instances[index], commands).await;
            let now = Instant::now();
            let instance = &mut live.instances[index];
            if matches!(readiness, Readiness::Ready) {
                instance.observed.ready = true;
            }
            match instance.signal(GroupSignal::Stop) {
                Ok(()) | Err(SignalError::Gone) => {}
                Err(SignalError::Failed(source)) => {
                    return Err(Error::Signal {
                        instance: label,
                        signal: GroupSignal::Stop.name(),
                        source,
                    });
                }
            }
            instance.observed.suspend(now);
            launched.stops += 1;
            if matches!(readiness, Readiness::Cancelled) {
                return Err(Error::Cancelled);
            }
        }
    }
    Ok(())
}

/// Waits for the launcher to enter its scope, up to the stall grace but at most 30 s.
async fn await_scope<O: Os>(
    env: &Env<O>,
    instance: &LiveInstance<O>,
    commands: &mut UnboundedReceiver<TrialCommand>,
) -> Readiness {
    if instance.observed.ready {
        return Readiness::Ready;
    }
    let end = Instant::now() + env.timing.readiness();
    loop {
        if matches!(env.os.in_scope(instance.pid, &instance.scope), Ok(true)) {
            return Readiness::Ready;
        }
        tokio::select! {
            command = commands.recv() => {
                if matches!(command, Some(TrialCommand::Cancel)) {
                    return Readiness::Cancelled;
                }
            }
            () = sleep(READINESS_POLL) => {}
            () = sleep_until(end) => return Readiness::TimedOut,
        }
    }
}

/// Tears down a partly started trial with the same phases and budget as a finished one, then
/// finishes the output log writer within its bound and joins what it and the readers report with
/// the error that made the start fail.
async fn roll_back<O: Os>(
    env: &Rc<Env<O>>,
    live: Live<O>,
    mut judge: Judge,
    inbound: &mut Receiver<Inbound>,
    logs: LogWriter,
    cause: Error,
) -> Partial<Option<Stopped>> {
    let budget = Budget::start(env.timing);
    let ended = Teardown::begin(&env.os, inbound, &mut judge, live, budget)
        .reap()
        .await
        .drain()
        .await
        .verify()
        .await
        .finish()
        .await;
    let mut diagnostics = settle_logs(&mut judge, logs.finish(env.timing.writer_join).await);
    let panic = judge.take_panic();
    let invalid_affinity = judge.take_invalid_affinity();
    diagnostics.splice(0..0, judge.conclude().diagnostics);
    let (first, proof) = match ended.proof {
        Ok(stopped) => {
            if let Err(error) = env.frequency.restore(&mut diagnostics) {
                diagnostics.push(error_chain(&error));
            }
            if let Some(moved) = env.controller.restore()
                && !moved.complete()
            {
                diagnostics.push(restored_line(moved));
            }
            (cause, Some(stopped))
        }
        Err(containment) => (
            Error::Containment(Arc::new(containment.caused_by(cause))),
            None,
        ),
    };
    let mut later: Vec<Error> = invalid_affinity
        .into_iter()
        .map(Error::AffinityObservation)
        .collect();
    if let Some(what) = panic {
        later.push(Error::Panicked { what });
    }
    if !diagnostics.is_empty() {
        later.push(Error::Output(OutputFailure { diagnostics }));
    }
    Partial {
        value: proof,
        error: Error::join_after(first, later).into(),
    }
}

/// Hands the writer's failures to the evidence; a join that ran out of time is a diagnostic of its
/// own, returned for the caller to report.
fn settle_logs(judge: &mut Judge, ended: LogsEnd) -> Vec<String> {
    match ended {
        LogsEnd::Done(failures) => {
            for failure in failures {
                judge.observe(Observation::OutputError {
                    instance: failure.instance,
                    stream: failure.stream,
                    error: format!("write output log: {}", failure.error),
                });
            }
            Vec::new()
        }
        LogsEnd::TimedOut(bound) => vec![format!(
            "the output log writer did not finish within {bound:?}"
        )],
        LogsEnd::Vanished => vec!["the output log writer ended without reporting".to_owned()],
    }
}

/// Polls `work`, turning a panic in it into an `Err` the caller can still clean up after.
async fn contained<F: Future>(work: F) -> Result<F::Output, Box<dyn Any + Send>> {
    let mut work = pin!(work);
    poll_fn(
        |cx| match catch_unwind(AssertUnwindSafe(|| work.as_mut().poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(output)) => Poll::Ready(Ok(output)),
            Err(payload) => Poll::Ready(Err(payload)),
        },
    )
    .await
}

pub(crate) fn panic_text(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic without a message".to_owned())
}

enum End {
    Duration,
    Cancelled,
    Stopped,
    Abandoned,
    Evidence,
    Storage,
    Toggle(Error),
    /// Supervision panicked; the teardown still runs and proves or withholds the stop.
    Panicked(String),
}

struct Supervisor<O: Os> {
    env: Rc<Env<O>>,
    plan: TrialPlan,
    live: Live<O>,
    judge: Judge,
    inbound: Receiver<Inbound>,
    inbound_open: bool,
    commands: UnboundedReceiver<TrialCommand>,
    events: StdSender<Event>,
    cancelled: Arc<AtomicBool>,
    writer: Option<SamplesWriter>,
    logs: LogWriter,
    storage_failed: UnboundedReceiver<()>,
    sensors: Sensors,
    timeline: Peekable<Box<dyn Iterator<Item = Step>>>,
    started: Instant,
    toggles: Toggles,
    tctl_max: Option<i64>,
    errors: Vec<Error>,
    placement: PlacementEvidence,
    rotation: usize,
}

impl<O: Os> Supervisor<O> {
    async fn run(mut self) {
        let end = match contained(self.supervise()).await {
            Ok(end) => end,
            Err(payload) => End::Panicked(panic_text(payload.as_ref())),
        };
        self.finish(end).await;
    }

    /// Runs due work first on every turn, so a flood of output can neither delay the deadline nor
    /// skip a load step or a sampling tick; output is then taken in bounded batches.
    async fn supervise(&mut self) -> End {
        let sample = self.env.timing.sample;
        let deadline = self.started + self.plan.duration;
        let mut next_tick = self.started + sample;
        loop {
            if let Some(end) = self.due(deadline, &mut next_tick).await {
                return end;
            }
            let step_at = self.timeline.peek().map(|step| self.started + step.at);
            tokio::select! {
                biased;
                command = self.commands.recv() => return match command {
                    Some(TrialCommand::Cancel) => End::Cancelled,
                    Some(TrialCommand::Stop) => End::Stopped,
                    Some(TrialCommand::Abandon) | None => End::Abandoned,
                },
                Some(()) = self.storage_failed.recv() => return End::Storage,
                () = sleep_until(deadline) => return End::Duration,
                () = async {
                    match step_at {
                        Some(at) => sleep_until(at).await,
                        None => pending::<()>().await,
                    }
                } => {}
                () = sleep_until(next_tick) => {}
                event = self.inbound.recv(), if self.inbound_open => match event {
                    None => self.inbound_open = false,
                    Some(event) => {
                        if let Some(end) = self.batch(event) {
                            return end;
                        }
                    }
                },
            }
        }
    }

    /// Ends at the deadline, else sends every load step that is due and takes a due sample.
    async fn due(&mut self, deadline: Instant, next_tick: &mut Instant) -> Option<End> {
        let now = Instant::now();
        if now >= deadline {
            return Some(End::Duration);
        }
        while self
            .timeline
            .peek()
            .is_some_and(|step| self.started + step.at <= now)
        {
            if let Some(end) = self.step() {
                return Some(end);
            }
        }
        if now >= *next_tick {
            *next_tick = now + self.env.timing.sample;
            return self.tick().await;
        }
        None
    }

    /// Handles `first` and at most `EVENT_BATCH - 1` more queued events.
    fn batch(&mut self, first: Inbound) -> Option<End> {
        let mut event = Some(first);
        let mut handled = 0;
        while let Some(current) = event.take() {
            let flow = dispatch(
                &mut self.judge,
                &mut self.live.instances,
                &mut self.live.failures,
                current,
            );
            if let Some(end) = self.after(flow) {
                return Some(end);
            }
            handled += 1;
            if handled < EVENT_BATCH {
                event = self.inbound.try_recv().ok();
            }
        }
        None
    }

    /// An exit before teardown is unexpected, whichever instance it was.
    fn after(&mut self, flow: Flow) -> Option<End> {
        match flow {
            Flow::Go => None,
            Flow::Stop => Some(End::Evidence),
            Flow::Exited(instance) => {
                let detail = self.live.instances[instance]
                    .child
                    .as_ref()
                    .and_then(|child| child.exit().ok().flatten())
                    .map_or_else(|| "exited".to_owned(), |info| info.to_string());
                self.judge.observe(Observation::UnexpectedExit {
                    instance,
                    pid: Some(self.live.instances[instance].pid),
                    detail,
                });
                Some(End::Evidence)
            }
        }
    }

    fn step(&mut self) -> Option<End> {
        let step = self.timeline.next()?;
        let now = Instant::now();
        let signal = match step.action {
            Action::Stop => GroupSignal::Stop,
            Action::Cont => GroupSignal::Cont,
        };
        let instance = &mut self.live.instances[step.instance];
        match instance.signal(signal) {
            Ok(()) | Err(SignalError::Gone) => {}
            Err(SignalError::Failed(source)) => {
                return Some(End::Toggle(Error::Signal {
                    instance: instance.plan.name.dir_name(),
                    signal: signal.name(),
                    source,
                }));
            }
        }
        match step.action {
            Action::Stop => {
                instance.observed.suspend(now);
                self.toggles.stops += 1;
            }
            Action::Cont => {
                instance.observed.resume(now);
                self.toggles.conts += 1;
            }
        }
        None
    }

    async fn tick(&mut self) -> Option<End> {
        let now = Instant::now();
        let count = self.live.instances.len();
        let next_step = self.timeline.peek().map(|step| self.started + step.at);
        let deadline = self.started + self.plan.duration;
        let halt_at = [
            Some(deadline),
            Some(now + self.env.timing.sample),
            next_step,
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(now);
        let cancelled = Arc::clone(&self.cancelled);
        let mut stop = false;
        for step in 0..count {
            let index = (self.rotation + step) % count;
            let polled = poll_watched(
                &mut self.live.instances[index],
                &mut self.judge,
                &mut self.live.failures,
                halt_at,
                &mut || cancelled.load(Ordering::Acquire) || Instant::now() >= halt_at,
            )
            .await;
            stop |= polled.stop;
        }
        let mut workers = PerCore::<i64>::default();
        if !stop && !cancelled.load(Ordering::Acquire) {
            for step in 0..count {
                // A watched poll or an earlier sample may have used the rest of the duration; the
                // observations after it are no part of the trial, and they must not delay the
                // teardown or turn a late read failure into evidence.
                if Instant::now() >= deadline {
                    break;
                }
                let index = (self.rotation + step) % count;
                stop |= self.sample(index, now, &mut workers);
            }
        }
        self.rotation = self.rotation.wrapping_add(1);
        self.record(now, &workers);
        stop.then_some(End::Evidence)
    }

    /// Reads the required observations of one ready instance: its threads and its CPU time.
    fn sample(&mut self, index: usize, now: Instant, workers: &mut PerCore<i64>) -> bool {
        let env = Rc::clone(&self.env);
        let instance = &mut self.live.instances[index];
        if !instance.observed.ready {
            if !matches!(env.os.in_scope(instance.pid, &instance.scope), Ok(true)) {
                return false;
            }
            instance.observed.ready = true;
        }
        let pid = instance.pid;
        let mut stop = false;
        match env.os.threads(pid) {
            Ok(found) => {
                let outside = found
                    .threads
                    .iter()
                    .find(|thread| !instance.plan.cpus.contains(&thread.cpu));
                if let Some(thread) = outside {
                    stop |= self.judge.observe(Observation::Escape {
                        cpu: thread.cpu,
                        pid,
                        tid: thread.tid,
                    });
                }
                for error in found.errors {
                    self.judge.observe(Observation::SamplingLoss {
                        instance: index,
                        what: format!("thread sampling lost: {}", error_chain(&error)),
                    });
                }
            }
            Err(ProcError::Gone) => {}
            Err(error) => {
                self.judge.observe(Observation::SamplingLoss {
                    instance: index,
                    what: format!("thread sampling lost: {}", error_chain(&error)),
                });
            }
        }
        match env.os.usage(pid) {
            Ok(usage) => {
                // The two threads of an SMT core run as separate instances, so a core's time is
                // the sum of its instances'.
                if let Some(total) = sample_millis(usage.cpu_time).ok().and_then(|milliseconds| {
                    workers
                        .get(instance.plan.core)
                        .unwrap_or(0)
                        .checked_add(milliseconds)
                }) {
                    workers.set(instance.plan.core, total);
                } else {
                    let error = ProcError::CpuTimeOverflow { pid };
                    self.judge.observe(Observation::SamplingLoss {
                        instance: index,
                        what: format!("usage sampling lost: {}", error_chain(&error)),
                    });
                    return stop;
                }
                let active = instance.observed.active_at(now);
                if let Some(stalled) = instance.observed.stall.observe(active, usage.cpu_time) {
                    stop |= self.judge.observe(Observation::Stall {
                        instance: index,
                        pid,
                        cpu: usage.cpu,
                        detail: stalled.detail(),
                    });
                }
            }
            Err(ProcError::Gone) => {}
            Err(error) => {
                self.judge.observe(Observation::SamplingLoss {
                    instance: index,
                    what: format!("usage sampling lost: {}", error_chain(&error)),
                });
            }
        }
        stop
    }

    /// Hands one conditions sample to the writer without waiting for storage.
    fn record(&mut self, now: Instant, workers: &PerCore<i64>) {
        let readings = match self.sensors.read(now) {
            Ok(readings) => readings,
            Err(error) => {
                self.errors.push(error.into());
                return;
            }
        };
        let elapsed_ms = match sample_millis(now.saturating_duration_since(self.started)) {
            Ok(elapsed_ms) => elapsed_ms,
            Err(error) => {
                self.errors.push(Error::SampleElapsed(error));
                return;
            }
        };
        if let Some(tctl) = readings.tctl_c {
            self.tctl_max = Some(self.tctl_max.map_or(tctl, |max| max.max(tctl)));
        }
        let sample = TrialConditions {
            elapsed_ms,
            tctl_c: readings.tctl_c,
            tccd_c: readings.tccd_c,
            core_mhz: readings.core_mhz,
            worker_cpu_ms: *workers,
            package_power_w: readings.package_power_w,
            cores_power_w: readings.cores_power_w,
            pm_table: self
                .env
                .pm_table
                .as_ref()
                .and_then(|reader| reader.pm_table()),
        };
        if let Some(writer) = &self.writer {
            if let Err(error) = writer.offer(&sample) {
                self.errors.push(Error::Storage(error.into()));
            }
        }
    }

    async fn finish(self, end: End) {
        let Self {
            env,
            plan,
            mut live,
            mut judge,
            mut inbound,
            events,
            writer,
            logs,
            started,
            toggles,
            tctl_max,
            mut errors,
            mut commands,
            mut placement,
            ..
        } = self;
        for instance in &live.instances {
            if !instance.observed.ready && live.scoped {
                judge.observe(Observation::NeverEntered {
                    instance: instance.index,
                    scope: instance.scope.to_string(),
                });
            }
        }
        while let Ok(event) = inbound.try_recv() {
            let flow = dispatch(&mut judge, &mut live.instances, &mut live.failures, event);
            if let Flow::Exited(instance) = flow {
                let detail = live.instances[instance]
                    .child
                    .as_ref()
                    .and_then(|child| child.exit().ok().flatten())
                    .map_or_else(|| "exited".to_owned(), |info| info.to_string());
                judge.observe(Observation::UnexpectedExit {
                    instance,
                    pid: Some(live.instances[instance].pid),
                    detail,
                });
            }
        }
        let ran = Instant::now()
            .saturating_duration_since(started)
            .min(plan.duration);
        let abandoned_before = matches!(end, End::Abandoned);
        let mut panics = Vec::new();
        match end {
            End::Cancelled => errors.push(Error::Cancelled),
            End::Toggle(error) => errors.push(error),
            End::Panicked(what) => panics.push(what),
            End::Duration | End::Stopped | End::Abandoned | End::Evidence | End::Storage => {}
        }
        let budget = Budget::start(env.timing);
        let ended = Teardown::begin(&env.os, &mut inbound, &mut judge, live, budget)
            .reap()
            .await
            .drain()
            .await
            .verify()
            .await
            .finish()
            .await;
        if ended.proof.is_ok()
            && let Err(error) = restore(&env, &events)
        {
            let reason = error_chain(&error);
            let _ = events.send(Event::Progress(reason.clone()));
            placement.inconclusive.get_or_insert(reason);
        }
        panics.extend(judge.take_panic());
        for what in &panics {
            judge.observe(Observation::SamplingLoss {
                instance: 0,
                what: format!("trial supervision panicked: {what}"),
            });
            errors.push(Error::Panicked { what: what.clone() });
        }
        let bound = env.timing.writer_join;
        let samples = async {
            match writer {
                Some(writer) => writer.finish(bound).await,
                None => Ok(()),
            }
        };
        let (samples, logs) = tokio::join!(samples, logs.finish(bound));
        if let Err(failure) = samples {
            errors.push(Error::Storage(failure));
        }
        let late = settle_logs(&mut judge, logs);
        errors.extend(
            judge
                .take_invalid_affinity()
                .into_iter()
                .map(Error::AffinityObservation),
        );
        let mut verdict = judge.conclude();
        verdict.diagnostics.extend(late);
        if !verdict.diagnostics.is_empty() {
            errors.push(Error::Output(OutputFailure {
                diagnostics: verdict.diagnostics,
            }));
        }
        if plan.bursts() {
            let _ = events.send(Event::Progress(format!(
                "bursts: {} continues, {} stops",
                toggles.conts, toggles.stops
            )));
        }
        let result = TrialResult {
            ran,
            failure: verdict.failure,
            inconclusive: verdict
                .inconclusive
                .or(placement.inconclusive)
                .map(String::into_boxed_str),
            excluded: placement.excluded,
            escaped: verdict.escaped,
            tctl_max_c: tctl_max,
            stops: toggles.stops,
            conts: toggles.conts,
        };
        let abandoned = abandoned_before
            || std::iter::from_fn(|| commands.try_recv().ok())
                .any(|command| matches!(command, TrialCommand::Abandon));
        #[cfg(test)]
        if let Some(hook) = AFTER_ABANDON_SNAPSHOT.take() {
            hook();
        }
        let finished = Finished {
            result,
            errors,
            teardown: ended.proof.map_err(Arc::new),
            abandoned,
        };
        if abandoned {
            report_abandoned(&plan, &finished.teardown, &panics);
        }
        if let Err(SendError(Event::Finished(undelivered))) =
            events.send(Event::Finished(Box::new(finished)))
            && !abandoned
        {
            report_abandoned(&plan, &undelivered.teardown, &panics);
        }
    }
}

#[cfg(test)]
thread_local! {
    /// Runs once on the supervisor's thread right after it last looked for an abandonment and
    /// before it publishes its end: where a handle dropped on another thread can land.
    static AFTER_ABANDON_SNAPSHOT: std::cell::Cell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::Cell::new(None) };
    /// What `report_abandoned` would have written to stderr.
    pub(crate) static ABANDON_REPORTS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

#[cfg(test)]
pub(crate) fn after_abandon_snapshot(hook: impl FnOnce() + 'static) {
    AFTER_ABANDON_SNAPSHOT.set(Some(Box::new(hook)));
}

#[cfg(test)]
pub(crate) fn register<O: Os>(env: &Env<O>, supervised: Supervised) {
    env.supervisors.borrow_mut().register(supervised);
}

fn report_abandoned(
    plan: &TrialPlan,
    teardown: &Result<Stopped, Arc<Containment>>,
    panics: &[String],
) {
    let panic = panics.iter().fold(String::new(), |mut text, what| {
        text.push_str("; supervision panicked: ");
        text.push_str(what);
        text
    });
    announce(&match teardown {
        Ok(_) => format!(
            "togi: trial {} was dropped while running; torn down{panic}",
            plan.id
        ),
        Err(containment) => format!(
            "togi: trial {} was dropped while running; teardown unconfirmed: {}{panic}",
            plan.id,
            error_chain(containment)
        ),
    });
}

#[cfg(not(test))]
#[expect(
    clippy::print_stderr,
    reason = "an abandoned trial has no caller left to report to"
)]
fn announce(message: &str) {
    eprintln!("{message}");
}

#[cfg(test)]
fn announce(message: &str) {
    ABANDON_REPORTS.with_borrow_mut(|reports| reports.push(message.to_owned()));
}

fn sample_millis(duration: Duration) -> Result<i64, std::num::TryFromIntError> {
    i64::try_from(duration.as_millis())
}

#[cfg(test)]
mod numeric_tests {
    use super::sample_millis;
    use std::time::Duration;

    #[test_case::test_case(Duration::ZERO, Some(0); "zero")]
    #[test_case::test_case(Duration::from_millis(i64::MAX.cast_unsigned()), Some(i64::MAX); "largest exact milliseconds")]
    #[test_case::test_case(Duration::from_millis(i64::MAX.cast_unsigned() + 1), None; "first overflowing milliseconds")]
    #[test_case::test_case(Duration::MAX, None; "largest duration")]
    fn sampled_milliseconds_refuse_overflow(duration: Duration, expected: Option<i64>) {
        assert_eq!(sample_millis(duration).ok(), expected);
    }
}
