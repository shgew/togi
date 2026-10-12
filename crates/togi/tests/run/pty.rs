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
/// The file in [`DIR`] that lets a `real-panic` trial panic.
const GO: &str = "panic-now";

/// A trial that runs until the run is cancelled, or panics as [`Panic`] says.
struct Endless(Box<dyn Trials>, Panic);

struct EndlessRunning(Box<dyn Running>, Panic);

/// When a trial's wait panics.
#[derive(Clone)]
enum Panic {
    Never,
    Now,
    /// Once the file exists: the test makes it after it saw the dashboard draw.
    Once(PathBuf),
}

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
        Ok(Box::new(EndlessRunning(
            self.0.start(cancel, spec, ready)?,
            self.1.clone(),
        )))
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
        match &self.1 {
            Panic::Never => {}
            Panic::Now => panic!("injected panic while the dashboard shows"),
            Panic::Once(go) => {
                wait_until("the test to see the dashboard", || go.exists());
                panic!("injected panic while the dashboard shows");
            }
        }
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
    let scenario = std::env::var(SCENARIO).unwrap_or_default();
    if matches!(scenario.as_str(), "real" | "real-panic") {
        fake.surface = std::sync::Arc::new(togi_cli::dashboard::Watch);
    } else {
        fake.surface = std::sync::Arc::new(Scripted(|_: &Stop| Ok(())));
    }
    let panic = match scenario.as_str() {
        "panic" => Panic::Now,
        "real-panic" => Panic::Once(dir.join(GO)),
        _ => Panic::Never,
    };
    fake.wrap = Box::new(move |machine| Machine {
        trials: Box::new(Endless(machine.trials, panic.clone())),
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
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
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
        .env(SCENARIO, "run")
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

#[test]
fn a_session_panic_stops_the_dashboard_and_gives_the_terminal_back_before_the_panic_propagates() {
    let dir = TempDir::new().unwrap();
    let (master, slave) = open_pty();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["pty::helper", "--exact", "--nocapture", "--test-threads=1"])
        .env(SCENARIO, "panic")
        .env(DIR, dir.path())
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::null())
        .stderr(Stdio::from(slave))
        .spawn()
        .unwrap();
    let status = wait_for_exit(&mut child);
    assert_eq!(status.code(), Some(2));
    let restored = tcgetattr(&master).unwrap();
    assert!(
        restored.local_modes.contains(LocalModes::ECHO)
            && restored.local_modes.contains(LocalModes::ICANON),
        "the previous input settings are back"
    );
    let mut chunk = [0_u8; 4096];
    let read = rustix::io::read(&master, &mut chunk).unwrap();
    let seen = chunk[..read].to_vec();
    assert!(
        String::from_utf8_lossy(&seen).contains("panic: injected panic while the dashboard shows"),
        "{}",
        String::from_utf8_lossy(&seen)
    );
}

const HIDE_CURSOR: &[u8] = b"\x1b[?25l";
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn ctrl_c_stops_a_run_that_shows_the_real_dashboard_and_the_cursor_comes_back() {
    let dir = TempDir::new().unwrap();
    let (master, slave) = open_pty();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["pty::helper", "--exact", "--nocapture", "--test-threads=1"])
        .env(SCENARIO, "real")
        .env(DIR, dir.path())
        .env("TERM", "xterm-256color")
        .env_remove("NO_COLOR")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::null())
        .stderr(Stdio::from(slave))
        .spawn()
        .unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let reader = {
        let seen = seen.clone();
        let master = rustix::io::dup(&master).unwrap();
        std::thread::spawn(move || {
            let mut chunk = [0_u8; 4096];
            while let Ok(read) = rustix::io::read(&master, &mut chunk) {
                if read == 0 {
                    break;
                }
                seen.lock().unwrap().extend_from_slice(&chunk[..read]);
            }
        })
    };
    wait_until("a trial to run", || {
        events_of(dir.path())
            .iter()
            .any(|event| event.kind() == EventKind::TrialStart)
    });
    wait_until("the dashboard to draw", || {
        contains(&seen.lock().unwrap(), HIDE_CURSOR)
    });

    kill_process(Pid::from_child(&child), Signal::INT).unwrap();
    let status = wait_for_exit(&mut child);
    assert_eq!(status.code(), Some(0));
    drop(child);
    reader.join().unwrap();

    let seen = seen.lock().unwrap();
    let hidden = seen
        .windows(HIDE_CURSOR.len())
        .rposition(|window| window == HIDE_CURSOR)
        .unwrap();
    assert!(
        contains(&seen[hidden..], SHOW_CURSOR),
        "the cursor is shown again after the last time it was hidden"
    );
    assert!(matches!(
        &events_of(dir.path()).last().unwrap().payload,
        Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::Signal
    ));
    let restored = tcgetattr(&master).unwrap();
    assert!(
        restored
            .local_modes
            .contains(LocalModes::ECHO | LocalModes::ICANON)
    );
}

const PALETTE_SET: &[u8] = b"\x1b]P0000000";
const PALETTE_RESET: &[u8] = b"\x1b]R";

fn last(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

/// The Go `TestRunHardwareRestoresCursorAndPaletteWhenSessionPanics`: the real dashboard on the Linux console
/// hides the cursor and sets the palette, then the session panics and both are restored before the panic shows.
#[test]
fn a_session_panic_over_the_real_linux_console_dashboard_restores_the_cursor_and_the_palette() {
    let dir = TempDir::new().unwrap();
    let (master, slave) = open_pty();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["pty::helper", "--exact", "--nocapture", "--test-threads=1"])
        .env(SCENARIO, "real-panic")
        .env(DIR, dir.path())
        .env("TERM", "linux")
        .env_remove("NO_COLOR")
        .env_remove("JOURNAL_STREAM")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::null())
        .stderr(Stdio::from(slave))
        .spawn()
        .unwrap();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let reader = {
        let seen = seen.clone();
        let master = rustix::io::dup(&master).unwrap();
        std::thread::spawn(move || {
            let mut chunk = [0_u8; 4096];
            while let Ok(read) = rustix::io::read(&master, &mut chunk) {
                if read == 0 {
                    break;
                }
                seen.lock().unwrap().extend_from_slice(&chunk[..read]);
            }
        })
    };
    wait_until("the dashboard to hide the cursor and set the palette", || {
        let seen = seen.lock().unwrap();
        contains(&seen, HIDE_CURSOR) && contains(&seen, PALETTE_SET)
    });
    std::fs::write(dir.path().join(GO), b"").unwrap();
    let status = wait_for_exit(&mut child);
    assert_eq!(status.code(), Some(2));
    drop(child);
    reader.join().unwrap();

    let seen = seen.lock().unwrap();
    let text = String::from_utf8_lossy(&seen);
    assert!(
        last(&seen, SHOW_CURSOR) > last(&seen, HIDE_CURSOR),
        "the cursor is shown again after the last time it was hidden: {text:?}"
    );
    assert!(
        last(&seen, PALETTE_RESET) > last(&seen, b"\x1b]P"),
        "the palette is reset after the last time it was set: {text:?}"
    );
    assert!(
        text.contains("panic: injected panic while the dashboard shows"),
        "{text:?}"
    );
    let restored = tcgetattr(&master).unwrap();
    assert!(
        restored
            .local_modes
            .contains(LocalModes::ECHO | LocalModes::ICANON),
        "the previous input settings are back"
    );
}
