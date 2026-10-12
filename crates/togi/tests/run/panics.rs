//! What a panic does to the real process: each case runs `togi run` in a child process, this test binary run
//! again on one test (`helper`), against the fakes of `support`. The child exits with `run_process`'s status.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use tempfile::TempDir;
use togi_journal::EventKind;
use togi_machine::{
    CancellationToken, CoreId, Error as MachineError, Machine, ObservedOffset, Partial, Reporter,
    Running, Smu, SmuValidated, Started, Stopped, Swept, TargetOffset, TrialResult, TrialSpec,
    Trials,
};

use crate::support::{Fake, biased, interrupt_on_first_write};

const SCENARIO: &str = "TOGI_RUN_TEST_SCENARIO";
const DIR: &str = "TOGI_RUN_TEST_DIR";
const ORDER: &str = "order.log";

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
        file.sync_all().unwrap();
    }
}

struct Logging {
    inner: Box<dyn Smu>,
    order: Order,
    first_write: Option<Box<dyn FnMut()>>,
}

impl Logging {
    fn written(&mut self) {
        if let Some(mut action) = self.first_write.take() {
            action();
        }
        self.order.note("write");
    }
}

impl Smu for Logging {
    fn offset(&mut self, core: CoreId) -> Result<ObservedOffset, MachineError> {
        self.inner.offset(core)
    }

    fn set_offset(&mut self, core: CoreId, offset: TargetOffset) -> Result<(), MachineError> {
        self.written();
        self.inner.set_offset(core, offset)
    }

