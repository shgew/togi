//! Ctrl-C at a terminal: `run` shows the dashboard on a pseudo-terminal, the dashboard closes, and SIGINT still
//! reaches `run`'s own listener, which stops the session as on any SIGINT. The child is this test binary run
//! again on one test (`helper`), as in `panics`.

use std::ffi::OsString;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions, kill_process, waitid};
use rustix::termios::{LocalModes, tcgetattr};
use tempfile::TempDir;
use togi_cli::dashboard::Stop;
use togi_journal::{EventKind, Payload, ShutdownReason};
use togi_machine::{
    CancellationToken, Error as MachineError, Machine, Partial, Reporter, Running, Started,
    Stopped, Swept, TrialResult, TrialSpec, Trials,
};
use togi_render::open_pty;

use crate::support::{CORES, Fake, Scripted, simulator, written};

const SCENARIO: &str = "TOGI_RUN_TEST_PTY";
const DIR: &str = "TOGI_RUN_TEST_PTY_DIR";
// Match the real_process_ slow-timeout in both nextest profiles: five 60-second periods.
// Host load can delay the helper; the runner, not a shorter local bound, owns the test limit.
const BOUND: Duration = Duration::from_mins(5);
/// The file in [`DIR`] that lets a `real-panic` trial panic.
const GO: &str = "panic-now";

/// Trials that run until the run is cancelled, or panic as [`Panic`] says, from the first one at an offset the run
/// wrote; the night's stock controls before it run as the machine runs them.
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
    fn controller_pid(&self) -> u32 {
        togi_sim::CONTROLLER_PID
    }

    fn content(
        &mut self,
        workload: togi_machine::WorkloadId,
    ) -> Result<togi_machine::ContentId, MachineError> {
        self.0.content(workload)
    }

    fn sweep(&mut self, cancel: &CancellationToken) -> Result<Swept, MachineError> {
        self.0.sweep(cancel)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        ready: Stopped,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        let endless = written(&spec);
        let running = self.0.start(cancel, spec, ready)?;
        if !endless {
            return Ok(running);
        }
        Ok(Box::new(EndlessRunning(running, self.1.clone())))
    }

    fn ended(&mut self, id: &str) -> Result<(), MachineError> {
        self.0.ended(id)
    }

    fn samples<'a>(
        &'a self,
        id: &'a str,
    ) -> Box<
        dyn Iterator<Item = Result<togi_machine::TrialConditions, togi_machine::SampleError>> + 'a,
    > {
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
                Changes::new(go.parent().unwrap())
                    .wait_until("the test to see the dashboard", || go.exists());
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
fn real_process_helper() {
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
    fake.wrap = Box::new(move |machine, _| Machine {
        trials: Box::new(Endless(machine.trials, panic.clone())),
        ..machine
    });
    let args: Vec<OsString> = fake
        .arguments(&["run"])
        .into_iter()
        .map(Into::into)
        .collect();
    let exit = togi_cli::run_process(args, &fake);
    if scenario == "run" {
        let mut smu = fake.sim.seams().smu;
        for core in 0..CORES {
            assert_eq!(
                smu.offset(togi_machine::CoreId::new(core).unwrap())
                    .unwrap()
                    .0,
                0,
                "core {core}",
            );
        }
    }
    std::process::exit(i32::from(exit.code()));
}

#[derive(Default)]
struct Output {
    bytes: Mutex<Vec<u8>>,
    changed: Condvar,
}

impl Output {
    fn wait_until(&self, what: &str, mut ready: impl FnMut(&[u8]) -> bool) {
        let (seen, _) = self
            .changed
            .wait_timeout_while(self.bytes.lock().unwrap(), BOUND, |seen| !ready(seen))
            .unwrap();
        assert!(ready(&seen), "timed out waiting for {what}");
    }
}

/// Everything the child writes to the terminal, read while it runs: macOS discards what is still queued once the
/// last slave descriptor closes. The reader ends at that close.
fn record(master: &OwnedFd) -> (Arc<Output>, JoinHandle<()>) {
    let seen = Arc::new(Output::default());
    let master = rustix::io::dup(master).unwrap();
    let reader = {
        let seen = seen.clone();
        std::thread::spawn(move || {
            let mut chunk = [0_u8; 4096];
            while let Ok(read) = rustix::io::read(&master, &mut chunk) {
                if read == 0 {
                    break;
                }
                seen.bytes.lock().unwrap().extend_from_slice(&chunk[..read]);
                seen.changed.notify_all();
            }
        })
    };
    (seen, reader)
}

fn events_of(dir: &Path) -> Vec<togi_journal::Event> {
    togi_journal::Recording::read(&dir.join("state/events.jsonl"))
        .map(togi_journal::Recording::into_events)
        .unwrap_or_default()
}

/// The journal and panic-release file announce changes through inotify on Linux. Other supported
/// platforms retain their polling fallback: rustix's kqueue registration requires unsafe code.
struct Changes {
    #[cfg(target_os = "linux")]
    fd: OwnedFd,
    #[cfg(not(target_os = "linux"))]
    interval: Duration,
}

impl Changes {
    fn new(directory: &Path) -> Self {
        std::fs::create_dir_all(directory).unwrap();
        #[cfg(target_os = "linux")]
        {
            use rustix::fs::inotify::{self, CreateFlags, WatchFlags};

            let fd = inotify::init(CreateFlags::CLOEXEC).unwrap();
            inotify::add_watch(
                &fd,
                directory,
                WatchFlags::MODIFY | WatchFlags::CREATE | WatchFlags::MOVED_TO,
            )
            .unwrap();
            Self { fd }
        }
        #[cfg(not(target_os = "linux"))]
        Self {
            interval: Duration::from_millis(20),
        }
    }

    fn wait_until(&self, what: &str, mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + BOUND;
        while !ready() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            #[cfg(target_os = "linux")]
            {
                let timeout =
                    Timespec::try_from(deadline.saturating_duration_since(Instant::now())).unwrap();
                let mut fds = [PollFd::new(&self.fd, PollFlags::IN)];
                assert!(
                    poll(&mut fds, Some(&timeout)).unwrap() > 0,
                    "timed out waiting for {what}"
                );
                let mut changes = [0_u8; 4096];
                rustix::io::read(&self.fd, &mut changes).unwrap();
            }
            #[cfg(not(target_os = "linux"))]
            std::thread::sleep(self.interval);
        }
    }
}

