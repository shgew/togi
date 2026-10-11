#![cfg(unix)]

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant as StdInstant};

use rustix::io::Errno;
use rustix::process::{Pid, test_kill_process};
use togi_command::{Error, Invocation, Output, Stream};
use tokio::time::Instant;

const DEADLINE: Duration = Duration::from_millis(300);
const SLACK: Duration = Duration::from_millis(900);
/// Long enough for the escape helper to start and leave the group before the deadline.
#[cfg(target_os = "linux")]
const ESCAPE_DEADLINE: Duration = Duration::from_secs(2);
const ESCAPE_HELPER: &str = "TOGI_COMMAND_ESCAPE_HELPER";

fn sh(script: &str) -> Invocation {
    Invocation::new("sh").arg("-c").arg(script)
}

/// `sh -c script` with this test binary as `$0`, for `escape_command`.
#[cfg(target_os = "linux")]
fn sh_with_helper(script: &str) -> Invocation {
    sh(script)
        .arg(std::env::current_exe().unwrap_or_else(|error| panic!("no test binary path: {error}")))
}

/// Shell text that runs this test binary as the escape helper, which writes `ready` once it has
/// left the runner's session and process group. `setsid` is not in the Nix check sandbox.
#[cfg(target_os = "linux")]
fn escape_command(ready: &Path) -> String {
    format!(
        "{ESCAPE_HELPER}={} \"$0\" --exact escape_helper --test-threads 1",
        ready.display()
    )
}

/// Does nothing unless re-invoked by `escape_command`: then it starts its own session, as
/// `setsid` would, and holds the inherited pipes open until killed.
#[test]
fn escape_helper() {
    let Some(ready) = std::env::var_os(ESCAPE_HELPER) else {
        return;
    };
    rustix::process::setsid().unwrap();
    fs::write(ready, "").unwrap();
    std::thread::sleep(Duration::from_secs(60));
}

async fn run_for(invocation: &Invocation, timeout: Duration) -> Result<Output, Error> {
    #[expect(
        clippy::disallowed_methods,
        reason = "tests exercise the bounded runner with real processes"
    )]
    togi_command::run(invocation, Instant::now() + timeout).await
}

/// A process the test started outside the runner's reach (a reparented descendant or a helper in
/// its own session), killed when dropped so a failed assertion leaks nothing.
///
/// On Linux it holds a pidfd opened while the process is known to run, which names that process
/// for good: liveness is the pidfd's readability and the kill goes through `pidfd_send_signal`,
/// so an id that init reaped and the kernel reused is never signalled. Elsewhere no stable
/// identity exists for a process that is not our child, so nothing is ever signalled by id; the
/// helpers there sleep for a finite time instead.
struct Watched {
    #[cfg(target_os = "linux")]
    pidfd: std::os::fd::OwnedFd,
    #[cfg(not(target_os = "linux"))]
    pid: Pid,
}

impl Watched {
    /// `None` unless the process is running now.
    #[cfg(target_os = "linux")]
    fn open(pid: Pid) -> Option<Self> {
        let pidfd = rustix::process::pidfd_open(pid, rustix::process::PidfdFlags::empty()).ok()?;
        let watched = Self { pidfd };
        watched.is_running().then_some(watched)
    }

    #[cfg(not(target_os = "linux"))]
    fn open(pid: Pid) -> Option<Self> {
        is_running(pid).then_some(Self { pid })
    }

    /// A pidfd is readable once its process has terminated.
    #[cfg(target_os = "linux")]
    fn is_running(&self) -> bool {
        let mut fds = [rustix::event::PollFd::new(
            &self.pidfd,
            rustix::event::PollFlags::IN,
        )];
        let now = rustix::event::Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        matches!(rustix::event::poll(&mut fds, Some(&now)), Ok(0))
    }

    #[cfg(not(target_os = "linux"))]
    fn is_running(&self) -> bool {
        is_running(self.pid)
    }

    async fn stops(&self) -> bool {
        for _ in 0..50 {
            if !self.is_running() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }
}

#[cfg(target_os = "linux")]
impl Drop for Watched {
    fn drop(&mut self) {
        let _ = rustix::process::pidfd_send_signal(&self.pidfd, rustix::process::Signal::KILL);
    }
}

fn pid_in(path: &Path) -> Pid {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| raw.trim().parse().ok())
        .and_then(Pid::from_raw)
        .unwrap_or_else(|| panic!("{} holds no pid", path.display()))
}

