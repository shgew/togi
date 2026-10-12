use std::process::ExitStatus;
use std::time::Duration;

use togi_command::{Invocation, Output};

const DEADLINE: Duration = Duration::from_secs(30);

#[derive(Debug, thiserror::Error)]
pub enum RebootError {
    #[error("run systemctl reboot")]
    Run(#[source] togi_command::Error),
    #[error("systemctl reboot exited with {status}{}", detail(stderr))]
    Exit { status: ExitStatus, stderr: String },
}

fn detail(stderr: &str) -> String {
    if stderr.is_empty() {
        String::new()
    } else {
        format!(": {stderr}")
    }
}

fn invocation() -> Invocation {
    Invocation::new("systemctl").arg("reboot")
}

fn outcome(result: Result<Output, togi_command::Error>) -> Result<(), RebootError> {
    let output = result.map_err(RebootError::Run)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(RebootError::Exit {
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

/// Asks systemd for a plain reboot, waiting at most 30 seconds for the request to be accepted.
#[expect(
    clippy::disallowed_methods,
    reason = "systemctl reboot is the dead-end reboot the binary requests"
)]
pub fn reboot() -> Result<(), RebootError> {
    outcome(togi_command::run_blocking(&invocation(), DEADLINE))
}

#[cfg(test)]
mod tests;
