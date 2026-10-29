//! Read-only day prerequisites; no stress backend or offset-writing capability.

use std::path::Path;
use std::time::Duration;

use togi_command::{Invocation, Output};
use togi_config::Config;
use togi_machine::{BootId, Check, WatchdogState};

use crate::diagnose::{Diagnosis, diagnosis_host};
use crate::error::Error;
use crate::host::{HostReadings, Privilege, SystemHost};
use crate::preflight::{Outcome, Step};

#[expect(
    clippy::disallowed_methods,
    reason = "the read-only day diagnostic queries journalctl's persistent storage with bounded output and runtime"
)]
pub(crate) fn at(
    root: &Path,
    config: &Config,
    request_privileged: bool,
    privilege: Privilege,
) -> Result<Diagnosis, Error> {
    let (host, privileged) = diagnosis_host(root, config, request_privileged, privilege)?;
    let persistent = match togi_detect::boot_id() {
        Ok(boot) => persistent_with(&boot, |invocation| {
            togi_command::run_blocking(invocation, Duration::from_secs(10))
        }),
        Err(error) => check(
            "persistent_journal",
            togi_machine::error_chain(&error),
            false,
        ),
    };
    Ok(with_host(
        &host,
        privileged,
        crate::watchdog::arming(root),
        persistent,
    ))
}

pub(crate) fn with_host<R: HostReadings>(
    host: &SystemHost<R>,
    privileged: bool,
    watchdog: WatchdogState,
    persistent: Check,
) -> Diagnosis {
    let mut diagnosis = Diagnosis {
        ran: Vec::new(),
        skipped: Vec::new(),
        bios_context_error: None,
    };
    for outcome in host.run_steps(&[Step::Cpu, Step::RyzenSmu, Step::Readback], privileged) {
        match outcome {
            Outcome::Ran(check) => diagnosis.ran.push(check),
            Outcome::Skipped(skip) => diagnosis.skipped.push(skip),
        }
    }
    let (ok, detail) = match watchdog {
        WatchdogState::Armed { detail } => (true, detail),
        WatchdogState::Unarmed { detail } => (false, detail),
    };
    diagnosis.ran.push(check("watchdog", detail, ok));
    diagnosis.ran.push(persistent);
    diagnosis
}

/// Query only persistent storage and this exact boot, not the most recent boot in that directory.
/// An empty directory, an old boot, or a failed query never proves persistent current-boot logging.
fn persistent_with(
    boot: &BootId,
    run: impl FnOnce(&Invocation) -> Result<Output, togi_command::Error>,
) -> Check {
    let invocation = Invocation::new("journalctl")
        .args([
            "--directory=/var/log/journal",
            "--no-pager",
            "--quiet",
            "--lines=1",
            "--output=json",
            "--output-fields=_BOOT_ID",
        ])
        .arg(format!("--boot={}", boot.as_str()))
        .output_limit(64 * 1024);
    let output = match run(&invocation) {
        Ok(output) => output,
        Err(error) => {
            return check(
                "persistent_journal",
                togi_machine::error_chain(&error),
                false,
            );
        }
    };
    if !output.status.success() {
        return check(
            "persistent_journal",
            format!(
                "read persistent journal: {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            false,
        );
    }
    let entry = serde_json::from_slice::<serde_json::Value>(&output.stdout).ok();
    let recorded = entry
        .as_ref()
        .and_then(|entry| entry.get("_BOOT_ID")?.as_str());
    // journalctl renders boot IDs without the kernel UUID's hyphens.
    match recorded {
        Some(recorded)
            if recorded
                .chars()
                .eq(boot.as_str().chars().filter(|ch| *ch != '-')) =>
        {
            check(
                "persistent_journal",
                "this boot has an entry in /var/log/journal; retention and future writes are not guaranteed",
                true,
            )
        }
        _ => check(
            "persistent_journal",
            "no readable entry for this boot in /var/log/journal; volatile or earlier-boot logs are not proof",
            false,
        ),
    }
}

fn check(name: &str, detail: impl Into<String>, ok: bool) -> Check {
    Check {
        name: name.into(),
        detail: detail.into(),
        ok,
    }
}

#[cfg(test)]
mod tests;