#[test]
fn real_process_ctrl_c_reaches_runs_own_listener_after_the_dashboard_closed_and_the_session_stops()
{
    stopped_by_signal(Signal::INT);
}

pub(super) fn stopped_by_signal(signal: Signal) {
    let dir = TempDir::new().unwrap();
    let changes = Changes::new(&dir.path().join("state"));
    let (master, slave) = open_pty().unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "real_process tests control the run helper over a PTY"
    )]
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "pty::real_process_helper",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(SCENARIO, "run")
        .env(DIR, dir.path())
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::null())
        .stderr(Stdio::from(slave))
        .spawn()
        .unwrap();

    changes.wait_until("a trial at a written offset to run", || {
        let events = events_of(dir.path());
        events
            .iter()
            .skip_while(|event| event.kind() != EventKind::SmuIntent)
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

    kill_process(Pid::from_child(&child), signal).unwrap();
    let status = wait_for_exit(&mut child);
    assert_eq!(status.code(), Some(0));

    let events = events_of(dir.path());
    assert!(matches!(
        &events.last().unwrap().payload,
        Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::Signal
    ));
    assert!(
        events
            .iter()
            .any(|event| event.kind() == EventKind::SmuIntent)
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind() == EventKind::ProfileRestored)
    );
    let restored = tcgetattr(&master).unwrap();
    assert!(
        restored.local_modes.contains(LocalModes::ECHO)
            && restored.local_modes.contains(LocalModes::ICANON),
        "the previous settings are back once run stops"
    );
}

