//! What a panic does to the real process: each case runs `togi run` in a child process, this test binary run
//! again on one test (`helper`), against the fakes of `support`. The child exits with `run_process`'s status.
//!
//! The panic hook ends the process for no panic but the owner's own, and that one only after its cleanup. A
//! panic on another thread is reported and left to unwind: a thread that contains it (a `catch_unwind`, a
//! tokio task) goes on, and a thread that does not ends alone, and whoever depended on it meets the failure
//! that ending causes, which the owner's cleanup handles.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead as _, BufReader, Write as _};
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use togi_journal::EventKind;
use togi_machine::{
    BootId, CancellationToken, CoreId, Error as MachineError, Kernel, KernelCursor, KernelRead,
    Machine, Mce, ObservedOffset, Partial, PstoreRecord, Reporter, ResetReason, Running, Smu,
    SmuValidated, Started, Stopped, Swept, TargetOffset, TrialResult, TrialSpec, Trials,
};

use crate::support::{Fake, simulator, written};

const SCENARIO: &str = "TOGI_RUN_TEST_SCENARIO";
const DIR: &str = "TOGI_RUN_TEST_DIR";
const ORDER: &str = "order.log";
const RELEASE: &str = "release";
const FALLBACK: &str = "printing events instead";
/// What the hook prints for the probe panic the cases raise while the dashboard still owns the screen: it holds
/// the line back until the dashboard has let go, so it comes after the fallback line and only once event lines
/// flow again.
const PROBE: &str = "togi: panic on thread probe: the dashboard has let go";

/// What the cases write down in the order it happened, for the parent to read.
#[derive(Clone)]
struct Order(Arc<Mutex<File>>);

impl Order {
    fn open(dir: &Path) -> Self {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(ORDER))
            .unwrap();
        Self(Arc::new(Mutex::new(file)))
    }

    fn note(&self, line: &str) {
        let mut file = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        writeln!(file, "{line}").unwrap();
    }
}

fn interrupt() {
    rustix::process::kill_process(rustix::process::getpid(), rustix::process::Signal::INT).unwrap();
}

struct Logging {
    inner: Box<dyn Smu>,
    order: Order,
    first_write: Option<Box<dyn FnMut()>>,
    /// Once set, every write is refused, as a restoration the hardware does not take.
    refuse: Option<Arc<AtomicBool>>,
}

impl Logging {
    fn written(&mut self) -> Result<(), MachineError> {
        if let Some(mut action) = self.first_write.take() {
            action();
        }
        if self
            .refuse
            .as_ref()
            .is_some_and(|refuse| refuse.load(Ordering::SeqCst))
        {
            self.order.note("refused");
            return Err(MachineError::Other("restoration write refused".into()));
        }
        self.order.note("write");
        Ok(())
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the fake records simulated SMU writes before delegation"
)]
impl Smu for Logging {
    fn offset(&mut self, core: CoreId) -> Result<ObservedOffset, MachineError> {
        self.inner.offset(core)
    }

    fn set_offset(&mut self, core: CoreId, offset: TargetOffset) -> Result<(), MachineError> {
        self.written()?;
        self.inner.set_offset(core, offset)
    }

    fn set_all_offsets(&mut self, offset: TargetOffset) -> Result<(), MachineError> {
        self.written()?;
        self.inner.set_all_offsets(offset)
    }

    fn emergency_zero(
        &mut self,
        validated: SmuValidated,
        stopped: &Stopped,
    ) -> Result<(), MachineError> {
        self.inner.emergency_zero(validated, stopped)
    }
}

/// An SMU whose worker thread dies of an uncaught panic when the first write reaches it: that write and every
/// later one fail, as the mailbox's do once its worker is lost.
struct LosesItsWorker {
    inner: Box<dyn Smu>,
    lost: bool,
}

impl LosesItsWorker {
    fn lose(&mut self) -> MachineError {
        if !self.lost {
            self.lost = true;
            let worker = thread::Builder::new()
                .name("test-worker".to_owned())
                .spawn(|| panic!("injected panic on a worker thread"))
                .unwrap();
            let _ = worker.join();
        }
        MachineError::Other("the worker is gone".into())
    }
}

