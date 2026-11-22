//! `togi run`: start or resume the tuning session in the foreground.
//!
//! The order of a run is fixed by `docs/spec/runtime.md`. The platform is checked first. What reading alone
//! decides comes next, before any lock: a journal this build may not append to (a refusal appends nothing) and
//! the configuration. Then the process's own signal listener starts and the host lock and the journal's lock
//! are taken, in that order, as `reset` does; the journal is judged again under them. Only then is the boot id
//! read, the hardware opened, the carry prepared and the session run. Once the host lock is held and the
//! hardware set up, the session owns the cleanup of every exit; a panic in it comes back with that cleanup
//! done and its outcome reported, and is resumed after the journal and the host lock are put away.

use std::path::PathBuf;
use std::sync::LazyLock;

use clap::{Args as ClapArgs, ValueHint};
use togi_defect::KNOWN_DEFECTS;
use togi_hostlock::HostLock;
use togi_journal::{Build, Operation, Verdict};
use togi_machine::CancellationToken;
use togi_render::{Renderer, Style, escape_text};
use togi_session::{PrepareError, RunInput, SessionError, prepare};
use togi_tuningboot::{Bootloader, LeaveReason, RestartCount};

use crate::args::ConfigArgs;
use crate::cli::{Context, examples};
use crate::exit::Exit;
use crate::report::{current_build, exit_with, refusal_message, unknown_event_message};

mod outcome;

static EXAMPLES: LazyLock<String> = LazyLock::new(|| examples(&[crate::cli::RUN_EXAMPLE]));

/// Start or resume the night's tuning session in the foreground
///
/// Runs the night of the per-CCD tuning policy: controls, coverage mapping, qualification and reproduction, then
/// cap tests and re-qualification while nothing else is due. The night has no deadline: it runs until you stop it
/// (a signal or a reboot), a conclusive failure at CO 0, no configured backend giving evidence any more,
/// or a runtime safety dead end. A restart after a crash continues the night; a restart after a
/// stop or a normal boot opens the next one. After a crash the next run attributes it from the journal and
/// continues. On resume, known defects affecting past decisions are recorded with their cores; a too-aggressive
/// one stops the run until reset --all starts a fresh session. It needs root.
///
/// A session starts only from a verified all-zero baseline and returns every core to CO 0 when it stops. A
/// BIOS change found on resume is recorded and the session continues. A journal from an older ruleset, schema or
/// evidence epoch, and no newer one, is archived. A newer ruleset, schema or evidence epoch stops the run before
/// another event is written; reset --all archives that session. Journal lines are colored on terminals and in
/// the system journal unless the environment asks for no colour.
///
/// Every run requires an armed hardware watchdog before each nonzero profile application and each workload start.
/// Manual startup checks once and refuses immediately; --tuning-boot waits up to 30 seconds.
/// Loss detected at a later boundary stops the run and restores CO 0.
///
/// Only one day observer, run or reset can own this machine, even with different state directories.
/// Stop the day service before tuning. The NixOS module creates /run/lock/togi.lock at boot;
/// run can create it if it is missing.
/// If the lock cannot be taken, no hardware is accessed and no event is written.
///
/// Prints one line per journal event and any final diagnostics to stderr, even on a terminal.
/// Use `togi watch` in a separate terminal to see the dashboard.
///
/// --tuning-boot runs the unattended service with an armed hardware watchdog. The first durable journal
/// append resets its consecutive restart-limit count and records a pending leave reason once. Dead ends and
/// incompatible journals persist a short leave reason before clearing the saved GRUB entry.
#[derive(ClapArgs, Debug)]
#[command(after_help = EXAMPLES.as_str())]
pub struct Args {
    #[command(flatten)]
    pub config: ConfigArgs,

    /// Run as the tuning boot service: use the GRUB environment to persist a leave reason before
    /// restoring the day's boot selection, and reboot after a boot loop
    #[arg(long, value_name = "file", value_hint = ValueHint::FilePath)]
    pub tuning_boot: Option<PathBuf>,
}

