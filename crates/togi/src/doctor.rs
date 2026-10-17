//! `togi doctor`: whether this machine is ready for `togi run`, without starting a session.

use std::fmt::Write as _;
use std::sync::LazyLock;

use clap::Args as ClapArgs;
use togi_hardware::Diagnosis;
use togi_hostlock::Error as LockError;
use togi_journal::{Operation, Verdict};
use togi_machine::BiosContext;
use togi_render::{Renderer, Style, escape_text};

use crate::args::ConfigArgs;
use crate::cli::{Context, examples};
use crate::exit::Exit;
use crate::report::{current_build, emit, exit_with, fail, refusal_message, unknown_event_message};

static EXAMPLES: LazyLock<String> = LazyLock::new(|| {
    examples(&[
        ("togi doctor", "Make the checks that need no root"),
        ("sudo togi doctor", "Make every check run makes"),
    ])
});

/// Check whether this machine is ready for a run
///
/// Make the preflight checks of run, check the hardware watchdog and, when a session is recorded,
/// whether the BIOS context still matches it, without starting a session. Writes no journal
/// event, changes no offset and leaves GRUB alone. As root it makes every check and takes the host
/// lock, so a running run or reset stops it. Without root it checks the CPU, the `ryzen_smu` driver
/// identity, the backends, the backend user and the watchdog, and marks the PM table, readback,
/// slot mapping, systemd-run and BIOS context checks skipped.
///
/// Prints one row per check, with ok, FAIL, warn or skipped, then ready, not fully checked, or not
/// ready with the failed checks: those run would turn into a preflight dead end. A missing
/// hardware watchdog is a warning, as in a run started outside the tuning boot. A changed BIOS
/// context is a warning too: run archives the session and starts a new one. A journal with a
/// newer schema, ruleset or evidence epoch, or with unknown event kinds, is refused as run refuses
/// it. On a platform other than
/// Linux it reports that hardware runs need Linux and exits 1 before reading anything.
#[derive(ClapArgs, Debug)]
#[command(after_help = EXAMPLES.as_str())]
pub struct Args {
    #[command(flatten)]
    pub config: ConfigArgs,
}

const COMMAND: &str = "doctor";
const WARNINGS: [&str; 2] = ["watchdog", "bios_context"];

/// Makes the checks and prints the table.
pub fn run(args: &Args, ctx: &mut Context<'_>) -> Exit {
    let system = ctx.system;
    if let Err(error) = system.check_platform() {
        return fail(ctx, COMMAND, error);
    }
    let renderer = Renderer::new(system.stderr_facts());
    let recorded = match recorded_context(ctx, renderer) {
        Ok(recorded) => recorded,
        Err(exit) => return exit,
    };
    let config = match args
        .config
        .load_with_source_read(system.config_path(), |path| system.read_config(path))
        .map(|(config, _)| config)
    {
        Ok(config) => config,
        Err(error) => {
            return exit_with(ctx, COMMAND, togi_render::error_chain(&error), Exit::Usage);
        }
    };
    let privileged = system.privileged();
    let _lock = if privileged {
        let (path, _) = system.host_lock();
        match system.acquire_host_lock() {
            Ok(lock) => Some(lock),
            Err(LockError::Busy { .. }) => {
                let message = format!(
                    "acquire host lock {}: another togi process holds the host lock",
                    path.display()
                );
                return exit_with(ctx, COMMAND, message, Exit::Locked);
            }
            Err(error) => return fail(ctx, COMMAND, error),
        }
    } else {
        None
    };
    let diagnosis = match system.diagnose(&config, recorded.as_ref(), privileged) {
        Ok(diagnosis) => diagnosis,
        Err(error) => return fail(ctx, COMMAND, togi_render::error_chain(&error)),
    };
    let (table, failed) = table(&diagnosis);
    if let Err(exit) = ctx
        .out
        .write_all(table.as_bytes())
        .map_err(|error| ctx.error(format!("write output: {error}")))
    {
        return exit;
    }
    if let Some(error) = &diagnosis.bios_context_error {
        return fail(
            ctx,
            COMMAND,
            format!("read BIOS context: {}", togi_render::error_chain(error)),
        );
    }
    if !failed.is_empty() {
        let line = format!("not ready: {}", failed.join(", "));
        if let Err(exit) = emit(ctx, &line) {
            return exit;
        }
        let _ = writeln!(
            ctx.err,
            "{}",
            renderer.text(Style::RedBold, &format!("togi {COMMAND}: {line}"))
        );
        return Exit::Preflight;
    }
    let verdict = if privileged {
        "ready"
    } else {
        "not fully checked: run sudo togi doctor"
    };
    match emit(ctx, verdict) {
        Ok(()) => Exit::Ok,
        Err(exit) => exit,
    }
}