impl Smu for LosesItsWorker {
    fn offset(&mut self, core: CoreId) -> Result<ObservedOffset, MachineError> {
        self.inner.offset(core)
    }

    fn set_offset(&mut self, _: CoreId, _: TargetOffset) -> Result<(), MachineError> {
        Err(self.lose())
    }

    fn set_all_offsets(&mut self, _: TargetOffset) -> Result<(), MachineError> {
        Err(self.lose())
    }

    fn emergency_zero(&mut self, _: SmuValidated, _: &Stopped) -> Result<(), MachineError> {
        Err(self.lose())
    }
}

#[derive(Clone, Copy, Default)]
struct Faults {
    /// Every write after the panic is refused.
    refuse_writes: bool,
    /// The trial's teardown cannot say it stopped the workload.
    unconfirmed_stop: bool,
}

/// Trials that panic once one runs at an offset the run wrote: a night starts with stock controls, which leave
/// nothing to restore.
struct PanickingTrials {
    inner: Box<dyn Trials>,
    order: Order,
    panicked: Arc<AtomicBool>,
    faults: Faults,
}

impl Trials for PanickingTrials {
    fn controller_pid(&self) -> u32 {
        togi_sim::CONTROLLER_PID
    }

    fn content(
        &mut self,
        workload: togi_machine::WorkloadId,
    ) -> Result<togi_machine::ContentId, MachineError> {
        self.inner.content(workload)
    }

    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, MachineError> {
        self.inner.sweep(cancel)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        let armed = written(&spec);
        let inner = self.inner.start(cancel, spec, ready)?;
        Ok(Box::new(PanickingRunning {
            inner,
            order: self.order.clone(),
            panicked: Arc::clone(&self.panicked),
            armed,
            faults: self.faults,
        }))
    }

    fn ended(&mut self, id: &str) -> Result<(), MachineError> {
        self.inner.ended(id)
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<
        dyn Iterator<Item = Result<togi_machine::TrialConditions, togi_machine::SampleError>> + 'a,
    > {
        self.inner.samples(id)
    }
}

struct PanickingRunning {
    inner: Box<dyn Running>,
    order: Order,
    panicked: Arc<AtomicBool>,
    /// Whether it runs at a profile the run wrote, so panics.
    armed: bool,
    faults: Faults,
}

impl Running for PanickingRunning {
    fn started(&self) -> Started {
        self.inner.started()
    }

    fn wait(
        &mut self,
        cancel: &CancellationToken,
        report: &mut dyn Reporter,
    ) -> Result<TrialResult, Partial<TrialResult>> {
        if !self.armed {
            return self.inner.wait(cancel, report);
        }
        self.panicked.store(true, Ordering::SeqCst);
        self.order.note("panic");
        panic!("injected panic while a trial runs");
    }

    fn stop(self: Box<Self>) -> Result<Stopped, MachineError> {
        if !self.armed {
            return self.inner.stop();
        }
        self.order.note("stop");
        let stopped = self.inner.stop()?;
        if self.faults.unconfirmed_stop {
            return Err(MachineError::Other(
                "the teardown could not be confirmed".into(),
            ));
        }
        Ok(stopped)
    }
}

fn logging(
    machine: Machine<'static>,
    order: &Order,
    first_write: Option<Box<dyn FnMut()>>,
) -> Machine<'static> {
    Machine {
        smu: Box::new(Logging {
            inner: machine.smu,
            order: order.clone(),
            first_write,
            refuse: None,
        }),
        ..machine
    }
}

