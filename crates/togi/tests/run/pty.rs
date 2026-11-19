//! Run at a pseudo-terminal: signals still stop the session, and run writes event lines without changing terminal state.

use std::ffi::OsString;
use std::fs::File;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process};
use rustix::termios::{LocalModes, tcgetattr};
use tempfile::TempDir;
use togi_journal::{EventKind, Payload, ShutdownReason};
use togi_machine::{
    CancellationToken, Error as MachineError, FrequencyJournal, Machine, Partial, Reporter,
    Running, Started, Stopped, Swept, TargetOffset, TrialResult, TrialSpec, Trials,
};
use togi_render::open_pty;

use crate::support::{CORES, Fake, simulator, written};

const SCENARIO: &str = "TOGI_RUN_TEST_PTY";
const DIR: &str = "TOGI_RUN_TEST_PTY_DIR";
// Match the real_process_ slow-timeout in both nextest profiles: five 60-second periods.
// Host load can delay the helper; the runner, not a shorter local bound, owns the test limit.
const BOUND: Duration = Duration::from_mins(5);
const RUN: &str = "run";
const PANIC: &str = "panic";

/// Trials that wait for cancellation at the first nonzero mapping trial.
struct Endless(Box<dyn Trials>, bool);

struct EndlessRunning(Box<dyn Running>, bool);

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

    fn recover_frequency(
        &mut self,
        stopped: &Stopped,
        journal: &mut dyn FrequencyJournal,
    ) -> Result<String, MachineError> {
        self.0.recover_frequency(stopped, journal)
    }

    fn start(
        &mut self,
        cancel: &CancellationToken,
        spec: TrialSpec,
        ready: Stopped,
        journal: &mut dyn FrequencyJournal,
    ) -> Result<Box<dyn Running>, Partial<Option<Stopped>>> {
        let endless = written(&spec);
        let running = self.0.start(cancel, spec, ready, journal)?;
        if !endless {
            return Ok(running);
        }
        Ok(Box::new(EndlessRunning(running, self.1)))
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
        assert!(!self.1, "injected panic while a trial runs");
        cancel.wait();
        self.0.wait(cancel, report)
    }

    fn stop(self: Box<Self>, journal: &mut dyn FrequencyJournal) -> Result<Stopped, MachineError> {
        self.0.stop(journal)
    }
}

#[test]
fn real_process_helper() {
    if std::env::var_os(SCENARIO).is_none() {
        return;
    }
    let dir = PathBuf::from(std::env::var_os(DIR).unwrap());
    let mut fake = Fake::in_dir(simulator(1000), &dir);
    fake.attest_stock();
    fake.real_stderr = true;
    let scenario = std::env::var(SCENARIO).unwrap();
    let panic = scenario == PANIC;
    fake.wrap = Box::new(move |machine, _| Machine {
        trials: Box::new(Endless(machine.trials, panic)),
        ..machine
    });
    let args: Vec<OsString> = fake
        .arguments(&["run"])
        .into_iter()
        .map(Into::into)
        .collect();
    let exit = togi_cli::run_process(args, &fake);
    if !panic {
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

/// Everything the child writes to the terminal, read while it runs: macOS discards what is still queued once the
/// last slave descriptor closes. The reader ends at that close.
fn record(master: &OwnedFd) -> (Arc<Mutex<Vec<u8>>>, JoinHandle<()>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let master = rustix::io::dup(master).unwrap();
    let reader = {
        let seen = seen.clone();
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
    (seen, reader)
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

fn start(dir: &Path, scenario: &str, slave: File) -> std::process::Child {
    #[expect(
        clippy::disallowed_methods,
        reason = "real_process tests control the run helper over a PTY"
    )]
    Command::new(std::env::current_exe().unwrap())
        .args([
            "pty::real_process_helper",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(SCENARIO, scenario)
        .env(DIR, dir)
        .env("TERM", "linux")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::null())
        .stderr(Stdio::from(slave))
        .spawn()
        .unwrap()
}

#[test_case::test_case(Signal::INT; "sigint")]
#[test_case::test_case(Signal::HUP; "sighup")]
#[test_case::test_case(Signal::TERM; "sigterm")]
fn real_process_signals_stop_a_run_at_a_terminal_with_a_live_event_log(signal: Signal) {
    let dir = TempDir::new().unwrap();
    let (master, slave) = open_pty().unwrap();
    let before = tcgetattr(&master).unwrap().local_modes;
    assert!(before.contains(LocalModes::ISIG));
    let mut child = start(dir.path(), RUN, slave);
    let (seen, reader) = record(&master);

    wait_until("a trial at a written offset to run", || {
        let events = events_of(dir.path());
        events
            .iter()
            .skip_while(|event| event.kind() != EventKind::SmuIntent)
            .any(|event| event.kind() == EventKind::TrialStart)
    });
    assert_eq!(tcgetattr(&master).unwrap().local_modes, before);
    wait_until("a live event line", || {
        String::from_utf8_lossy(&seen.lock().unwrap()).contains(" trial.start ")
    });

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
    drop(child);
    reader.join().unwrap();
    assert_eq!(tcgetattr(&master).unwrap().local_modes, before);
    let seen = seen.lock().unwrap();
    let text = String::from_utf8_lossy(&seen);
    assert_no_screen(&text);
    let applied = events
        .iter()
        .filter(|event| event.kind() == EventKind::ProfileApplied)
        .count();
    assert!(applied > 0, "no profile application in the journal");
    assert_eq!(text.matches(" profile.applied ").count(), applied, "{text}");
    assert_eq!(text.matches(" profile.restored ").count(), 1, "{text}");
    assert_eq!(text.matches(" shutdown ").count(), 1, "{text}");
    assert!(
        text.find(" profile.restored ") < text.find(" shutdown "),
        "{text}"
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
            panic!("run did not exit");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn real_process_a_panic_at_a_terminal_logs_restoration_before_exiting_two() {
    let dir = TempDir::new().unwrap();
    let (master, slave) = open_pty().unwrap();
    let before = tcgetattr(&master).unwrap().local_modes;
    let mut child = start(dir.path(), PANIC, slave);
    let (seen, reader) = record(&master);
    let status = wait_for_exit(&mut child);
    assert_eq!(status.code(), Some(2));
    assert!(events_of(dir.path()).iter().any(|event| matches!(
        &event.payload,
        Payload::ProfileRestored(restored) if restored.offsets.iter().all(|(_, offset)| offset == TargetOffset::ZERO)
    )));
    drop(child);
    reader.join().unwrap();
    assert_eq!(tcgetattr(&master).unwrap().local_modes, before);
    let seen = seen.lock().unwrap();
    let text = String::from_utf8_lossy(&seen);
    assert_no_screen(&text);
    let restored = text.find(" profile.restored ").expect("restoration line");
    let panic = text
        .find("panic: injected panic while a trial runs")
        .expect("panic line");
    assert!(restored < panic, "{text}");
}

fn assert_no_screen(text: &str) {
    assert!(!text.contains("\x1b[?25l"), "run hid the cursor: {text}");
    assert!(
        !text.contains("\x1b]P0000000"),
        "run changed the palette: {text}"
    );
}
