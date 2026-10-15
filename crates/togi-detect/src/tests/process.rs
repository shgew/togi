use std::fs;
use std::os::fd::OwnedFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use togi_machine::Kernel as _;

use super::support::{OLD, boot, caused_by, pstore};
use crate::Kernel;
use crate::runner::Process;

const DEADLINE: Duration = Duration::from_millis(500);
const SLACK: Duration = Duration::from_secs(3);

fn program(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("journalctl");
    fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn kernel_running(program: PathBuf, budget: Duration) -> Kernel {
    Kernel::with_parts(&[], Box::new(Process::at(program)), budget, pstore(&[])).unwrap()
}

struct Identity {
    pid: u32,
    started: String,
}

fn started(pid: u32) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, after_name) = stat.rsplit_once(')')?;
    let fields: Vec<&str> = after_name.split_whitespace().collect();
    if fields.first() == Some(&"Z") {
        return None;
    }
    fields.get(19).map(|ticks| (*ticks).to_owned())
}

fn identity(path: &Path) -> Identity {
    let recorded = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("the program never recorded its identity: {error}"));
    let (pid, started) = recorded.trim().split_once(' ').unwrap();
    Identity {
        pid: pid.parse().unwrap(),
        started: started.to_owned(),
    }
}

fn record(pid: &str, file: &Path) -> String {
    format!(
        "set -- $(cat /proc/{pid}/stat)\necho \"{pid} ${{22}}\" > '{}'\n",
        file.display()
    )
}

fn assert_gone(process: &Identity) {
    for _ in 0..100 {
        if started(process.pid).is_none_or(|ticks| ticks != process.started) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("process {} survived the deadline", process.pid);
}

/// A helper the test started, killed when dropped so a failed assertion leaks no `sleep 3600`.
///
/// The pidfd is opened by id and then kept only if the process behind it still has the recorded
/// start time; from then on the pidfd names that process for good, so the kill never reaches an
/// id the kernel reused.
struct Reaped(Option<OwnedFd>);

impl Reaped {
    fn arm(process: &Identity) -> Self {
        let pidfd = i32::try_from(process.pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .and_then(|pid| {
                rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()).ok()
            })
            .filter(|_| started(process.pid).as_deref() == Some(process.started.as_str()));
        Self(pidfd)
    }
}

impl Drop for Reaped {
    fn drop(&mut self) {
        if let Some(pidfd) = &self.0 {
            let _ = rustix::process::pidfd_send_signal(pidfd, rustix::process::Signal::KILL);
        }
    }
}

fn timed_out(error: &togi_machine::Error) -> bool {
    caused_by::<togi_command::Error>(error)
        .is_some_and(|source| matches!(source, togi_command::Error::TimedOut { .. }))
}

#[test]
fn real_process_a_hung_journalctl_is_killed_at_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let body = format!("{}exec sleep 3600\n", record("$$", &pid_file));
    let mut kernel = kernel_running(program(dir.path(), &body), DEADLINE);
    let started_at = Instant::now();
    let error = kernel.mces(&boot(OLD), Duration::ZERO).unwrap_err();
    let elapsed = started_at.elapsed();
    let helper = identity(&pid_file);
    let _reaped = Reaped::arm(&helper);
    assert!(timed_out(&error), "{error:?}");
    assert!(elapsed < DEADLINE + SLACK, "{elapsed:?}");
    assert_gone(&helper);
}

#[test]
fn real_process_a_descendant_holding_the_pipes_cannot_extend_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let body = format!("sleep 3600 &\n{}exit 0\n", record("$!", &pid_file));
    let mut kernel = kernel_running(program(dir.path(), &body), DEADLINE);
    let started_at = Instant::now();
    let error = kernel.mces(&boot(OLD), Duration::ZERO).unwrap_err();
    let elapsed = started_at.elapsed();
    let helper = identity(&pid_file);
    let _reaped = Reaped::arm(&helper);
    assert!(timed_out(&error), "{error:?}");
    assert!(
        elapsed + Duration::from_millis(100) >= DEADLINE,
        "{elapsed:?}"
    );
    assert!(elapsed < DEADLINE + SLACK, "{elapsed:?}");
    assert_gone(&helper);
}

#[test]
fn real_process_a_hung_command_of_a_later_step_is_bounded_by_the_same_budget() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let body = format!(
        "if [ \"$1\" = --list-boots ]; then\necho '[{{\"boot_id\":\"{OLD}\"}},{{\"boot_id\":\"{}\"}}]'\nexit 0\nfi\n{}exec sleep 3600\n",
        super::support::LATER,
        record("$$", &pid_file)
    );
    let mut kernel = kernel_running(program(dir.path(), &body), DEADLINE);
    let started_at = Instant::now();
    let error = kernel.reset_reason_after(&boot(OLD)).unwrap_err();
    let elapsed = started_at.elapsed();
    let helper = identity(&pid_file);
    let _reaped = Reaped::arm(&helper);
    assert!(timed_out(&error), "{error:?}");
    assert!(elapsed < DEADLINE + SLACK, "{elapsed:?}");
    assert_gone(&helper);
}

#[test]
fn real_process_a_program_that_answers_is_run_by_path_with_the_exact_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let args_file = dir.path().join("args");
    let body = format!(
        "printf '%s\\n' \"$@\" > '{}'\nprintf '%s\\n' '{{\"MESSAGE\":\"[Hardware Error]: CPU:2 (1a:44:0) MC0_STATUS[Over|CE]: 0xdc204000000d0175\",\"__REALTIME_TIMESTAMP\":\"5\",\"__MONOTONIC_TIMESTAMP\":\"6\"}}'\n",
        args_file.display()
    );
    let mut kernel = kernel_running(program(dir.path(), &body), Duration::from_secs(10));
    let mces = kernel.mces(&boot(OLD), Duration::ZERO).unwrap();
    assert_eq!(mces.len(), 1);
    let args = fs::read_to_string(args_file).unwrap();
    assert_eq!(args.lines().nth(2), Some(OLD));
    assert_eq!(args.lines().count(), 10);
}

#[test]
fn real_process_a_missing_program_is_a_spawn_error() {
    let mut kernel = kernel_running(PathBuf::from("/nonexistent/journalctl"), DEADLINE);
    let error = kernel.mces(&boot(OLD), Duration::ZERO).unwrap_err();
    assert!(
        caused_by::<togi_command::Error>(&error)
            .is_some_and(|source| matches!(source, togi_command::Error::Spawn { .. })),
        "{error:?}"
    );
}
