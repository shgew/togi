//! Ctrl-C at a terminal: `run` shows the dashboard on a pseudo-terminal, the dashboard closes, and SIGINT still
//! reaches `run`'s own listener, which stops the session as on any SIGINT. The child is this test binary run
//! again on one test (`helper`), as in `panics`.

use std::ffi::OsString;
use std::fs::File;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{LocalModes, tcgetattr};
use tempfile::TempDir;
use togi_cli::dashboard::Stop;
use togi_journal::{EventKind, Payload, ShutdownReason};
use togi_machine::{
    CancellationToken, Error as MachineError, Machine, Partial, Reporter, Running, Started,
    Stopped, Swept, TrialResult, TrialSpec, Trials,
};

use crate::support::{Fake, Scripted, simulator};

const SCENARIO: &str = "TOGI_RUN_TEST_PTY";
const DIR: &str = "TOGI_RUN_TEST_PTY_DIR";
const BOUND: Duration = Duration::from_secs(30);

/// A trial that runs until the run is cancelled.
struct Endless(Box<dyn Trials>);

struct EndlessRunning(Box<dyn Running>);

impl Trials for Endless {
    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, MachineError> {
        self.0.sweep(cancel)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        Ok(Box::new(EndlessRunning(self.0.start(cancel, spec, ready)?)))
    }

    fn ended(&mut self, id: &str) -> Result<(), MachineError> {
        self.0.ended(id)
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<dyn Iterator<Item = togi_machine::TrialConditions> + 'a> {
        self.0.samples(id)
    }
}

impl Running for EndlessRunning {
    fn started(&self) -> Started {
        self.0.started()
    }

    fn wait(
        &mut self,
        cancel: &CancellationToken,
        report: &mut dyn Reporter,
    ) -> Result<TrialResult, Partial<TrialResult>> {
        cancel.wait();
        self.0.wait(cancel, report)
    }

    fn stop(self: Box<Self>) -> Result<Stopped, MachineError> {
        self.0.stop()
    }
}

#[test]
fn helper() {
    if std::env::var_os(SCENARIO).is_none() {
        return;
    }
    let dir = PathBuf::from(std::env::var_os(DIR).unwrap());
    let mut fake = Fake::in_dir(simulator(1000), &dir);
    fake.real_stderr = true;
    fake.interactive = true;
    fake.surface = std::sync::Arc::new(Scripted(|_: &Stop| Ok(())));
    fake.wrap = Box::new(|machine| Machine {
        trials: Box::new(Endless(machine.trials)),
        ..machine
    });
    let args: Vec<OsString> = fake
        .arguments(&["run"])
        .into_iter()
        .map(Into::into)
        .collect();
    let exit = togi_cli::run_process(args, &fake);
    std::process::exit(i32::from(exit.code()));
}

fn open_pty() -> (OwnedFd, File) {
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let slave = PathBuf::from(OsString::from(
        String::from_utf8(ptsname(&master, Vec::new()).unwrap().into_bytes()).unwrap(),
    ));
    let slave = File::options().read(true).write(true).open(slave).unwrap();
    (master, slave)
}

fn events_of(dir: &Path) -> Vec<togi_journal::Event> {
    togi_journal::Recording::read(&dir.join("state/events.jsonl"))
        .map(togi_journal::Recording::into_events)
        .unwrap_or_default()
}

fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + BOUND;
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn ctrl_c_reaches_runs_own_listener_after_the_dashboard_closed_and_the_session_stops() {
    let dir = TempDir::new().unwrap();
    let (master, slave) = open_pty();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["pty::helper", "--exact", "--nocapture", "--test-threads=1"])
        .env(SCENARIO, "1")
        .env(DIR, dir.path())
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::null())
        .stderr(Stdio::from(slave))
        .spawn()
        .unwrap();

    wait_until("a trial to run", || {
        events_of(dir.path())
            .iter()
            .any(|event| event.kind() == EventKind::TrialStart)
    });
    let quiet = tcgetattr(&master).unwrap();
    assert!(
        !quiet.local_modes.contains(LocalModes::ECHO)
            && !quiet.local_modes.contains(LocalModes::ICANON),
        "the dashboard turns echo and line buffering off"
    );
    assert!(
        quiet.local_modes.contains(LocalModes::ISIG),
        "the signal keys stay on, so Ctrl-C still stops run"
    );

    kill_process(Pid::from_child(&child), Signal::INT).unwrap();
    let status = wait_for_exit(&mut child);
    assert_eq!(status.code(), Some(0));

    let events = events_of(dir.path());
    assert!(matches!(
        &events.last().unwrap().payload,
        Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::Signal
    ));
    let restored = tcgetattr(&master).unwrap();
    assert!(
        restored.local_modes.contains(LocalModes::ECHO)
            && restored.local_modes.contains(LocalModes::ICANON),
        "the previous settings are back once run stops"
    );
}

fn wait_for_exit(child: &mut std::process::Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + BOUND;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            panic!("run did not stop after SIGINT");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