/// What a panic in the trial leaves behind, with the order its teardown ran in noted by `order`.
fn panicking(machine: Machine<'static>, order: &Order, faults: Faults) -> Machine<'static> {
    let panicked = Arc::new(AtomicBool::new(false));
    let machine = Machine {
        smu: Box::new(Logging {
            inner: machine.smu,
            order: order.clone(),
            first_write: None,
            refuse: faults.refuse_writes.then(|| Arc::clone(&panicked)),
        }),
        ..machine
    };
    Machine {
        trials: Box::new(PanickingTrials {
            inner: machine.trials,
            order: order.clone(),
            panicked,
            faults,
        }),
        ..machine
    }
}

/// Blocks on the host lock from another descriptor, and when it is released notes whether the journal's lock
/// was already free: the journal must be closed before the host lock goes.
fn watch_the_locks(dir: &Path, order: &Order) {
    let host = File::open(dir.join("togi.lock")).unwrap();
    let journal = dir.join("state/lock");
    let order = order.clone();
    thread::spawn(move || {
        host.lock().unwrap();
        let line = match File::open(&journal).unwrap().try_lock() {
            Ok(()) => "journal_closed_before_host_lock_released",
            Err(_) => "journal_held_when_host_lock_released",
        };
        order.note(line);
    });
}

/// A dashboard whose drawing panics at once.
struct PanicsAtOnce;

impl togi_cli::dashboard::Surface for PanicsAtOnce {
    fn draw(
        &self,
        _: &Path,
        _: u64,
        _: &togi_cli::dashboard::Stop,
    ) -> Result<(), togi_watch::terminal::Error> {
        panic!("injected panic in the dashboard\x1b[31m");
    }
}

/// Waits until the parent says the dashboard's fallback was seen on stderr.
fn wait_for_release(dir: &Path) {
    let release = dir.join(RELEASE);
    let end = Instant::now() + Duration::from_secs(30);
    while !release.exists() {
        assert!(
            Instant::now() < end,
            "the fallback to event lines never reached stderr"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

/// A dashboard that says on stderr when it is told to stop.
struct SaysWhenItStops;

impl togi_cli::dashboard::Surface for SaysWhenItStops {
    fn draw(
        &self,
        _: &Path,
        _: u64,
        stop: &togi_cli::dashboard::Stop,
    ) -> Result<(), togi_watch::terminal::Error> {
        stop.wait();
        let _ = writeln!(std::io::stderr(), "dashboard stopped drawing");
        Ok(())
    }
}

/// A dashboard that draws until `cue`, then fails as a drawing that returns an error does.
struct FailsOnCue(Arc<AtomicBool>);

impl togi_cli::dashboard::Surface for FailsOnCue {
    fn draw(
        &self,
        _: &Path,
        _: u64,
        stop: &togi_cli::dashboard::Stop,
    ) -> Result<(), togi_watch::terminal::Error> {
        while !self.0.load(Ordering::SeqCst) {
            if stop.is_stopped() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(1));
        }
        Err(togi_watch::terminal::Error::Output(std::io::Error::other(
            "the dashboard fell over",
        )))
    }
}

/// Fails the dashboard and returns once the parent has seen the probe line, which the hook prints only after
/// the dashboard has let go of the screen: event lines flow again from then on, and the fallback line is out.
fn fail_the_dashboard(cue: &AtomicBool, dir: &Path) {
    let probe = thread::Builder::new()
        .name("probe".to_owned())
        .spawn(|| panic!("the dashboard has let go"))
        .unwrap();
    assert!(probe.join().is_err());
    cue.store(true, Ordering::SeqCst);
    wait_for_release(dir);
}

/// The kernel log of a run whose dashboard fails once the journal holds `kind`: on the first read after that,
/// or, with `at_drop`, when the session lets go of the machine, which is after its last event.
struct FailsTheDashboard {
    inner: Box<dyn Kernel>,
    journal: PathBuf,
    kind: &'static str,
    at_drop: bool,
    cue: Arc<AtomicBool>,
    dir: PathBuf,
    failed: bool,
}

impl FailsTheDashboard {
    fn fail_if_the_journal_holds_the_kind(&mut self) {
        if self.failed {
            return;
        }
        let text = fs::read_to_string(&self.journal).unwrap_or_default();
        if text.contains(self.kind) {
            self.failed = true;
            fail_the_dashboard(&self.cue, &self.dir);
        }
    }
}

impl Drop for FailsTheDashboard {
    fn drop(&mut self) {
        if self.at_drop {
            self.fail_if_the_journal_holds_the_kind();
        }
    }
}

impl Kernel for FailsTheDashboard {
    fn mces(&mut self, boot: &BootId, since: Duration) -> Result<Vec<Mce>, Partial<Vec<Mce>>> {
        self.inner.mces(boot, since)
    }

    fn read_mces(
        &mut self,
        boot: &BootId,
        after: Option<&KernelCursor>,
    ) -> Result<KernelRead, Partial<KernelRead>> {
        if !self.at_drop {
            self.fail_if_the_journal_holds_the_kind();
        }
        self.inner.read_mces(boot, after)
    }

    fn reset_reason(&mut self, boot: &BootId) -> Result<ResetReason, MachineError> {
        self.inner.reset_reason(boot)
    }

    fn reset_reason_after(&mut self, boot: &BootId) -> Result<ResetReason, MachineError> {
        self.inner.reset_reason_after(boot)
    }

    fn saved_pstore(&mut self, boot: &BootId) -> Result<Option<PstoreRecord>, MachineError> {
        self.inner.saved_pstore(boot)
    }
}

/// The soft limit on file sizes the process had before [`refuse_journal_growth`] lowered it.
static FSIZE: OnceLock<rustix::process::Rlimit> = OnceLock::new();

/// What the child writes once the limit is lifted again, to show it can: more than the journal ever held.
const GROWN_AFTER_THE_FAILURE: &str = "grown-after-the-failure";
const GROWN_LEN: usize = 1 << 20;

/// Makes the next append to the journal fail, as a full disk does: the process may not grow a file past the size
/// the journal has now. [`allow_growth_again`] lifts it.
fn refuse_journal_growth(journal: &Path) {
    use rustix::process::{Resource, Signal, getrlimit, setrlimit};
    use tokio::signal::unix::{SignalKind, signal};
    let (registered, ready) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let mut exceeded = signal(SignalKind::from_raw(Signal::XFSZ.as_raw())).unwrap();
            registered.send(()).unwrap();
            loop {
                exceeded.recv().await;
            }
        });
    });
    ready.recv().unwrap();
    let before = getrlimit(Resource::Fsize);
    FSIZE.set(before).unwrap();
    let mut limit = before;
    limit.current = Some(fs::metadata(journal).unwrap().len());
    setrlimit(Resource::Fsize, limit).unwrap();
}

