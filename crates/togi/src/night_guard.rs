//! `togi night-guard`: the early-boot guard of the tuning boot.

use togi_tuningboot::night::{GrubSelection, Guard, guard};

use std::sync::LazyLock;

use crate::cli::{Context, examples};
use crate::exit::Exit;
use crate::night::setup;
use crate::report::fail;

static EXAMPLES: LazyLock<String> = LazyLock::new(|| {
    examples(&[(
        "sudo togi night-guard",
        "Record this tuning boot, then end the night or let the tuner start",
    )])
});

/// Return to the day when the night is over, before the tuner starts
///
/// Runs first in every tuning boot. It records the boot in the GRUB environment, where the
/// count needs neither the tuner nor its journal. When the morning time has passed, or too many
/// crash reboots came too fast, it restores the day's boot selection exactly and reboots; the
/// tuner never starts. Otherwise it exits 0 and the tuner may start. A tuning boot entered from
/// the boot menu gets a night that ends at the next morning time.
///
/// An error exits unsuccessfully, which keeps the tuner from starting unguarded.
#[derive(clap::Args, Debug)]
#[command(after_help = EXAMPLES.as_str())]
pub struct Args {}

const COMMAND: &str = "night-guard";

/// Records this boot, then ends the night or lets it go on.
pub fn run(_args: &Args, ctx: &mut Context<'_>) -> Exit {
    let setup = match setup(ctx) {
        Ok(setup) => setup,
        Err(message) => return fail(ctx, COMMAND, message),
    };
    let mut selection = GrubSelection::new(ctx.system.bootloader(&setup.grubenv));
    let decision = guard(
        &mut selection,
        ctx.system.now(),
        setup.morning,
        &ctx.system.zone(),
    );
    match decision {
        Ok(Guard::Tune(night)) => {
            let _ = writeln!(
                ctx.err,
                "togi {COMMAND}: the night goes on until {}",
                night.until()
            );
            Exit::Ok
        }
        Ok(Guard::Return(end)) => {
            let _ = writeln!(ctx.err, "togi {COMMAND}: {end}; returning to the day");
            match ctx.system.reboot() {
                Ok(()) => Exit::Ok,
                Err(error) => fail(ctx, COMMAND, togi_render::error_chain(&error)),
            }
        }
        Err(error) => fail(ctx, COMMAND, togi_render::error_chain(&error)),
    }
}
