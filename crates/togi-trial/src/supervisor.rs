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
use std::sync::mpsc::{Receiver as StdReceiver, Sender as StdSender, channel};
use std::task::Poll;
use std::time::Duration;

use togi_backend::Backend;
use togi_machine::{
    CancellationToken, CoreId, CoreInfo, CpuId, OnCancel, Partial, PerCore, PmTableReader, Sample,
    Signal, Started, Stopped, Toggles, TrialConditions, TrialResult, TrialSpec,
};
use tokio::sync::mpsc::{
    Receiver, Sender, UnboundedReceiver, UnboundedSender, channel as bounded, unbounded_channel,
};
use tokio::time::{Instant, sleep, sleep_until};

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
use crate::plan::{Load, TrialPlan};
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
    pub(crate) dir: PathBuf,
    pub(crate) backends: Backends,
    pub(crate) topology: Vec<CoreInfo>,
    pub(crate) identity: Option<BackendIdentity>,
    pub(crate) unscoped: bool,
    pub(crate) user_manager: bool,
    pub(crate) pm_table: Option<Arc<dyn PmTableReader + Send + Sync>>,
    /// Test construction: where samples go instead of the trial's `samples.jsonl`.
    pub(crate) storage: Option<Rc<dyn Fn() -> Result<Open, StorageError>>>,
    /// Test construction: what the output log thread writes to instead of the log files.
    pub(crate) log_sink: Option<Rc<dyn Fn() -> Wrap>>,
    /// Every supervisor started on this thread, so shutdown can have each tear its trial down.
    pub(crate) supervisors: RefCell<Vec<Supervised>>,
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