/// Puts the limit [`refuse_journal_growth`] lowered back, once the failure it caused has been seen: the process
/// writes its coverage profile when it exits, and a profile cut short by the limit is corrupt.
fn allow_growth_again() {
    if let Some(limit) = FSIZE.get() {
        rustix::process::setrlimit(rustix::process::Resource::Fsize, *limit).unwrap();
    }
}

/// The child: runs `togi run` for the scenario named in the environment and exits with its status. Does nothing
/// when the environment names none, which is how an ordinary test run passes over it.
#[test]
fn real_process_helper() {
    let Ok(scenario) = std::env::var(SCENARIO) else {
        return;
    };
    let dir = PathBuf::from(std::env::var_os(DIR).unwrap());
    let order = Order::open(&dir);
    let mut fake = Fake::in_dir(simulator(1000), &dir);
    fake.real_stderr = true;
    match scenario.as_str() {
        "before_the_host_lock" => fake.panic_in_platform = true,
        "after_hardware_setup" => {
            fake.wrap = Box::new(move |machine, _| panicking(machine, &order, Faults::default()));
        }
        "failed_restoration" => {
            fake.wrap = Box::new(move |machine, _| {
                let faults = Faults {
                    refuse_writes: true,
                    ..Faults::default()
                };
                panicking(machine, &order, faults)
            });
        }
        "unconfirmed_teardown" => {
            fake.wrap = Box::new(move |machine, _| {
                let faults = Faults {
                    unconfirmed_stop: true,
                    ..Faults::default()
                };
                panicking(machine, &order, faults)
            });
        }
        "lock_order" => {
            let dir = dir.clone();
            fake.wrap = Box::new(move |machine, _| {
                watch_the_locks(&dir, &order);
                panicking(machine, &order, Faults::default())
            });
        }
        "contained_worker" => {
            fake.wrap = Box::new(move |machine, _| {
                let action: Box<dyn FnMut()> = Box::new(|| {
                    let worker = thread::Builder::new()
                        .name("test-worker".to_owned())
                        .spawn(|| {
                            panic::catch_unwind(AssertUnwindSafe(|| {
                                panic!("injected panic a worker thread contains")
                            }))
                        })
                        .unwrap();
                    assert!(worker.join().unwrap().is_err());
                    interrupt();
                });
                logging(machine, &order, Some(action))
            });
        }
        "tokio_task" => {
            fake.wrap = Box::new(move |machine, _| {
                let action: Box<dyn FnMut()> = Box::new(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .build()
                        .unwrap();
                    let task = thread::spawn(move || {
                        runtime.block_on(async {
                            tokio::spawn(async { panic!("injected panic in a tokio task") }).await
                        })
                    });
                    assert!(task.join().unwrap().is_err());
                    interrupt();
                });
                logging(machine, &order, Some(action))
            });
        }
        "lost_worker" => {
            fake.wrap = Box::new(move |machine, _| Machine {
                smu: Box::new(LosesItsWorker {
                    inner: machine.smu,
                    lost: false,
                }),
                ..machine
            });
        }
        "dashboard" => {
            fake.interactive = true;
            fake.surface = Arc::new(PanicsAtOnce);
            let dir = dir.clone();
            fake.wrap = Box::new(move |machine, _| {
                let dir = dir.clone();
                let action: Box<dyn FnMut()> = Box::new(move || {
                    wait_for_release(&dir);
                    interrupt();
                });
                logging(machine, &order, Some(action))
            });
        }
        "journal_failure" => {
            fake.interactive = true;
            fake.surface = Arc::new(SaysWhenItStops);
            let journal = dir.join("state/events.jsonl");
            fake.wrap = Box::new(move |machine, _| {
                let journal = journal.clone();
                let action: Box<dyn FnMut()> = Box::new(move || refuse_journal_growth(&journal));
                logging(machine, &order, Some(action))
            });
        }
        "fallback_between_the_closing_events" | "fallback_after_the_closing_events" => {
            let after = scenario == "fallback_after_the_closing_events";
            let cue = Arc::new(AtomicBool::new(false));
            fake.interactive = true;
            fake.surface = Arc::new(FailsOnCue(Arc::clone(&cue)));
            let dir = dir.clone();
            fake.wrap = Box::new(move |machine, _| {
                let machine = logging(machine, &order, Some(Box::new(interrupt)));
                Machine {
                    kernel: Box::new(FailsTheDashboard {
                        inner: machine.kernel,
                        journal: dir.join("state/events.jsonl"),
                        kind: if after {
                            "\"kind\":\"shutdown\""
                        } else {
                            "\"kind\":\"profile.restored\""
                        },
                        at_drop: after,
                        cue: Arc::clone(&cue),
                        dir: dir.clone(),
                        failed: false,
                    }),
                    ..machine
                }
            });
        }
        other => panic!("unknown scenario {other}"),
    }
    let args: Vec<OsString> = fake
        .arguments(&["run"])
        .into_iter()
        .map(Into::into)
        .collect();
    let exit = togi_cli::run_process(args, &fake);
    if FSIZE.get().is_some() {
        allow_growth_again();
        fs::write(dir.join(GROWN_AFTER_THE_FAILURE), vec![0_u8; GROWN_LEN]).unwrap();
    }
    let end = Instant::now() + Duration::from_secs(10);
    while scenario == "lock_order"
        && !fs::read_to_string(dir.join(ORDER))
            .is_ok_and(|text| text.contains("host_lock_released"))
        && Instant::now() < end
    {
        thread::sleep(Duration::from_millis(1));
    }
    std::process::exit(i32::from(exit.code()));
}

