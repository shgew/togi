use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;
use std::time::Duration;

use togi_command::Output;

use super::{DEADLINE, RebootError, invocation, outcome};

fn output(code: i32, stderr: &str) -> Output {
    Output {
        status: ExitStatus::from_raw(code << 8),
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

#[test]
fn asks_systemd_for_a_plain_reboot_within_thirty_seconds() {
    assert_eq!(
        invocation(),
        togi_command::Invocation::new("systemctl").arg("reboot")
    );
    assert_eq!(DEADLINE, Duration::from_secs(30));
}

#[test]
fn an_accepted_request_is_success() {
    assert!(outcome(Ok(output(0, ""))).is_ok());
}

#[test]
fn a_refused_request_keeps_the_exit_status_and_stderr() {
    let error = outcome(Ok(output(1, " Failed to reboot \n"))).unwrap_err();
    assert!(matches!(&error, RebootError::Exit { stderr, .. } if stderr == "Failed to reboot"));
    assert_eq!(
        error.to_string(),
        "systemctl reboot exited with exit status: 1: Failed to reboot"
    );
}

#[test]
fn a_command_that_could_not_run_or_timed_out_is_an_error() {
    let error = outcome(Err(togi_command::Error::TimedOut {
        program: "systemctl".into(),
        timeout: DEADLINE,
    }))
    .unwrap_err();
    assert!(matches!(
        error,
        RebootError::Run(togi_command::Error::TimedOut { .. })
    ));
}