/// The BIOS context of the session `run` would continue, if one is recorded; `Err` is the exit of
/// a journal `run` would refuse or cannot read.
fn recorded_context(
    ctx: &mut Context<'_>,
    renderer: Renderer,
) -> Result<Option<BiosContext>, Exit> {
    let current = current_build();
    let recorded = match ctx.system.recorded_build(ctx.state_dir) {
        Ok(Some(recorded)) => recorded,
        Ok(None) => return Ok(None),
        Err(error) => return Err(fail(ctx, COMMAND, togi_render::error_chain(&error))),
    };
    match current.verdict(&recorded, Operation::Resume) {
        Verdict::Archive => return Ok(None),
        Verdict::Refuse(refusal) => {
            let message = refusal_message(&refusal, &recorded, &current);
            return Err(refuse(ctx, renderer, &message));
        }
        Verdict::Use => {}
    }
    let path = ctx.state_dir.join("events.jsonl");
    let recording = ctx.system.recording(&path).map_err(|error| {
        match unknown_event_message(&error, &recorded, &current) {
            Some(message) => refuse(ctx, renderer, &message),
            None => fail(ctx, COMMAND, togi_render::error_chain(&error)),
        }
    })?;
    Ok(recording
        .events()
        .iter()
        .find_map(|event| match &event.payload {
            togi_journal::Payload::SessionContext(context) => Some(context.bios_context()),
            _ => None,
        }))
}

fn refuse(ctx: &mut Context<'_>, renderer: Renderer, message: &str) -> Exit {
    let line = format!("togi {COMMAND}: {}", escape_text(message));
    let _ = writeln!(ctx.err, "{}", renderer.text(Style::RedBold, &line));
    Exit::Incompatible
}

/// The check table and the names of the failed checks.
fn table(diagnosis: &Diagnosis) -> (String, Vec<String>) {
    let mut failed = Vec::new();
    let mut rows: Vec<(String, &str, String)> = Vec::new();
    for check in &diagnosis.ran {
        let result = if check.ok {
            "ok"
        } else if WARNINGS.contains(&check.name.as_str()) {
            "warn"
        } else {
            failed.push(check.name.clone());
            "FAIL"
        };
        rows.push((
            check.name.clone(),
            result,
            escape_text(&check.detail).to_string(),
        ));
    }
    for skipped in &diagnosis.skipped {
        rows.push((
            skipped.name.to_string(),
            "skipped",
            escape_text(&skipped.reason.to_string()).to_string(),
        ));
    }
    let name_width = rows
        .iter()
        .map(|row| row.0.len())
        .chain(["CHECK".len()])
        .max()
        .unwrap_or_default();
    let result_width = rows
        .iter()
        .map(|row| row.1.len())
        .chain(["RESULT".len()])
        .max()
        .unwrap_or_default();
    let mut text = String::new();
    let _ = writeln!(
        text,
        "{:<name_width$}  {:<result_width$}  DETAIL",
        "CHECK", "RESULT"
    );
    for (name, result, detail) in &rows {
        let _ = writeln!(
            text,
            "{name:<name_width$}  {result:<result_width$}  {detail}"
        );
    }
    (text, failed)
}
