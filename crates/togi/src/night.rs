//! `togi night`: switch from the day into the tuning boot without the boot menu.

use std::io::ErrorKind;
use std::path::Path;
use std::sync::LazyLock;

use clap::Args as ClapArgs;
use togi_journal::{Payload, SessionStart, Stamp, StockAttested};
use togi_tuningboot::night::{BootSelection, SETUP_PATH, Selection, Setup};

use crate::cli::{Context, examples};
use crate::exit::Exit;
use crate::report::{emit, fail};
use crate::{read, status};

static EXAMPLES: LazyLock<String> = LazyLock::new(|| {
    examples(&[(
        "sudo togi night",
        "Show what the night will do, then reboot into the tuning boot until you end it",
    )])
});

/// Reboot into the tuning boot for the night
///
/// Shows what the night will do, as `togi status` plans it, then asks to reboot and confirm
/// stability at CO 0 with BIOS at stock. Yes durably records that assertion once per firmware
/// context before changing the boot selection or rebooting. Anything but yes changes nothing.
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
            status::plan(events, &model, &ctx.system.zone())
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
    let _host = match attest(ctx) {
        Ok(lock) => lock,
        Err(exit) => return exit,
    };
    let bootloader = match ctx.system.bootloader(setup.boot_path()) {
        Ok(bootloader) => bootloader,
        Err(error) => return fail(ctx, COMMAND, togi_render::error_chain(&error)),
    };
    let mut selection = BootSelection::new(bootloader);
    if let Err(error) = selection.begin(setup.entry()) {
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

/// The normal mutation lifecycle also makes confirmation durable before the first tuning boot.
/// Starting the envelope here leaves baseline readback and context observation to `run`.
fn attest(ctx: &mut Context<'_>) -> Result<Option<togi_hostlock::HostLock>, Exit> {
    let system = ctx.system;
    if let Some(recorded) = system
        .recorded_build(ctx.state_dir)
        .map_err(|error| fail(ctx, COMMAND, togi_render::error_chain(&error)))?
        && crate::report::current_build().verdict(&recorded, togi_journal::Operation::Append)
            == togi_journal::Verdict::Use
    {
        let recording = system
            .recording(&ctx.state_dir.join("events.jsonl"))
            .map_err(|error| fail(ctx, COMMAND, togi_render::error_chain(&error)))?;
        if let Some(Payload::SessionStart(start)) =
            recording.events().first().map(|event| &event.payload)
        {
            use crate::day::socket::{AttestStockRequest, Error, Reply, Request};
            match system.day_request(&Request::AttestStock(AttestStockRequest {
                session: start.session,
            })) {
                Ok(Reply::Controlled { .. }) => return Ok(None),
                Err(Error::Absent(_)) => {}
                Ok(Reply::Refused { reason }) => return Err(fail(ctx, COMMAND, reason)),
                Ok(
                    Reply::Questions { .. }
                    | Reply::State { .. }
                    | Reply::Answered
                    | Reply::Recorded { .. }
                    | Reply::Reapplied,
                ) => {
                    return Err(fail(ctx, COMMAND, "unexpected day service reply"));
                }
                Err(error) => return Err(fail(ctx, COMMAND, togi_render::error_chain(&error))),
            }
        }
    }
    let lock = system.acquire_host_lock().map_err(|error| {
        let exit = if matches!(error, togi_hostlock::Error::Busy { .. }) {
            Exit::Locked
        } else {
            Exit::Error
        };
        crate::report::exit_with(ctx, COMMAND, togi_render::error_chain(&error), exit)
    })?;
    let firmware = system
        .firmware_context(&lock)
        .map_err(|error| fail(ctx, COMMAND, togi_render::error_chain(&error)))?;
    let boot = system
        .boot_id()
        .map_err(|error| fail(ctx, COMMAND, togi_render::error_chain(&error)))?;
    let dir = system.lock_state_dir(ctx.state_dir).map_err(|error| {
        let exit = if matches!(error, togi_journal::LockError::Held { .. }) {
            Exit::Locked
        } else {
            Exit::Error
        };
        crate::report::exit_with(ctx, COMMAND, togi_render::error_chain(&error), exit)
    })?;
    let build = crate::report::current_build();
    let prepared = togi_session::prepare(dir, &build, boot, system.now())
        .map_err(|error| fail(ctx, COMMAND, togi_render::error_chain(&error)))?;
    let mut journal = prepared.journal;
    if journal.events().is_empty() {
        let session = match prepared.import {
            Some(import) => import.destination(),
            None => journal
                .state_dir()
                .session_id(system.now())
                .map_err(|error| fail(ctx, COMMAND, togi_render::error_chain(&error)))?,
        };
        let cores = system
            .topology()
            .map_err(|error| fail(ctx, COMMAND, togi_render::error_chain(&error)))?;
        journal
            .append(
                stamp(ctx)?,
                SessionStart {
                    build,
                    session,
                    cores,
                }
                .into(),
                &[],
            )
            .map_err(|error| fail(ctx, COMMAND, togi_render::error_chain(&error)))?;
    }
    if !journal.events().iter().any(|event| {
        matches!(&event.payload, Payload::StockAttested(attested) if attested.firmware == firmware)
    }) {
        journal
            .append(stamp(ctx)?, StockAttested { firmware }.into(), &[])
            .map_err(|error| fail(ctx, COMMAND, togi_render::error_chain(&error)))?;
    }
    journal
        .close()
        .map_err(|error| fail(ctx, COMMAND, togi_render::error_chain(&error)))?;
    Ok(Some(lock))
}

fn stamp(ctx: &mut Context<'_>) -> Result<Stamp, Exit> {
    Ok(Stamp {
        time: ctx.system.now(),
        mono: Some(
            ctx.system
                .monotonic()
                .map_err(|error| fail(ctx, COMMAND, togi_render::error_chain(&error)))?,
        ),
    })
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
    if write!(ctx.out, "Confirm the machine is stable at CO 0 with BIOS at stock, and reboot into the night now? [y/N] ")
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

/// Whether `line` is a complete line that says yes: `y` or `yes`, in any case, after trimming.
fn parse_answer(line: &str) -> bool {
    line.ends_with('\n') && {
        let answer = line.trim();
        answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
    }
}

#[cfg(test)]
mod tests;
