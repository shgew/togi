//! `togi emergency-stop`: stop what a dead togi left running and set every core to 0, and nothing else.

use std::fmt;
use std::io::Write;
use std::sync::LazyLock;

use clap::Args as ClapArgs;
use togi_config::Config;
use togi_hardware::emergency::{Failure, Recovery, stop_locked};
use togi_hostlock::Error as LockError;
use togi_machine::{Error as MachineError, ObservedOffset, Readback};

use crate::cli::{Context, examples};
use crate::exit::Exit;
use crate::report::{exit_with, fail};

static EXAMPLES: LazyLock<String> = LazyLock::new(|| {
    examples(&[(
        "sudo togi emergency-stop",
        "Stop leftover trials, then set every core to 0 and read each back",
    )])
});

/// Internal emergency recovery that stops leftover trials and sets every core to offset 0
///
/// Internal recovery for a togi process that died with trial workloads or Curve Optimizer offsets
/// left behind; the services run it after an abnormal exit. It requires Linux and root. It tries
/// to take the host lock, validates the CPU, the SMU driver and the core topology, stops every
/// stale togi trial scope and confirms none is left. Only then does it write 0 to every core
/// and read every core back.
///
/// It accepts no offset or configuration and never opens the session journal. A foreign-owned
/// or nonregular lock entry (including a symlink) does not prevent this zero-only recovery.
/// Exit status 0 means every core read back 0 under the lock; 4 means every core read back 0
/// but exclusion from another writer was not proven. A busy lock still leaves offsets unchanged
/// and exits 3. A failed validation or slot mapping or an unconfirmed sweep still blocks zeroing.
/// Other refusals and failures exit 1, with the cause on stderr.
#[derive(ClapArgs, Debug)]
#[command(after_help = EXAMPLES.as_str())]
pub struct Args {}

const COMMAND: &str = "emergency-stop";

#[derive(Debug, thiserror::Error)]
#[error("{missing} of {cores} cores did not read back 0")]
struct Unconfirmed {
    missing: usize,
    cores: usize,
    #[source]
    zero: Option<ZeroFailed>,
}

#[derive(Debug, thiserror::Error)]
#[error("set every core to 0")]
struct ZeroFailed(#[source] MachineError);

fn readback_line(readback: &Readback) -> String {
    match &readback.result {
        Ok(ObservedOffset(0)) => format!("core {:02} read back 0", readback.core),
        Ok(offset) => format!("core {:02} reads {}", readback.core, offset.0),
        Err(error) => format!(
            "core {:02} unreadable: {}",
            readback.core,
            togi_render::error_chain(error)
        ),
    }
}

fn say(report: &mut dyn Write, line: impl fmt::Display) {
    let _ = writeln!(report, "togi {COMMAND}: {line}");
}

/// Stops stale trials and zeroes every core without opening the session journal.
pub fn run(_args: &Args, ctx: &mut Context<'_>) -> Exit {
    let system = ctx.system;
    if let Err(error) = system.check_platform() {
        return fail(ctx, COMMAND, togi_render::error_chain(&error));
    }
    if !system.privileged() {
        return fail(
            ctx,
            COMMAND,
            "needs root to write the SMU; offsets unchanged",
        );
    }
    let (recovery, success) = match system.acquire_host_lock() {
        Ok(lock) => {
            #[expect(
                clippy::disallowed_methods,
                reason = "emergency-stop is the one zero-only recovery under the host lock"
            )]
            let result = system
                .open_machine(&Config::default(), ctx.state_dir, &lock)
                .map_err(Failure::from)
                .and_then(|mut machine| {
                    stop_locked(&mut machine, |detail| {
                        say(ctx.err, format_args!("trial scopes: {detail}"));
                    })
                });
            (result, Exit::Ok)
        }
        Err(LockError::RefusedEntry(refused)) => {
            say(
                ctx.err,
                format_args!("{refused}; attempting zero-only recovery without the lock"),
            );
            let result = system.recover_without_exclusion(&refused, ctx.state_dir, &mut |detail| {
                say(ctx.err, format_args!("trial scopes: {detail}"));
            });
            (result, Exit::ZeroedWithoutExclusion)
        }
        Err(error @ LockError::Busy { .. }) => {
            return exit_with(
                ctx,
                COMMAND,
                format!("{error}; offsets unchanged"),
                Exit::Locked,
            );
        }
        Err(
            error @ (LockError::NotProvisioned { .. }
            | LockError::UnsafeDirectory { .. }
            | LockError::UnsafeLock { .. }
            | LockError::Io { .. }),
        ) => {
            return fail(
                ctx,
                COMMAND,
                format!("{}; offsets unchanged", togi_render::error_chain(&error)),
            );
        }
    };
    let Recovery { cores, zeroed } = match recovery {
        Ok(recovered) => recovered,
        Err(error) => {
            return fail(
                ctx,
                COMMAND,
                format!("{}; offsets unchanged", togi_render::error_chain(&error)),
            );
        }
    };
    for readback in &zeroed.readbacks {
        say(ctx.err, readback_line(readback));
    }
    if !zeroed.is_clean() {
        let unconfirmed = Unconfirmed {
            missing: cores
                - zeroed
                    .readbacks
                    .iter()
                    .filter(|readback| readback.is_zero())
                    .count(),
            cores,
            zero: zeroed.zero.err().map(ZeroFailed),
        };
        return fail(ctx, COMMAND, togi_render::error_chain(&unconfirmed));
    }
    let assurance = if success == Exit::ZeroedWithoutExclusion {
        "; exclusion from another writer was not proven"
    } else {
        ""
    };
    exit_with(
        ctx,
        COMMAND,
        format!("every core read back 0 ({cores} cores){assurance}"),
        success,
    )
}
