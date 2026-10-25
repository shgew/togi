use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;
use std::time::Duration;

use togi_command::Output;

use super::{DEADLINE, StopError, invocation, outcome};

fn output(code: i32, stderr: &str) -> Output {
    Output {
        status: ExitStatus::from_raw(code << 8),
        stdout: Vec::new(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

#[test]
fn stops_the_tuning_service_and_waits_up_to_five_minutes() {
    assert_eq!(
        invocation(),
        togi_command::Invocation::new("systemctl")
            .arg("stop")
            .arg("togi.service")
    );
    assert_eq!(DEADLINE, Duration::from_secs(300));
}

#[test]
fn a_stopped_service_is_success() {
    assert!(outcome(Ok(output(0, ""))).is_ok());
}

#[test]
fn a_refused_stop_keeps_the_exit_status_and_stderr() {
    let error = outcome(Ok(output(5, " Unit togi.service not loaded. \n"))).unwrap_err();
    assert!(
        matches!(&error, StopError::Exit { stderr, .. } if stderr == "Unit togi.service not loaded.")
    );
    assert_eq!(
        error.to_string(),
        "systemctl stop exited with exit status: 5: Unit togi.service not loaded."
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
        StopError::Run(togi_command::Error::TimedOut { .. })
    ));
}