    fn set_all_offsets(&mut self, offset: TargetOffset) -> Result<(), MachineError> {
        self.written();
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

struct PanickingTrials {
    inner: Box<dyn Trials>,
    order: Order,
}

impl Trials for PanickingTrials {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, MachineError> {
        self.inner.sweep(cancel)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        let inner = self.inner.start(cancel, spec, ready)?;
        Ok(Box::new(PanickingRunning {
            inner,
            order: self.order.clone(),
        }))
    }

    fn ended(&mut self, id: &str) -> Result<(), MachineError> {
        self.inner.ended(id)
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<dyn Iterator<Item = togi_machine::TrialConditions> + 'a> {
        self.inner.samples(id)
    }
}

struct PanickingRunning {
    inner: Box<dyn Running>,
    order: Order,
}

impl Running for PanickingRunning {
    fn started(&self) -> Started {
        self.inner.started()
    }

    fn wait(
        &mut self,
        _: &CancellationToken,
        _: &mut dyn Reporter,
    ) -> Result<TrialResult, Partial<TrialResult>> {
        self.order.note("panic");
        panic!("injected panic while a trial runs");
    }

    fn stop(self: Box<Self>) -> Result<Stopped, MachineError> {
        self.order.note("stop");
        self.inner.stop()
    }
}

fn logging(machine: Machine, order: &Order, first_write: Option<Box<dyn FnMut()>>) -> Machine {
    Machine {
        smu: Box::new(Logging {
            inner: machine.smu,
            order: order.clone(),
            first_write,
        }),
        ..machine
    }
}

/// What a panic in the trial leaves behind, with the order its teardown ran in noted by `order`.
fn panicking(machine: Machine, order: &Order) -> Machine {
    let machine = logging(machine, order, None);
    Machine {
        trials: Box::new(PanickingTrials {
            inner: machine.trials,
            order: order.clone(),
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

/// A dashboard that panics once the journal records the run's baseline, so the events after it are the ones the
/// fallback to event lines must show.
struct PanicsOnceTheRunHasBegun;

impl togi_cli::dashboard::Surface for PanicsOnceTheRunHasBegun {
    fn draw(&self, state_dir: &Path, _: u64, _: &togi_cli::dashboard::Stop) -> Result<(), String> {
        let journal = state_dir.join("events.jsonl");
        while !fs::read_to_string(&journal).is_ok_and(|text| text.contains("session.baseline")) {
            thread::sleep(Duration::from_millis(1));
        }
        panic!("injected panic in the dashboard");
    }
}

fn block_forever() -> ! {
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}

/// The child: runs `togi run` for the scenario named in the environment and exits with its status. Does nothing
/// when the environment names none, which is how an ordinary test run passes over it.
#[test]
fn helper() {
    let Ok(scenario) = std::env::var(SCENARIO) else {
        return;
    };
    let dir = PathBuf::from(std::env::var_os(DIR).unwrap());
    let order = Order::open(&dir);
    let mut fake = Fake::in_dir(biased(1000, -20), &dir);
    fake.real_stderr = true;
    match scenario.as_str() {
        "before_the_host_lock" => fake.panic_in_platform = true,
        "after_hardware_setup" => {
            let order = order.clone();
            fake.wrap = Box::new(move |machine| panicking(machine, &order));
        }
        "lock_order" => {
            let order = order.clone();
            let dir = dir.clone();
            fake.wrap = Box::new(move |machine| {
                watch_the_locks(&dir, &order);
                panicking(machine, &order)
            });
        }
        "worker_thread" => {
            let order = order.clone();
            fake.wrap = Box::new(move |machine| {
                let action: Box<dyn FnMut()> = Box::new(|| {
                    let _ = thread::spawn(|| panic!("injected panic on a worker thread")).join();
                    block_forever();
                });
                logging(machine, &order, Some(action))
            });
        }
        "tokio_task" => {
            let order = order.clone();
            fake.wrap = Box::new(move |machine| {
                let action: Box<dyn FnMut()> = Box::new(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .build()
                        .unwrap();
                    let _ = thread::spawn(move || {
                        runtime.block_on(async {
                            let _ =
                                tokio::spawn(async { panic!("injected panic in a tokio task") })
                                    .await;
                        });
                    })
                    .join();
                    block_forever();
                });
                logging(machine, &order, Some(action))
            });
        }
        "dashboard" => {
            fake.interactive = true;
            fake.surface = Arc::new(PanicsOnceTheRunHasBegun);
            fake.wrap = Box::new(interrupt_on_first_write);
        }
        other => panic!("unknown scenario {other}"),
    }
    let args: Vec<OsString> = fake
        .arguments(&["run", "--cycles", "1"])
        .into_iter()
        .map(Into::into)
        .collect();
    let exit = togi_cli::run_process(args, &fake);
    let end = std::time::Instant::now() + Duration::from_secs(10);
    while scenario == "lock_order"
        && !fs::read_to_string(dir.join(ORDER))
            .is_ok_and(|text| text.contains("host_lock_released"))
        && std::time::Instant::now() < end
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
}

fn run(scenario: &str) -> Child {
    let dir = TempDir::new().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "panics::helper",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(SCENARIO, scenario)
        .env(DIR, dir.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    Child {
        status: output.status.code(),
        stderr: String::from_utf8(output.stderr).unwrap(),
        dir,
    }
}

#[test]
fn a_panic_before_the_host_lock_exits_two_with_a_panic_line() {
    let child = run("before_the_host_lock");
    assert_eq!(child.status, Some(2), "{}", child.stderr);
    assert!(
        child
            .stderr
            .contains("panic: injected panic before the host lock"),
        "{}",
        child.stderr
    );
    assert!(!child.dir.path().join("togi.lock").exists());
    assert!(child.events().is_empty());
}

#[test]
fn a_panic_after_hardware_setup_runs_the_owner_cleanup_in_order_then_exits_two() {
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
fn a_panic_on_a_worker_thread_exits_two_at_once_without_the_owner_cleanup() {
    let child = run("worker_thread");
    assert_eq!(child.status, Some(2), "{}", child.stderr);
    assert!(
        child
            .stderr
            .contains("panic: injected panic on a worker thread"),
        "{}",
        child.stderr
    );
    let events = child.events();
    assert!(!events.contains(&EventKind::ProfileRestored), "{events:?}");
    assert!(!events.contains(&EventKind::Shutdown), "{events:?}");
}

#[test]
fn a_panic_in_a_tokio_task_exits_two_at_once_without_the_owner_cleanup() {
    let child = run("tokio_task");
    assert_eq!(child.status, Some(2), "{}", child.stderr);
    assert!(
        child
            .stderr
            .contains("panic: injected panic in a tokio task"),
        "{}",
        child.stderr
    );
    let events = child.events();
    assert!(!events.contains(&EventKind::ProfileRestored), "{events:?}");
    assert!(!events.contains(&EventKind::Shutdown), "{events:?}");
}

#[test]
fn a_panic_in_the_dashboard_falls_back_to_event_lines_and_the_run_goes_on() {
    let child = run("dashboard");
    assert_eq!(child.status, Some(0), "{}", child.stderr);
    assert!(
        child.stderr.contains(
            "togi run: dashboard: injected panic in the dashboard; printing events instead"
        ),
        "{}",
        child.stderr
    );
    assert!(child.stderr.contains("shutdown"), "{}", child.stderr);
    assert!(child.events().contains(&EventKind::Shutdown));
}

#[test]
fn a_panic_closes_the_journal_before_the_host_lock_is_released() {
    let child = run("lock_order");
    assert_eq!(child.status, Some(2), "{}", child.stderr);
    let order = child.order();
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