/// Prepares, launches and begins supervising a trial. Timing starts when this returns.
/// On failure the same teardown ran; `Some` in the partial value proves nothing runs.
pub(crate) async fn start<O: Os>(
    env: Rc<Env<O>>,
    cancel: CancellationToken,
    spec: TrialSpec,
    ready: Stopped,
) -> Result<Handle, Partial<Option<Stopped>>> {
    let Some(backend) = env.backends.get(spec.workload.backend()).cloned() else {
        return Err(early(Error::BackendMissing(spec.workload.backend()), ready));
    };
    if let Err(error) = backend.check() {
        return Err(early(error.into(), ready));
    }
    let confinement = match env.confinement() {
        Ok(confinement) => confinement,
        Err(error) => return Err(early(error, ready)),
    };
    let plan = match TrialPlan::build(&spec, &env.topology) {
        Ok(plan) => plan,
        Err(error) => return Err(early(error.into(), ready)),
    };
    let instances = u32::try_from(plan.instances.len()).unwrap_or(u32::MAX);
    if !env.timing.representable(plan.duration, instances) {
        return Err(early(InvalidTrial::UnrepresentableDuration.into(), ready));
    }
    let identity = match confinement {
        Confinement::Scoped { identity, .. } => Some(identity),
        Confinement::Unscoped => None,
    };
    let container = match files::recreate(&env.dir, &plan.id, identity) {
        Ok(container) => container,
        Err(error) => return Err(early(error.into(), ready)),
    };
    let logs = match LogWriter::spawn(env.log_sink.as_ref().map(|make| make())) {
        Ok(logs) => logs,
        Err(error) => return Err(early(Error::LogWriter(error), ready)),
    };
    drop(ready);

    let (events_tx, events_rx) = channel();
    let (inbound_tx, mut inbound_rx) = bounded(INBOUND_CAPACITY);
    let (commands_tx, mut commands_rx) = unbounded_channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let registration = {
        let (commands, flag) = (commands_tx.clone(), Arc::clone(&cancelled));
        cancel.on_cancel(move || {
            flag.store(true, Ordering::Release);
            let _ = commands.send(TrialCommand::Cancel);
        })
    };
    let selector = Selector::new(
        plan.workload.backend,
        &plan
            .instances
            .iter()
            .map(|instance| (instance.core, instance.cpus[0]))
            .collect::<Vec<_>>(),
    );
    let judge = Judge::new(selector, Arc::clone(&backend), events_tx.clone());
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
        backend.as_ref(),
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
    for instance in &mut live.instances {
        instance.observed.resumed = started_at;
        instance.observed.active = Duration::ZERO;
    }
    let started = describe(&plan, &live, launched.argv, launched.files);
    let duration = plan.duration;
    let cores: Vec<CoreId> = plan
        .instances
        .iter()
        .map(|instance| instance.core)
        .collect();
    let (failed_tx, storage_failed) = unbounded_channel();
    let open = match &env.storage {
        Some(make) => make(),
        None => Ok(container.samples()),
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
    if let Load::Bursts { .. } = plan.load {
        let _ = events_tx.send(Event::Progress(
            "first half idle, then 100ms bursts every 2s, one core at a time".to_owned(),
        ));
    }
    let supervisor = Supervisor {
        sensors: Sensors::new(&env.roots, &env.topology, &cores, started_at),
        timeline: timeline(&plan.load, plan.duration).peekable(),
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
        rotation: 0,
        inbound_open: true,
    };
    let trial = supervisor.plan.id.to_string();
    let task = tokio::task::spawn_local(supervisor.run());
    let mut supervisors = env.supervisors.borrow_mut();
    supervisors.retain(|supervised| !supervised.task.is_finished());
    supervisors.push(Supervised {
        trial,
        commands: commands_tx.clone(),
        task,
    });
    drop(supervisors);
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
        schedule: match plan.load {
            Load::Steps(schedule) => Some(schedule),
            Load::Continuous | Load::Bursts { .. } => None,
        },
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one launch pass over the shared start state"
)]
async fn launch_all<O: Os>(
    env: &Rc<Env<O>>,
    plan: &TrialPlan,
    container: &Container,
    confinement: Confinement,
    backend: &dyn Backend,
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
    let suspend_first = matches!(plan.load, Load::Bursts { .. });
    for (index, instance) in plan.instances.iter().enumerate() {
        if cancelled.load(Ordering::Acquire) {
            return Err(Error::Cancelled);
        }
        let label = instance.name.dir_name();
        let scope = ScopeName::for_instance(&plan.id, instance.name, plan.per_core());
        let files = container.instance(instance, backend, &plan.workload, identity)?;
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
        let cpus = u32::try_from(instance.cpus.len()).unwrap_or(u32::MAX);
        live.instances.push(LiveInstance {
            index,
            plan: instance.clone(),
            scope,
            pid,
            child: Some(child),
            waiter,
            readers,
            watched: files
                .plan
                .watched
                .iter()
                .map(|name| Watched::new(Arc::clone(&files.dir), name.clone()))
                .collect(),
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
        if suspend_first {
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
        if env.os.in_scope(instance.pid, &instance.scope) == Ok(true) {
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
        .finish();
    let mut diagnostics = settle_logs(&mut judge, logs.finish(env.timing.writer_join).await);
    diagnostics.splice(0..0, judge.conclude().diagnostics);
    let (first, proof) = match ended.proof {
        Ok(stopped) => (cause, Some(stopped)),
        Err(containment) => (
            Error::Containment(Arc::new(containment.caused_by(cause))),
            None,
        ),
    };
    let mut later = Vec::new();
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

fn panic_text(payload: &(dyn Any + Send)) -> String {
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
            if let Some(end) = self.due(deadline, &mut next_tick) {
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
    fn due(&mut self, deadline: Instant, next_tick: &mut Instant) -> Option<End> {
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
            return self.tick();
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
                self.judge
                    .observe(Observation::UnexpectedExit { instance, detail });
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

    fn tick(&mut self) -> Option<End> {
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
                &mut || cancelled.load(Ordering::Acquire) || Instant::now() >= halt_at,
            );
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
            if env.os.in_scope(instance.pid, &instance.scope) != Ok(true) {
                return false;
            }
            instance.observed.ready = true;
        }
        let pid = instance.pid;
        let mut stop = false;
        match env.os.threads(pid) {
            Ok(found) => {
                let outside = found.threads.iter().find(|thread| {
                    !instance
                        .plan
                        .cpus
                        .iter()
                        .any(|cpu| u32::from(cpu.0) == thread.cpu)
                });
                if let Some(thread) = outside {
                    stop |= self.judge.observe(Observation::Escape {
                        cpu: CpuId(u16::try_from(thread.cpu).unwrap_or(u16::MAX)),
                        pid,
                        tid: thread.tid,
                    });
                }
                for error in found.errors {
                    self.judge.observe(Observation::SamplingLoss {
                        instance: index,
                        what: format!("thread sampling lost: {error}"),
                    });
                }
            }
            Err(crate::procfs::ProcError::Gone) => {}
            Err(error) => {
                self.judge.observe(Observation::SamplingLoss {
                    instance: index,
                    what: format!("thread sampling lost: {error}"),
                });
            }
        }
        match env.os.usage(pid) {
            Ok(usage) => {
                workers.set(
                    instance.plan.core,
                    i64::try_from(usage.cpu_time.as_millis()).unwrap_or(i64::MAX),
                );
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
            Err(crate::procfs::ProcError::Gone) => {}
            Err(error) => {
                self.judge.observe(Observation::SamplingLoss {
                    instance: index,
                    what: format!("usage sampling lost: {error}"),
                });
            }
        }
        stop
    }

    /// Hands one conditions sample to the writer without waiting for storage.
    fn record(&mut self, now: Instant, workers: &PerCore<i64>) {
        let readings = self.sensors.read(now);
        if let Some(tctl) = readings.tctl_c {
            self.tctl_max = Some(self.tctl_max.map_or(tctl, |max| max.max(tctl)));
        }
        let sample = TrialConditions {
            elapsed_ms: i64::try_from(now.saturating_duration_since(self.started).as_millis())
                .unwrap_or(i64::MAX),
            tctl_c: readings.tctl_c,
            tccd_c: readings.tccd_c,
            core_mhz: readings.core_mhz,
            worker_cpu_ms: *workers,
            package_power_w: readings.package_power_w,
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
                judge.observe(Observation::UnexpectedExit { instance, detail });
            }
        }
        let ran = Instant::now()
            .saturating_duration_since(started)
            .min(plan.duration);
        let abandoned_before = matches!(end, End::Abandoned);
        let mut panicked = None;
        match end {
            End::Cancelled => errors.push(Error::Cancelled),
            End::Toggle(error) => errors.push(error),
            End::Panicked(what) => {
                judge.observe(Observation::SamplingLoss {
                    instance: 0,
                    what: format!("trial supervision panicked: {what}"),
                });
                errors.push(Error::Panicked { what: what.clone() });
                panicked = Some(what);
            }
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
            .finish();
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
        let mut verdict = judge.conclude();
        verdict.diagnostics.extend(late);
        if !verdict.diagnostics.is_empty() {
            errors.push(Error::Output(OutputFailure {
                diagnostics: verdict.diagnostics,
            }));
        }
        if let Load::Bursts { .. } = plan.load {
            let _ = events.send(Event::Progress(format!(
                "bursts: {} continues, {} stops",
                toggles.conts, toggles.stops
            )));
        }
        let result = TrialResult {
            ran,
            failure: verdict.failure,
            inconclusive: verdict.inconclusive,
            escaped: verdict.escaped,
            tctl_max_c: tctl_max,
            stops: toggles.stops,
            conts: toggles.conts,
        };
        // The handle may have been dropped at any point of the teardown; nothing awaits between
        // this look at the commands and the publication, so the last answer is the one reported.
        let abandoned = abandoned_before
            || std::iter::from_fn(|| commands.try_recv().ok())
                .any(|command| matches!(command, TrialCommand::Abandon));
        if abandoned {
            report_abandoned(&plan, &ended.proof, panicked.as_deref());
        }
        let _ = events.send(Event::Finished(Box::new(Finished {
            result,
            errors,
            teardown: ended.proof.map_err(Arc::new),
            abandoned,
        })));
    }
}

#[expect(
    clippy::print_stderr,
    reason = "an abandoned trial has no caller left to report to"
)]
fn report_abandoned(
    plan: &TrialPlan,
    proof: &Result<Stopped, Containment>,
    panicked: Option<&str>,
) {
    let panic = panicked.map_or_else(String::new, |what| {
        format!("; supervision panicked: {what}")
    });
    match proof {
        Ok(_) => eprintln!(
            "togi: trial {} was dropped while running; torn down{panic}",
            plan.id
        ),
        Err(containment) => eprintln!(
            "togi: trial {} was dropped while running; teardown unconfirmed: {containment}{panic}",
            plan.id
        ),
    }
}
