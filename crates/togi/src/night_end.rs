//! `togi night-end`: leave the night for the day.

use togi_tuningboot::night::{GrubSelection, Selection as _};

use std::sync::LazyLock;

use crate::cli::{Context, examples};
use crate::exit::Exit;
use crate::night::setup;
use crate::report::{emit, fail};

static EXAMPLES: LazyLock<String> = LazyLock::new(|| {
    examples(&[(
        "sudo togi night-end",
        "Select again the boot entry the night replaced",
    )])
});

/// Restore the day's boot selection
///
/// Selects again exactly the boot entry `togi night` replaced, or none when the day had none or
/// the tuning boot was picked from the boot menu, and ends the night. It never selects a generation of its own. It does not reboot: the tuning
/// boot's shutdown hook runs it when the power button or a reboot ends the night. Repeating it
/// changes nothing.
#[derive(clap::Args, Debug)]
#[command(after_help = EXAMPLES.as_str())]
pub struct Args {}

const COMMAND: &str = "night-end";

/// Restores the selection and says what it was and is.
pub fn run(_args: &Args, ctx: &mut Context<'_>) -> Exit {
    let setup = match setup(ctx) {
        Ok(setup) => setup,
        Err(message) => return fail(ctx, COMMAND, message),
    };
    restore(
        ctx,
        &mut GrubSelection::new(ctx.system.bootloader(&setup.grubenv)),
    )
}

fn restore(ctx: &mut Context<'_>, selection: &mut GrubSelection) -> Exit {
    match selection.end() {
        Ok(entry) => match emit(
            ctx,
            format!(
                "boot entry restored to {:?}, was {:?}",
                entry.after, entry.before
            ),
        ) {
            Ok(()) => Exit::Ok,
            Err(exit) => exit,
        },
        Err(error) => fail(ctx, COMMAND, togi_render::error_chain(&error)),
    }
}

/// F10 on the tuning dashboard, once confirmed: stops the run cleanly, restores the day's
/// selection and reboots. A run that does not stop leaves everything as it was.
pub fn leave(ctx: &mut Context<'_>) -> Exit {
    if !ctx.system.privileged() {
        return fail(
            ctx,
            COMMAND,
            "leaving the night changes the boot loader and needs root",
        );
    }
    let setup = match setup(ctx) {
        Ok(setup) => setup,
        Err(message) => return fail(ctx, COMMAND, message),
    };
    if let Err(error) = ctx.system.stop_tuning() {
        return fail(
            ctx,
            COMMAND,
            format!("stop the run: {}", togi_render::error_chain(&error)),
        );
    }
    let mut selection = GrubSelection::new(ctx.system.bootloader(&setup.grubenv));
    let exit = restore(ctx, &mut selection);
    if exit != Exit::Ok {
        return exit;
    }
    let _ = writeln!(ctx.err, "togi {COMMAND}: rebooting into the day cycle");
    match ctx.system.reboot() {
        Ok(()) => Exit::Ok,
        Err(error) => fail(ctx, COMMAND, togi_render::error_chain(&error)),
    }
}
