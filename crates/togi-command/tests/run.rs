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

fn sh(script: &str) -> Invocation {
    Invocation::new("sh").arg("-c").arg(script)
}

async fn run_for(invocation: &Invocation, timeout: Duration) -> Result<Output, Error> {
    #[expect(
        clippy::disallowed_methods,
        reason = "tests exercise the bounded runner with real processes"
    )]
    togi_command::run(invocation, Instant::now() + timeout).await
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

async fn assert_stops(pid: Pid) {
    for _ in 0..50 {
        if !is_running(pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("process {pid:?} is still running");
}

async fn wait_for_pid_file(path: &Path) -> Pid {
    for _ in 0..100 {
        if fs::read_to_string(path).is_ok_and(|text| text.ends_with('\n')) {
            return pid_in(path);
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("{} was not written", path.display());
}

#[tokio::test]
async fn captures_stdout_and_stderr_exactly() {
    let output = run_for(&sh("printf out; printf err >&2"), Duration::from_secs(10))
        .await
        .unwrap();
    assert!(output.status.success());
    assert_eq!(output.stdout, b"out");
    assert_eq!(output.stderr, b"err");
}

#[tokio::test]
async fn non_zero_exit_is_output() {
    let output = run_for(&sh("printf partial; exit 3"), Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(output.stdout, b"partial");
}

#[tokio::test]
async fn stdin_is_closed() {
    let output = run_for(&sh("cat; printf done"), Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(output.stdout, b"done");
}

#[tokio::test]
async fn arguments_reach_the_program_without_a_shell() {
    let invocation = Invocation::new("printf")
        .arg("%s|")
        .args(["a b", "$HOME", "*"]);
    let output = run_for(&invocation, Duration::from_secs(10)).await.unwrap();
    assert_eq!(output.stdout, b"a b|$HOME|*|");
}

#[tokio::test]
async fn hung_child_is_killed_at_the_deadline() {
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
async fn a_passed_deadline_still_times_out() {
    let error = run_for(&sh("sleep 60"), Duration::ZERO).await.unwrap_err();
    assert!(matches!(error, Error::TimedOut { .. }), "{error:?}");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn group_descendant_holding_the_pipes_is_killed() {
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
async fn escaped_descendant_cannot_extend_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let script = format!("setsid sleep 60 & echo $! > {}", pid_file.display());
    let started = StdInstant::now();
    let error = run_for(&sh(&script), DEADLINE).await.unwrap_err();
    let elapsed = started.elapsed();
    let escaped = wait_for_pid_file(&pid_file).await;
    let _ = rustix::process::kill_process(escaped, rustix::process::Signal::KILL);
    assert!(elapsed < DEADLINE + SLACK, "{elapsed:?}");
    assert!(matches!(error, Error::TimedOut { .. }), "{error:?}");
    assert_stops(escaped).await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn exited_leader_stays_unreaped_until_the_group_is_killed() {
    let dir = tempfile::tempdir().unwrap();
    let leader_file = dir.path().join("leader");
    let escaped_file = dir.path().join("escaped");
    let script = format!(
        "echo $$ > {}; setsid sleep 60 & echo $! > {}",
        leader_file.display(),
        escaped_file.display()
    );
    let invocation = sh(&script);
    let observe = async {
        let leader = wait_for_pid_file(&leader_file).await;
        let escaped = wait_for_pid_file(&escaped_file).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(is_zombie(leader), "the exited leader was reaped early");
        assert!(is_running(escaped));
        (leader, escaped)
    };
    let (result, (leader, escaped)) = tokio::join!(run_for(&invocation, DEADLINE), observe);
    let still_running = is_running(escaped);
    let _ = rustix::process::kill_process(escaped, rustix::process::Signal::KILL);
    assert!(matches!(result, Err(Error::TimedOut { .. })), "{result:?}");
    assert!(
        still_running,
        "the group kill reached a process outside the group"
    );
    assert_eq!(test_kill_process(leader), Err(Errno::SRCH));
    assert_stops(escaped).await;
}

#[tokio::test]
async fn output_over_the_limit_on_stdout_kills_the_group() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let script = format!("echo $$ > {}; exec yes", pid_file.display());
    let started = StdInstant::now();
    let error = run_for(&sh(&script).output_limit(1000), Duration::from_secs(10))
        .await
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(5));
    match error {
        Error::OutputTooLarge {
            program,
            stream,
            limit,
        } => {
            assert_eq!(program, "sh");
            assert_eq!(stream, Stream::Stdout);
            assert_eq!(limit, 1000);
        }
        other => panic!("{other:?}"),
    }
    assert_stops(pid_in(&pid_file)).await;
}

#[tokio::test]
async fn output_over_the_limit_on_stderr_kills_the_group() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let script = format!("echo $$ > {}; exec yes >&2", pid_file.display());
    let error = run_for(&sh(&script).output_limit(1000), Duration::from_secs(10))
        .await
        .unwrap_err();
    assert!(
        matches!(
            error,
            Error::OutputTooLarge {
                stream: Stream::Stderr,
                ..
            }
        ),
        "{error:?}"
    );
    assert_stops(pid_in(&pid_file)).await;
}

#[tokio::test]
async fn output_exactly_at_the_limit_is_kept() {
    let output = run_for(&sh("printf 12345").output_limit(5), Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(output.stdout, b"12345");
}

#[tokio::test]
async fn dropping_the_future_kills_the_group() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let script = format!("sleep 60 & echo $! > {}; wait", pid_file.display());
    let invocation = sh(&script);
    let abandoned = tokio::time::timeout(
        Duration::from_millis(200),
        run_for(&invocation, Duration::from_secs(60)),
    )
    .await;
    assert!(abandoned.is_err());
    assert_stops(wait_for_pid_file(&pid_file).await).await;
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
fn run_blocking_runs_outside_a_runtime() {
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
fn run_blocking_times_out() {
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
async fn missing_program_names_it() {
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