fn is_zombie(pid: Pid) -> bool {
    fs::read_to_string(format!("/proc/{}/stat", pid.as_raw_nonzero()))
        .ok()
        .and_then(|stat| {
            stat.rsplit_once(')')
                .and_then(|(_, rest)| rest.trim_start().chars().next())
        })
        .is_some_and(|state| state == 'Z')
}

fn is_running(pid: Pid) -> bool {
    match test_kill_process(pid) {
        Ok(()) => !is_zombie(pid),
        Err(errno) => errno != Errno::SRCH,
    }
}

async fn stops(pid: Pid) -> bool {
    for _ in 0..50 {
        if !is_running(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

async fn assert_stops(pid: Pid) {
    assert!(stops(pid).await, "process {pid:?} is still running");
}

#[cfg(target_os = "linux")]
async fn wait_for_file(path: &Path) {
    for _ in 0..250 {
        if path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{} was not written", path.display());
}

async fn wait_for_pid_file(path: &Path) -> Pid {
    for _ in 0..250 {
        if fs::read_to_string(path).is_ok_and(|text| text.ends_with('\n')) {
            return pid_in(path);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{} was not written", path.display());
}

#[tokio::test]
async fn real_process_captures_stdout_and_stderr_exactly() {
    let output = run_for(&sh("printf out; printf err >&2"), Duration::from_secs(10))
        .await
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"out");
    assert_eq!(output.stderr, b"err");
}

#[tokio::test]
async fn real_process_non_zero_exit_is_output() {
    let output = run_for(&sh("printf partial; exit 3"), Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(output.stdout, b"partial");
}

#[tokio::test]
async fn real_process_stdin_is_closed() {
    let output = run_for(&sh("cat; printf done"), Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(output.stdout, b"done");
}

#[tokio::test]
async fn real_process_arguments_reach_the_program_without_a_shell() {
    let invocation = Invocation::new("printf")
        .arg("%s|")
        .args(["a b", "$HOME", "*"]);
    let output = run_for(&invocation, Duration::from_secs(10)).await.unwrap();
    assert_eq!(output.stdout, b"a b|$HOME|*|");
}

#[tokio::test]
async fn real_process_hung_child_is_killed_at_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let script = format!("echo $$ > {}; sleep 60", pid_file.display());
    let started = StdInstant::now();
    let error = run_for(&sh(&script), DEADLINE).await.unwrap_err();
    assert!(
        started.elapsed() < DEADLINE + SLACK,
        "{:?}",
        started.elapsed()
    );
    match error {
        Error::TimedOut { program, timeout } => {
            assert_eq!(program, "sh");
            assert!(timeout <= DEADLINE);
        }
        other => panic!("{other:?}"),
    }
    assert_stops(pid_in(&pid_file)).await;
}

#[tokio::test]
async fn a_deadline_not_in_the_future_times_out_without_spawning() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker");
    let invocation = sh(&format!("touch {}", marker.display()));
    let now = Instant::now();
    for deadline in [now, now.checked_sub(Duration::from_secs(1)).unwrap()] {
        let started = StdInstant::now();
        #[expect(
            clippy::disallowed_methods,
            reason = "tests exercise the bounded runner with real processes"
        )]
        let error = togi_command::run(&invocation, deadline).await.unwrap_err();
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "{:?}",
            started.elapsed()
        );
        match error {
            Error::TimedOut { program, timeout } => {
                assert_eq!(program, "sh");
                assert_eq!(timeout, Duration::ZERO);
            }
            other => panic!("{other:?}"),
        }
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!marker.exists(), "the command ran after its deadline");
}

#[test]
fn a_zero_blocking_timeout_times_out_without_spawning() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker");
    let started = StdInstant::now();
    #[expect(
        clippy::disallowed_methods,
        reason = "tests exercise the bounded runner with real processes"
    )]
    let error =
        togi_command::run_blocking(&sh(&format!("touch {}", marker.display())), Duration::ZERO)
            .unwrap_err();
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "{:?}",
        started.elapsed()
    );
    assert!(matches!(error, Error::TimedOut { .. }), "{error:?}");
    std::thread::sleep(Duration::from_millis(100));
    assert!(!marker.exists(), "the command ran after its deadline");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn real_process_group_descendant_holding_the_pipes_is_killed() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let script = format!("sleep 60 & echo $! > {}; echo started", pid_file.display());
    let started = StdInstant::now();
    let error = run_for(&sh(&script), DEADLINE).await.unwrap_err();
    assert!(
        started.elapsed() < DEADLINE + SLACK,
        "{:?}",
        started.elapsed()
    );
    assert!(matches!(error, Error::TimedOut { .. }), "{error:?}");
    assert_stops(pid_in(&pid_file)).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn real_process_escaped_descendant_cannot_extend_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let ready = dir.path().join("ready");
    let script = format!(
        "{} & echo $! > {}",
        escape_command(&ready),
        pid_file.display()
    );
    let started = StdInstant::now();
    let result = run_for(&sh_with_helper(&script), ESCAPE_DEADLINE).await;
    let elapsed = started.elapsed();
    let escaped = Watched::open(wait_for_pid_file(&pid_file).await);
    assert!(
        ready.exists(),
        "the helper had not left the group by the deadline"
    );
    assert!(
        escaped.is_some(),
        "the escaped helper did not hold the pipes"
    );
    assert!(elapsed < ESCAPE_DEADLINE + SLACK, "{elapsed:?}");
    assert!(matches!(result, Err(Error::TimedOut { .. })), "{result:?}");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn real_process_exited_leader_stays_unreaped_until_the_group_is_killed() {
    let dir = tempfile::tempdir().unwrap();
    let leader_file = dir.path().join("leader");
    let escaped_file = dir.path().join("escaped");
    let ready = dir.path().join("ready");
    let script = format!(
        "echo $$ > {}; {} & echo $! > {}",
        leader_file.display(),
        escape_command(&ready),
        escaped_file.display()
    );
    let invocation = sh_with_helper(&script);
    let mut run = Box::pin(run_for(&invocation, Duration::from_secs(60)));
    let observe = async {
        let leader = wait_for_pid_file(&leader_file).await;
        let escaped = wait_for_pid_file(&escaped_file).await;
        wait_for_file(&ready).await;
        let escaped = Watched::open(escaped);
        let mut zombie = false;
        for _ in 0..250 {
            if is_zombie(leader) {
                zombie = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        (leader, escaped, zombie)
    };
    let (leader, escaped, zombie) = tokio::select! {
        result = &mut run => panic!("the run ended before it was dropped: {result:?}"),
        observed = observe => observed,
    };
    let Some(escaped) = escaped else {
        panic!("the escaped helper was not running");
    };
    assert!(zombie, "the exited leader was reaped before the run ended");
    drop(run);
    assert!(
        escaped.is_running(),
        "the group kill reached a process outside the group"
    );
    assert_eq!(test_kill_process(leader), Err(Errno::SRCH));
}

/// A descendant in the group holds the pipes while the leader overflows `stream`.
async fn overflow_kills_the_group(stream: Stream) {
    let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("no temporary directory: {error}"));
    let pid_file = dir.path().join("pid");
    let go = dir.path().join("go");
    let redirect = match stream {
        Stream::Stdout => "",
        Stream::Stderr => " >&2",
    };
    let script = format!(
        "sleep 60 & echo $! > {}; while [ ! -e {} ]; do sleep 0.01; done; exec yes{redirect}",
        pid_file.display(),
        go.display()
    );
    let invocation = sh(&script).output_limit(1000);
    let started = StdInstant::now();
    let (result, (descendant, watched)) =
        tokio::join!(run_for(&invocation, Duration::from_secs(10)), async {
            let descendant = wait_for_pid_file(&pid_file).await;
            let watched = Watched::open(descendant);
            fs::write(&go, "")
                .unwrap_or_else(|error| panic!("cannot write {}: {error}", go.display()));
            (descendant, watched)
        });
    let alive = watched.is_some();
    let stopped = match &watched {
        Some(watched) => watched.stops().await,
        None => false,
    };
    assert!(alive, "the descendant was not running before the overflow");
    assert!(stopped, "the descendant {descendant:?} is still running");
    assert!(started.elapsed() < Duration::from_secs(5));
    match result {
        Ok(output) => panic!("{output:?}"),
        Err(Error::OutputTooLarge {
            program,
            stream: overflowed,
            limit,
        }) => {
            assert_eq!(program, "sh");
            assert_eq!(overflowed, stream);
            assert_eq!(limit, 1000);
        }
        Err(other) => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn real_process_output_over_the_limit_on_stdout_kills_the_group() {
    overflow_kills_the_group(Stream::Stdout).await;
}

#[tokio::test]
async fn real_process_output_over_the_limit_on_stderr_kills_the_group() {
    overflow_kills_the_group(Stream::Stderr).await;
}

#[tokio::test]
async fn real_process_output_exactly_at_the_limit_is_kept_on_both_streams() {
    let output = run_for(
        &sh("printf 12345; printf abcde >&2").output_limit(5),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert_eq!(output.stdout, b"12345");
    assert_eq!(output.stderr, b"abcde");
}

async fn assert_one_byte_over(script: &str, stream: Stream) {
    let result = run_for(&sh(script).output_limit(5), Duration::from_secs(10)).await;
    assert!(
        matches!(
            result,
            Err(Error::OutputTooLarge { stream: overflowed, limit: 5, .. }) if overflowed == stream
        ),
        "{result:?}"
    );
}

#[tokio::test]
async fn real_process_one_byte_over_the_limit_on_stdout_is_too_large() {
    assert_one_byte_over("printf 123456; printf abcde >&2", Stream::Stdout).await;
}

#[tokio::test]
async fn real_process_one_byte_over_the_limit_on_stderr_is_too_large() {
    assert_one_byte_over("printf 12345; printf abcdef >&2", Stream::Stderr).await;
}

#[tokio::test]
async fn real_process_dropping_the_future_kills_the_group() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let script = format!("sleep 60 & echo $! > {}; wait", pid_file.display());
    let invocation = sh(&script);
    let mut run = Box::pin(run_for(&invocation, Duration::from_secs(60)));
    let pid = tokio::select! {
        result = &mut run => panic!("the run ended before it was dropped: {result:?}"),
        pid = wait_for_pid_file(&pid_file) => pid,
    };
    drop(run);
    assert_stops(pid).await;
}

#[tokio::test]
async fn run_blocking_inside_a_runtime_does_not_spawn() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("marker");
    let invocation = sh(&format!("touch {}", marker.display()));
    #[expect(
        clippy::disallowed_methods,
        reason = "tests exercise the bounded runner with real processes"
    )]
    let error = togi_command::run_blocking(&invocation, Duration::from_secs(10)).unwrap_err();
    assert!(matches!(error, Error::InsideRuntime { .. }), "{error:?}");
    assert!(!marker.exists());
}

#[test]
fn real_process_run_blocking_runs_outside_a_runtime() {
    #[expect(
        clippy::disallowed_methods,
        reason = "tests exercise the bounded runner with real processes"
    )]
    let output =
        togi_command::run_blocking(&sh("printf hi; exit 2"), Duration::from_secs(10)).unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(output.stdout, b"hi");
}

#[test]
fn real_process_run_blocking_times_out() {
    let started = StdInstant::now();
    #[expect(
        clippy::disallowed_methods,
        reason = "tests exercise the bounded runner with real processes"
    )]
    let error = togi_command::run_blocking(&sh("sleep 60"), DEADLINE).unwrap_err();
    assert!(started.elapsed() < DEADLINE + SLACK);
    assert!(matches!(error, Error::TimedOut { .. }), "{error:?}");
}

#[tokio::test]
async fn real_process_missing_program_names_it() {
    let error = run_for(
        &Invocation::new("togi-command-no-such-program"),
        Duration::from_secs(10),
    )
    .await
    .unwrap_err();
    match &error {
        Error::Spawn { program, source } => {
            assert_eq!(program, "togi-command-no-such-program");
            assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
        }
        other => panic!("{other:?}"),
    }
    assert!(error.to_string().contains("togi-command-no-such-program"));
}