/// Runs the command.
pub fn run(args: &Args, ctx: &mut Context<'_>) -> Exit {
    if let Err(error) = ctx.system.check_platform() {
        return exit_with(ctx, COMMAND, togi_render::error_chain(&error), Exit::Error);
    }
    let renderer = Renderer::new(ctx.system.stderr_facts());
    let build = current_build();
    let mut bootloader = match args
        .tuning_boot
        .as_deref()
        .map(|selection| ctx.system.bootloader(selection))
        .transpose()
    {
        Ok(bootloader) => bootloader,
        Err(error) => {
            return exit_with(ctx, COMMAND, togi_render::error_chain(&error), Exit::Error);
        }
    };

    // What a run can decide from reading alone comes first, before any lock: a journal this build may not
    // append to, and the configuration. Neither writes anything, so neither needs the host lock, except the
    // tuning boot's refusal, which clears the boot entry. The same journal is judged again under the locks.
    let unlocked = match ctx.system.recorded_build(ctx.state_dir) {
        Ok(recorded) => recorded,
        Err(error) => {
            return exit_with(ctx, COMMAND, togi_render::error_chain(&error), Exit::Error);
        }
    };
    if let Some(refused) = refusal(&build, unlocked.as_ref(), ctx) {
        let _lock = match bootloader {
            Some(_) => match take_host_lock(ctx) {
                Ok(lock) => Some(lock),
                Err(exit) => return exit,
            },
            None => None,
        };
        return refuse(ctx, &refused, bootloader.as_deref_mut(), renderer);
    }
    let (config, source) = match args
        .config
        .load_with_source_read(ctx.system.config_path(), |path| {
            ctx.system.read_config(path)
        }) {
        Ok(loaded) => loaded,
        Err(error) => {
            return exit_with(ctx, COMMAND, togi_render::error_chain(&error), Exit::Usage);
        }
    };

    let cancel = CancellationToken::new();
    let _listening = match ctx.system.listen(&cancel) {
        Ok(listening) => listening,
        Err(error) => {
            return exit_with(ctx, COMMAND, togi_render::error_chain(&error), Exit::Error);
        }
    };

    // The host lock, then the journal's lock: `reset` takes them in this order too, so the two commands cannot
    // each hold the lock the other waits for.
    let lock = match take_host_lock(ctx) {
        Ok(lock) => lock,
        Err(exit) => return exit,
    };
    let dir = match ctx.system.lock_state_dir(ctx.state_dir) {
        Ok(dir) => dir,
        Err(error @ togi_journal::LockError::Held { .. }) => {
            return exit_with(ctx, COMMAND, error, Exit::Locked);
        }
        Err(error) => {
            return exit_with(ctx, COMMAND, togi_render::error_chain(&error), Exit::Error);
        }
    };
    let recorded = match dir.live_build() {
        Ok(recorded) => recorded,
        Err(error) => {
            return exit_with(ctx, COMMAND, togi_render::error_chain(&error), Exit::Error);
        }
    };
    if let Some(refused) = refusal(&build, recorded.as_ref(), ctx) {
        return refuse(ctx, &refused, bootloader.as_deref_mut(), renderer);
    }

    let boot = match ctx.system.boot_id() {
        Ok(boot) => boot,
        Err(error) => {
            return exit_with(ctx, COMMAND, togi_render::error_chain(&error), Exit::Error);
        }
    };
    let machine = match ctx.system.open_machine(&config, ctx.state_dir, &lock) {
        Ok(machine) => machine,
        Err(error) => {
            return exit_with(ctx, COMMAND, togi_render::error_chain(&error), Exit::Error);
        }
    };
    let prepared = match prepare(dir, &build, boot.clone(), ctx.system.now()) {
        Ok(prepared) => prepared,
        Err(PrepareError::Refused(refusal)) => {
            let refused = Refused {
                message: recorded.as_ref().map_or_else(
                    || refusal.to_string(),
                    |recorded| refusal_message(&refusal, recorded, &build),
                ),
                leave_reason: INCOMPATIBLE,
            };
            return refuse(ctx, &refused, bootloader.as_deref_mut(), renderer);
        }
        Err(error) => {
            return exit_with(ctx, COMMAND, togi_render::error_chain(&error), Exit::Error);
        }
    };
    let journal = prepared.journal;

    let zone = ctx.system.zone();
    let input = RunInput {
        config,
        source,
        build,
        boot,
        journal,
        machine,
        bootloader,
        defects: KNOWN_DEFECTS,
        resume: None,
        import: prepared.import,
        log: Some(ctx.system.stderr()),
        on_append: None,
        renderer,
        zone: zone.clone(),
        cancel,
        report: ctx.system.stderr(),
    };
    let mut ended = togi_session::run(input);
    // The process's entry point reports the panic once cleanup and any cleanup diagnostics are done.
    let panic = ended.panic.take();
    if panic.is_some() {
        ended
            .errors
            .retain(|error| !matches!(error, SessionError::Panicked(_)));
    }
    let result = outcome::conclude(
        ctx.err,
        renderer,
        &zone,
        ended.stop.as_ref(),
        &ended.errors,
        ended.journal.events(),
    );
    let dead_end = ended
        .stop
        .as_ref()
        .filter(|stop| matches!(stop, togi_session::Stop::DeadEnd { .. }));
    if panic.is_some()
        && !ended.errors.is_empty()
        && let Some(dead_end) = dead_end
    {
        outcome::conclude(
            ctx.err,
            renderer,
            &zone,
            Some(dead_end),
            &[],
            ended.journal.events(),
        );
    }
    if ended.stock_unverified {
        outcome::stock_not_verified(ctx.err, renderer);
    }
    let reboot_asked = matches!(
        dead_end,
        Some(togi_session::Stop::DeadEnd { reboot: true, .. })
    );
    let mut exit = result.exit;
    if let Err(error) = ended.journal.close() {
        let _ = writeln!(
            ctx.err,
            "togi run: {}",
            escape_text(&togi_render::error_chain(&error))
        );
        if exit == Exit::Ok {
            exit = Exit::Error;
        }
    }
    drop(lock);
    if panic.is_some() && reboot_asked {
        let _ = writeln!(
            ctx.err,
            "togi: a dead end asked for a reboot into the normal system; not rebooting after a panic"
        );
    } else if result.reboot {
        let _ = writeln!(ctx.err, "togi: rebooting into the normal system");
        if let Err(error) = ctx.system.reboot() {
            let _ = writeln!(
                ctx.err,
                "togi: systemctl reboot: {}",
                escape_text(&togi_render::error_chain(&error))
            );
        }
    }
    if let Some(panic) = panic {
        panic.resume();
    }
    exit
}