struct Child {
    status: Option<i32>,
    stderr: String,
    dir: TempDir,
}

impl Child {
    fn events(&self) -> Vec<EventKind> {
        togi_journal::Recording::read(&self.dir.path().join("state/events.jsonl"))
            .map(|recording| {
                recording
                    .events()
                    .iter()
                    .map(togi_journal::Event::kind)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn order(&self) -> Vec<String> {
        fs::read_to_string(self.dir.path().join(ORDER))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn lines(&self) -> Vec<&str> {
        self.stderr.lines().collect()
    }

    /// The index of the first stderr line containing `needle`.
    fn at(&self, needle: &str) -> usize {
        self.lines()
            .iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no line with {needle:?}: {}", self.stderr))
    }

    /// The index of the last stderr line containing `needle`.
    fn last_at(&self, needle: &str) -> usize {
        self.lines()
            .iter()
            .rposition(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no line with {needle:?}: {}", self.stderr))
    }

    fn count(&self, needle: &str) -> usize {
        self.lines()
            .iter()
            .filter(|line| line.contains(needle))
            .count()
    }
}

fn command(scenario: &str, dir: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "panics::real_process_helper",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(SCENARIO, scenario)
        .env(DIR, dir)
        .stdin(Stdio::null());
    command
}

#[expect(
    clippy::disallowed_methods,
    reason = "real_process tests capture the panic scenario child"
)]
fn run(scenario: &str) -> Child {
    let dir = TempDir::new().unwrap();
    let output = command(scenario, dir.path()).output().unwrap();
    Child {
        status: output.status.code(),
        stderr: String::from_utf8(output.stderr).unwrap(),
        dir,
    }
}

/// Runs a scenario that waits for the parent to see `line` on stderr before it goes on, so everything the child
/// prints after that line was printed while the session was still running.
#[expect(
    clippy::disallowed_methods,
    reason = "real_process tests coordinate the panic scenario child"
)]
fn run_released_by(scenario: &str, line: &str) -> Child {
    let dir = TempDir::new().unwrap();
    let mut child = command(scenario, dir.path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = String::new();
    let mut released = false;
    for text in BufReader::new(child.stderr.take().unwrap()).lines() {
        let text = text.unwrap();
        if !released && text.contains(line) {
            released = true;
            fs::write(dir.path().join(RELEASE), "").unwrap();
        }
        stderr.push_str(&text);
        stderr.push('\n');
    }
    Child {
        status: child.wait().unwrap().code(),
        stderr,
        dir,
    }
}

#[test]
fn real_process_a_panic_before_the_host_lock_exits_two_with_a_panic_line() {
    let child = run("before_the_host_lock");
    assert_eq!(child.status, Some(2), "{}", child.stderr);
    assert!(
        child
            .stderr
            .contains("panic: injected panic before the host lock\\x1b[31m"),
        "{}",
        child.stderr
    );
    assert!(!child.dir.path().join("togi.lock").exists());
    assert!(child.events().is_empty());
}

#[test]
fn real_process_a_panic_after_hardware_setup_runs_the_owner_cleanup_in_order_then_exits_two() {
    let child = run("after_hardware_setup");
    assert_eq!(child.status, Some(2), "{}", child.stderr);
    assert!(
        child
            .stderr
            .contains("panic: injected panic while a trial runs"),
        "{}",
        child.stderr
    );
    let order = child.order();
    let panicked = order.iter().position(|line| line == "panic").unwrap();
    let stopped = order.iter().position(|line| line == "stop").unwrap();
    let restored = order.iter().rposition(|line| line == "write").unwrap();
    assert!(
        panicked < stopped && stopped < restored,
        "the workload's teardown joined before the offsets were restored: {order:?}"
    );
    let events = child.events();
    assert!(events.contains(&EventKind::ProfileRestored), "{events:?}");
}

#[test]
fn real_process_a_panic_whose_cleanup_cannot_restore_the_offsets_reports_the_dead_end_and_still_exits_two()
 {
    let child = run("failed_restoration");
    assert_eq!(child.status, Some(2), "{}", child.stderr);
    assert!(
        child.order().contains(&"refused".to_owned()),
        "{:?}",
        child.order()
    );
    let events = child.events();
    assert!(events.contains(&EventKind::DeadEnd), "{events:?}");
    assert!(!events.contains(&EventKind::ProfileRestored), "{events:?}");
    assert!(
        child.at("togi: dead end smu:") < child.at("panic: injected panic while a trial runs"),
        "the dead end is reported before the panic ends the process: {}",
        child.stderr
    );
}

#[test]
fn real_process_a_panic_whose_teardown_cannot_be_confirmed_reports_it_and_the_dead_end_and_exits_two()
 {
    let child = run("unconfirmed_teardown");
    assert_eq!(child.status, Some(2), "{}", child.stderr);
    assert!(
        child.at("the teardown could not be confirmed")
            < child.at("panic: injected panic while a trial runs"),
        "{}",
        child.stderr
    );
    let events = child.events();
    assert!(!events.contains(&EventKind::ProfileRestored), "{events:?}");
    assert!(!events.contains(&EventKind::Shutdown), "{events:?}");
}

#[test]
fn real_process_a_panic_a_worker_thread_contains_does_not_end_the_run() {
    let child = run("contained_worker");
    assert_eq!(child.status, Some(0), "{}", child.stderr);
    assert!(
        child
            .stderr
            .contains("togi: panic on thread test-worker: injected panic a worker thread contains"),
        "the panic is reported even when its thread catches it: {}",
        child.stderr
    );
    assert!(
        !child.stderr.contains("panic: injected"),
        "{}",
        child.stderr
    );
    let events = child.events();
    assert!(events.contains(&EventKind::ProfileRestored), "{events:?}");
    assert!(events.contains(&EventKind::Shutdown), "{events:?}");
}

#[test]
fn real_process_a_panic_in_a_tokio_task_does_not_end_the_run() {
    let child = run("tokio_task");
    assert_eq!(child.status, Some(0), "{}", child.stderr);
    assert!(
        child
            .stderr
            .contains("panic on thread <unnamed>: injected panic in a tokio task"),
        "{}",
        child.stderr
    );
    let events = child.events();
    assert!(events.contains(&EventKind::ProfileRestored), "{events:?}");
    assert!(events.contains(&EventKind::Shutdown), "{events:?}");
}

#[test]
fn real_process_a_worker_that_dies_of_a_panic_ends_the_run_through_the_owner_not_the_hook() {
    let child = run("lost_worker");
    assert_eq!(
        child.status,
        Some(11),
        "the owner's dead end decides the status, not the hook's 2: {}",
        child.stderr
    );
    assert!(
        child
            .stderr
            .contains("togi: panic on thread test-worker: injected panic on a worker thread"),
        "{}",
        child.stderr
    );
    assert!(
        child.stderr.contains("togi: dead end smu:"),
        "{}",
        child.stderr
    );
    let events = child.events();
    assert!(events.contains(&EventKind::DeadEnd), "{events:?}");
}

#[test]
fn real_process_a_panic_in_the_dashboard_falls_back_to_event_lines_while_the_run_goes_on() {
    let child = run_released_by("dashboard", FALLBACK);
    assert_eq!(child.status, Some(0), "{}", child.stderr);
    assert!(
        child.stderr.contains(
            "togi run: dashboard: injected panic in the dashboard\\x1b[31m; printing events instead"
        ),
        "{}",
        child.stderr
    );
    // The run was stopped only after the fallback line had been read, at its first write, which follows the
    // night's stock controls: the last trial's end and everything after it came after the fallback, and every
    // trial's end is printed once as an event line by the gate. The trial's end is an event the summary of a
    // clean stop never prints: only the gate can have printed it.
    let fallback = child.at(FALLBACK);
    let ended = child
        .events()
        .iter()
        .filter(|kind| **kind == EventKind::TrialEnd)
        .count();
    assert!(ended > 0, "{}", child.stderr);
    assert_eq!(child.count(" trial.end "), ended, "{}", child.stderr);
    assert!(fallback < child.last_at(" trial.end "), "{}", child.stderr);
    assert_eq!(child.count(" profile.restored "), 1, "{}", child.stderr);
    assert!(
        fallback < child.at(" profile.restored "),
        "{}",
        child.stderr
    );
    assert_eq!(child.count(" shutdown "), 1, "{}", child.stderr);
    assert!(fallback < child.at(" shutdown "), "{}", child.stderr);
    assert!(child.events().contains(&EventKind::Shutdown));
}

#[test]
fn real_process_a_dashboard_that_gives_way_between_the_restoration_and_the_shutdown_shows_each_closing_line_once()
 {
    let child = run_released_by("fallback_between_the_closing_events", PROBE);
    assert_eq!(child.status, Some(0), "{}", child.stderr);
    // The restoration was recorded while the dashboard still showed, so the gate dropped its line and the run
    // prints it when it ends; the shutdown came after the fallback, so the gate printed it.
    assert_eq!(child.count(" profile.restored "), 1, "{}", child.stderr);
    assert_eq!(child.count(" shutdown "), 1, "{}", child.stderr);
    let fallback = child.at(FALLBACK);
    assert!(
        fallback < child.at(" profile.restored "),
        "{}",
        child.stderr
    );
    assert!(fallback < child.at(" shutdown "), "{}", child.stderr);
    let events = child.events();
    assert!(events.contains(&EventKind::ProfileRestored), "{events:?}");
    assert!(events.contains(&EventKind::Shutdown), "{events:?}");
}

#[test]
fn real_process_a_dashboard_that_gives_way_after_the_last_event_shows_both_closing_lines_once_in_order()
 {
    let child = run_released_by("fallback_after_the_closing_events", PROBE);
    assert_eq!(child.status, Some(0), "{}", child.stderr);
    assert_eq!(child.count(" profile.restored "), 1, "{}", child.stderr);
    assert_eq!(child.count(" shutdown "), 1, "{}", child.stderr);
    assert!(
        child.at(FALLBACK) < child.at(" profile.restored "),
        "{}",
        child.stderr
    );
    assert!(
        child.at(" profile.restored ") < child.at(" shutdown "),
        "{}",
        child.stderr
    );
}

#[test]
fn real_process_a_panic_closes_the_journal_before_the_host_lock_is_released() {
    let child = run("lock_order");
    assert_eq!(child.status, Some(2), "{}", child.stderr);
    let order = child.order();
    let stopped = order.iter().position(|line| line == "stop").unwrap();
    let restored = order.iter().rposition(|line| line == "write").unwrap();
    let released = order
        .iter()
        .position(|line| line == "journal_closed_before_host_lock_released")
        .unwrap();
    assert!(stopped < restored && restored < released, "{order:?}");
    assert!(
        order
            .iter()
            .any(|line| line == "journal_closed_before_host_lock_released"),
        "{order:?}"
    );
    assert!(
        !order
            .iter()
            .any(|line| line == "journal_held_when_host_lock_released"),
        "{order:?}"
    );
}

#[test]
fn real_process_the_emergency_report_of_a_journal_failure_is_printed_after_the_dashboard_stopped_drawing()
 {
    let child = run("journal_failure");
    assert_eq!(child.status, Some(1), "{}", child.stderr);
    let report = child.at("every core set to CO 0 without an intent (readback all 0)");
    assert!(
        child.at("dashboard stopped drawing") < report,
        "the report reached the screen the dashboard still owned: {}",
        child.stderr
    );
    assert_eq!(child.count("journal write failed"), 2, "{}", child.stderr);
    // The limit that made the append fail is lifted again before the child exits, so that it can write its
    // coverage profile, which a limit left in place cuts short.
    let grown = child.dir.path().join(GROWN_AFTER_THE_FAILURE);
    assert_eq!(
        fs::metadata(&grown).map(|meta| meta.len()).ok(),
        Some(GROWN_LEN as u64),
        "{}",
        child.stderr
    );
}
