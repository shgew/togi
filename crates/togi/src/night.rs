//! `togi night`: switch from the day into the tuning boot without the boot menu.

use std::io::ErrorKind;
use std::path::Path;
use std::sync::LazyLock;

use clap::Args as ClapArgs;
use togi_tuningboot::night::{BootSelection, SETUP_PATH, Selection, Setup};

use crate::cli::{Context, examples};
use crate::exit::Exit;
use crate::report::{emit, fail};
use crate::run::prompt::parse_answer;
use crate::{read, status};

static EXAMPLES: LazyLock<String> = LazyLock::new(|| {
    examples(&[(
        "sudo togi night",
        "Show what the night will do, then reboot into the tuning boot until you end it",
    )])
});

/// Reboot into the tuning boot for the night
///
/// Shows what the night will do, as `togi status` plans it, then asks to reboot. On yes it says
/// good night, points the boot loader at the tuning entry, remembering the selection it replaces,
/// and reboots. Anything but yes changes nothing.
///
/// The night has no time limit: it runs until you end it with F10 on the tuning dashboard or the
/// power button, or until the tuner stops on its own at a dead end or its restart limit. Each
/// restores the selection the night replaced exactly. Crash reboots stay in the night.
///
/// Needs root, and the NixOS module's tuning boot, which writes /etc/togi/night.json.
#[derive(ClapArgs, Debug)]
#[command(after_help = EXAMPLES.as_str())]
pub struct Args {}

const COMMAND: &str = "night";

/// How the night is confirmed before the machine reboots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmation {
    /// This command asks.
    Ask,
    /// The caller has asked already: the key in `togi watch`.
    Given,
}

/// Runs `togi night`.
pub fn run(_args: &Args, ctx: &mut Context<'_>) -> Exit {
    begin(ctx, Confirmation::Ask)
}

/// What the night switch needs from the module's setup file.
///
/// # Errors
///
/// The message for stderr when the file is missing, unreadable or invalid.
pub fn setup(ctx: &Context<'_>) -> Result<Setup, String> {
    let text = ctx
        .system
        .read_config(Path::new(SETUP_PATH))
        .map_err(|error| {
            if error.kind() == ErrorKind::NotFound {
                format!(
                    "no tuning boot is set up on this machine: {SETUP_PATH} is missing; the NixOS module writes it when tuning is enabled"
                )
            } else {
                format!("read {SETUP_PATH}: {}", togi_render::error_chain(&error))
            }
        })?;
    text.parse()
        .map_err(|error| format!("invalid {SETUP_PATH}: {}", togi_render::error_chain(&error)))
}

/// What the night will do, as `togi status` plans it today. The crash range Ruleset 13 forecasts
/// joins these lines here; until it does, the night shows what the planner offers.
fn plan(ctx: &mut Context<'_>) -> Result<Vec<String>, Exit> {
    let line = match ctx.system.recorded_build(ctx.state_dir) {
        Ok(None) => None,
        Ok(Some(_)) => {
            let recording = read::open(ctx, COMMAND)?;
            let events = recording.events();
            let model = match ctx.system.journal_fs() {
                Some(fs) => togi_watch::model::Model::read_in(&*fs, ctx.state_dir, events),
                None => togi_watch::model::Model::read(ctx.state_dir, events),
            };
            status::plan(events, &model)
                .map_err(|error| fail(ctx, COMMAND, togi_render::error_chain(&error)))?
        }
        Err(error) => return Err(fail(ctx, COMMAND, togi_render::error_chain(&error))),
    };
    Ok(vec![line.map_or_else(
        || "no tuning session yet; the night starts one".to_owned(),
        |line| format!("tonight: {line}"),
    )])
}

/// Shows the night, confirms it as `confirmation` says, and starts it: the tuning entry selected,
/// the day's selection remembered, the machine rebooting. Declining changes nothing.
pub fn begin(ctx: &mut Context<'_>, confirmation: Confirmation) -> Exit {
    if !ctx.system.privileged() {
        return fail(
            ctx,
            COMMAND,
            "the night changes the boot loader and needs root",
        );
    }
    let setup = match setup(ctx) {
        Ok(setup) => setup,
        Err(message) => return fail(ctx, COMMAND, message),
    };
    let lines = match plan(ctx) {
        Ok(lines) => lines,
        Err(exit) => return exit,
    };
    for line in lines {
        if let Err(exit) = emit(ctx, line) {
            return exit;
        }
    }
    let summary = "The night runs until you end it, or until the tuner stops on its own.";
    if let Err(exit) = emit(ctx, summary) {
        return exit;
    }
    if confirmation == Confirmation::Ask {
        match ask(ctx) {
            Ok(true) => {}
            Ok(false) => return unchanged(ctx),
            Err(exit) => return exit,
        }
    }
    let mut selection = BootSelection::new(ctx.system.bootloader(&setup.grubenv));
    if let Err(error) = selection.begin(&setup.entry) {
        return fail(ctx, COMMAND, togi_render::error_chain(&error));
    }
    let goodnight = "Good night. Rebooting into the tuning boot; F10 on its dashboard or the power button brings back the day.";
    if let Err(exit) = emit(ctx, goodnight) {
        restore_day(ctx, &mut selection, "the failed message");
        return exit;
    }
    if let Err(error) = ctx.system.reboot() {
        let exit = fail(ctx, COMMAND, togi_render::error_chain(&error));
        restore_day(ctx, &mut selection, "the failed reboot");
        return exit;
    }
    Exit::Ok
}

/// Gives the tuning selection back after the night failed to start, so no later reboot enters a night the command
/// reported as failed.
fn restore_day(ctx: &mut Context<'_>, selection: &mut impl Selection, after: &str) {
    if let Err(error) = selection.end() {
        let _ = writeln!(
            ctx.err,
            "togi {COMMAND}: restore the day's boot entry after {after}: {}",
            togi_render::error_chain(&error)
        );
    }
}

/// Asks whether to reboot into the night and reads the answer typed after the question.
fn ask(ctx: &mut Context<'_>) -> Result<bool, Exit> {
    if write!(ctx.out, "Reboot into the night now? [y/N] ")
        .and_then(|()| ctx.out.flush())
        .is_err()
    {
        return Err(fail(ctx, COMMAND, "write the question"));
    }
    match ctx.system.answer() {
        Ok(line) => {
            let _ = writeln!(ctx.out);
            Ok(line.is_some_and(|line| parse_answer(&line)))
        }
        Err(error) => Err(fail(
            ctx,
            COMMAND,
            format!("read the answer: {}", togi_render::error_chain(&error)),
        )),
    }
}

fn unchanged(ctx: &mut Context<'_>) -> Exit {
    match emit(ctx, "Nothing changed.") {
        Ok(()) => Exit::Ok,
        Err(exit) => exit,
    }
}