fn wait_for_exit(child: &mut std::process::Child) -> std::process::ExitStatus {
    let pid = Pid::from_child(child);
    std::thread::scope(|scope| {
        let (exited, exit) = mpsc::channel();
        scope.spawn(move || {
            // Leave the child waitable so only Child::wait reaps it, including after a timeout.
            let status = loop {
                match waitid(
                    WaitId::Pid(pid),
                    WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
                ) {
                    Err(rustix::io::Errno::INTR) => {}
                    result => break result.map(|status| status.is_some()),
                }
            };
            let _ = exited.send(status);
        });
        if let Ok(status) = exit.recv_timeout(BOUND) {
            assert!(status.unwrap(), "waitid returned without an exit");
        } else {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("run did not stop within {BOUND:?}");
        }
    });
    child.wait().unwrap()
}

#[test]
fn real_process_a_session_panic_stops_the_dashboard_and_gives_the_terminal_back_before_the_panic_propagates()
 {
    let dir = TempDir::new().unwrap();
    let (master, slave) = open_pty().unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "the real_process test checks panic terminal restoration"
    )]
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "pty::real_process_helper",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(SCENARIO, "panic")
        .env(DIR, dir.path())
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::null())
        .stderr(Stdio::from(slave))
        .spawn()
        .unwrap();
    let (seen, reader) = record(&master);
    let status = wait_for_exit(&mut child);
    assert_eq!(status.code(), Some(2));
    drop(child);
    reader.join().unwrap();
    let restored = tcgetattr(&master).unwrap();
    assert!(
        restored.local_modes.contains(LocalModes::ECHO)
            && restored.local_modes.contains(LocalModes::ICANON),
        "the previous input settings are back"
    );
    let seen = String::from_utf8_lossy(&seen.bytes.lock().unwrap()).into_owned();
    assert!(
        seen.contains("panic: injected panic while the dashboard shows"),
        "{seen}"
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
fn real_process_ctrl_c_stops_a_run_that_shows_the_real_dashboard_and_the_cursor_comes_back() {
    let dir = TempDir::new().unwrap();
    let changes = Changes::new(&dir.path().join("state"));
    let (master, slave) = open_pty().unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "the real_process test sends Ctrl-C through a PTY"
    )]
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "pty::real_process_helper",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(SCENARIO, "real")
        .env(DIR, dir.path())
        .env("TERM", "xterm-256color")
        .env_remove("NO_COLOR")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::null())
        .stderr(Stdio::from(slave))
        .spawn()
        .unwrap();
    let (seen, reader) = record(&master);
    changes.wait_until("a trial to run", || {
        events_of(dir.path())
            .iter()
            .any(|event| event.kind() == EventKind::TrialStart)
    });
    seen.wait_until("the dashboard to draw", |seen| contains(seen, HIDE_CURSOR));

    kill_process(Pid::from_child(&child), Signal::INT).unwrap();
    let status = wait_for_exit(&mut child);
    assert_eq!(status.code(), Some(0));
    drop(child);
    reader.join().unwrap();

    let seen = seen.bytes.lock().unwrap();
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

/// The real dashboard on the Linux console hides the cursor and sets the palette, then the session
/// panics and both are restored before the panic shows.
#[test]
fn real_process_a_session_panic_over_the_real_linux_console_dashboard_restores_the_cursor_and_the_palette()
 {
    let dir = TempDir::new().unwrap();
    let (master, slave) = open_pty().unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "the real_process test checks console restoration after panic"
    )]
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "pty::real_process_helper",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
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
    let (seen, reader) = record(&master);
    seen.wait_until(
        "the dashboard to hide the cursor and set the palette",
        |seen| contains(seen, HIDE_CURSOR) && contains(seen, PALETTE_SET),
    );
    std::fs::write(dir.path().join(GO), b"").unwrap();
    let status = wait_for_exit(&mut child);
    assert_eq!(status.code(), Some(2));
    drop(child);
    reader.join().unwrap();

    let seen = seen.bytes.lock().unwrap();
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