/// Takes the host lock, or says why not and how the run exits.
fn take_host_lock(ctx: &mut Context<'_>) -> Result<HostLock, Exit> {
    match ctx.system.acquire_host_lock() {
        Ok(lock) => Ok(lock),
        Err(error @ togi_hostlock::Error::Busy { .. }) => {
            Err(exit_with(ctx, COMMAND, error, Exit::Locked))
        }
        Err(error) => Err(exit_with(
            ctx,
            COMMAND,
            togi_render::error_chain(&error),
            Exit::Error,
        )),
    }
}

/// Why the journal of `state_dir`, written by `recorded`, may not be appended to by `build`: a newer schema,
/// ruleset or evidence epoch, or a kind of event this build does not know in a journal it otherwise reads.
fn refusal(build: &Build, recorded: Option<&Build>, ctx: &Context<'_>) -> Option<Refused> {
    let recorded = recorded?;
    match build.verdict(recorded, Operation::Resume) {
        Verdict::Refuse(refusal) => Some(Refused {
            message: refusal_message(&refusal, recorded, build),
            leave_reason: INCOMPATIBLE,
        }),
        Verdict::Use => ctx
            .system
            .recording(&ctx.state_dir.join(JOURNAL_FILE))
            .err()
            .and_then(|error| unknown_event_message(&error, recorded, build))
            .map(|message| Refused {
                message,
                leave_reason: "unknown event kind",
            }),
        Verdict::Archive => None,
    }
}

/// A journal this build may not append to: prints why, and in the tuning boot persists a leave reason and
/// clears the saved boot entry, without appending anything. Called with the host lock held whenever there is a
/// boot entry to touch, so the entry is never touched without it. Never reboots.
fn refuse(
    ctx: &mut Context<'_>,
    refused: &Refused,
    bootloader: Option<&mut (dyn Bootloader + '_)>,
    renderer: Renderer,
) -> Exit {
    let line = format!("togi {COMMAND}: {}", escape_text(&refused.message));
    let _ = writeln!(ctx.err, "{}", renderer.text(Style::RedBold, &line));
    let Some(bootloader) = bootloader else {
        return Exit::Incompatible;
    };
    let reason = LeaveReason::new(refused.leave_reason, RestartCount::ZERO)
        .and_then(|reason| togi_tuningboot::write_reason(bootloader, &reason));
    if let Err(error) = reason {
        let _ = writeln!(
            ctx.err,
            "togi: persist boot leave reason: {}",
            escape_text(&togi_render::error_chain(&error))
        );
    }
    match bootloader.clear_saved_entry() {
        Ok(entry) => {
            let _ = writeln!(
                ctx.err,
                "togi: cleared GRUB saved entry from {:?} to {:?}; the next boot selects the normal system",
                entry.before, entry.after
            );
        }
        Err(error) => {
            let _ = writeln!(
                ctx.err,
                "togi: restore boot selection: {}; no reboot requested",
                escape_text(&togi_render::error_chain(&error))
            );
        }
    }
    Exit::Incompatible
}

const COMMAND: &str = "run";
const JOURNAL_FILE: &str = "events.jsonl";
const INCOMPATIBLE: &str = "journal incompatible";

/// Why a journal may not be appended to, and the leave reason the tuning boot persists for it.
struct Refused {
    message: String,
    leave_reason: &'static str,
}

#[cfg(test)]
mod tests;
