use std::process::ExitStatus;
use std::time::Duration;

use togi_command::{Invocation, Output};

/// The tuning service the night switch stops. It stops on SIGTERM and takes as long as a clean
/// stop of the run takes, which systemd bounds with the unit's own stop timeout.
const UNIT: &str = "togi.service";
const DEADLINE: Duration = Duration::from_secs(300);

#[derive(Debug, thiserror::Error)]
pub enum StopError {
    #[error("run systemctl stop")]
    Run(#[source] togi_command::Error),
    #[error("systemctl stop exited with {status}{}", detail(stderr))]
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
    Invocation::new("systemctl").arg("stop").arg(UNIT)
}

fn outcome(result: Result<Output, togi_command::Error>) -> Result<(), StopError> {
    let output = result.map_err(StopError::Run)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(StopError::Exit {
            status: output.status,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

/// Stops the tuning service and waits, for at most five minutes, until systemd reports it
/// stopped, so the run has ended cleanly before the caller changes the boot.
#[expect(
    clippy::disallowed_methods,
    reason = "systemctl stop ends the tuning service the night switch is leaving"
)]
pub fn stop_tuning() -> Result<(), StopError> {
    outcome(togi_command::run_blocking(&invocation(), DEADLINE))
}

#[cfg(test)]
mod tests;
