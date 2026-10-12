//! `togi restart-limit`: the tuning service's recovery after it hits its restart limit.

use std::path::PathBuf;
use std::sync::LazyLock;

use clap::builder::{NonEmptyStringValueParser, TypedValueParser as _};
use clap::{Args as ClapArgs, ValueHint};
use togi_tuningboot::{RestartLimit, restart_limit};

use crate::cli::{Context, examples};
use crate::exit::Exit;
use crate::report::{chain, fail};

static EXAMPLES: LazyLock<String> = LazyLock::new(|| {
    examples(&[(
        "sudo togi restart-limit --tuning-boot /boot/grub/grubenv",
        "Retry the tuning boot twice, then return to the normal system",
    )])
});

/// Recover after the tuning service reaches its restart limit
///
/// Persist the count and leave reason in the GRUB environment before requesting a reboot. The
/// first two consecutive restart-limit hits reboot back into the tuning boot; the third clears and
/// verifies `saved_entry` so the reboot selects the normal system. The first durable journal append
/// by run resets this count.
///
/// A retry creates /run/togi-retry-tuning-boot after the GRUB write succeeds, so the shutdown
/// service keeps the tuning entry selected. A failed reboot removes the marker. Errors exit
/// unsuccessfully so the service can fall back to a normal boot. This command does not open the
/// journal or access the CPU hardware.
#[derive(ClapArgs, Debug)]
#[command(after_help = EXAMPLES.as_str())]
pub struct Args {
    /// GRUB environment file for retry state and the saved tuning entry
    #[arg(
        long,
        required = true,
        value_name = "file",
        value_hint = ValueHint::FilePath,
        value_parser = NonEmptyStringValueParser::new().map(PathBuf::from),
    )]
    pub tuning_boot: PathBuf,
}

const COMMAND: &str = "restart-limit";
const ATTEMPTS: u8 = 3;

/// Records the hit in GRUB, sets or clears the retry marker and requests the reboot.
pub fn run(args: &Args, ctx: &mut Context<'_>) -> Exit {
    let system = ctx.system;
    let mut bootloader = system.bootloader(&args.tuning_boot);
    let limit = match restart_limit(&mut *bootloader) {
        Ok(limit) => limit,
        Err(error) => return fail(ctx, COMMAND, chain(&error)),
    };
    let (count, retry) = match limit {
        RestartLimit::Retry(count) => (count.get(), true),
        RestartLimit::Exhausted => (ATTEMPTS, false),
    };
    if retry {
        if let Err(error) = system.create_retry_marker() {
            return fail(ctx, COMMAND, format!("create retry marker: {error}"));
        }
        let _ = writeln!(
            ctx.err,
            "togi {COMMAND}: attempt {count} of {ATTEMPTS}; rebooting back into the tuning boot"
        );
    } else {
        if let Err(error) = system.remove_retry_marker() {
            return fail(ctx, COMMAND, format!("remove retry marker: {error}"));
        }
        let _ = writeln!(
            ctx.err,
            "togi {COMMAND}: attempt {count} of {ATTEMPTS}; rebooting into the normal system"
        );
    }
    if let Err(error) = system.reboot() {
        let exit = fail(ctx, COMMAND, chain(&error));
        if retry && let Err(error) = system.remove_retry_marker() {
            let _ = writeln!(
                ctx.err,
                "togi {COMMAND}: remove retry marker after failed reboot: {error}"
            );
        }
        return exit;
    }
    Exit::Ok
}
