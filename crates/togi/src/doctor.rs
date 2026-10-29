//! `togi doctor`: whether this machine is ready for `togi run`, without starting a session.

use std::fmt::Write as _;
use std::sync::LazyLock;

use clap::Args as ClapArgs;
use togi_hardware::Diagnosis;
use togi_hostlock::Error as LockError;
use togi_journal::{Event, Operation, Payload, Recording, Verdict};
use togi_machine::{BiosContext, BootId, Check, CoreInfo};
use togi_render::{Renderer, Style, escape_text};

use crate::args::ConfigArgs;
use crate::cli::{Context, examples};
use crate::exit::Exit;
use crate::report::{current_build, emit, exit_with, fail, refusal_message, unknown_event_message};

static EXAMPLES: LazyLock<String> = LazyLock::new(|| {
    examples(&[
        ("togi doctor", "Make the checks that need no root"),
        ("sudo togi doctor", "Make every check run makes"),
        (
            "sudo togi doctor --day",
            "Check daily-observation prerequisites",
        ),
    ])
});

/// Check whether this machine is ready for a run
///
/// Make the preflight checks of run, check the hardware watchdog and, when a session is recorded,
/// whether the BIOS context still matches it, without starting a session. Writes no journal
/// event, changes no offset and leaves GRUB alone. As root it makes every check and takes the host
/// lock, so a running run or reset stops it. Without root it checks the CPU, the `ryzen_smu` driver
/// identity, the backends, the backend user and the watchdog, and marks the PM table, readback,
/// slot mapping, systemd-run and BIOS context checks skipped. With or without root it also checks
/// whether the tuner supports the topology, reporting a failed topology row when it does not.
///
/// Prints the check rows with ok, FAIL, warn or skipped, then ready, not fully checked, or not
/// ready with the failed checks: those run would refuse at startup. A missing
/// hardware watchdog is a warning, as in a run started outside the tuning boot. A changed BIOS
/// context is a warning too: run records the new context and the session continues under it. A journal with a
/// newer schema, ruleset or evidence epoch, or with unknown event kinds, is refused as run refuses
/// it. On a platform other than
/// Linux it reports that hardware runs need Linux and exits 1 before reading anything.
///
/// With --day, check CPU and `ryzen_smu` identity, current offset readback, PID 1's armed hardware
/// watchdog (including the shared counting check where supported), this boot's entries in the
/// persistent system journal, and the installed GRUB or systemd-boot setup. Also require a
/// recorded, complete CO-0 baseline or restoration receipt citing this boot's zero readbacks.
/// Applied D, a day boot's observations, and a current zero readback are not historical baseline
/// proof. These read-only diagnostics neither apply a profile nor give a stability verdict.
#[derive(ClapArgs, Debug)]
#[command(after_help = EXAMPLES.as_str())]
pub struct Args {
    #[command(flatten)]
    pub config: ConfigArgs,

    /// Check day-observation prerequisites instead of night-run preflight
    #[arg(long)]
    pub day: bool,
}

const COMMAND: &str = "doctor";
/// Checks whose failure is reported as `warn` and never makes the machine not ready: backend availability excludes
/// only the backend from a night.
const WARNINGS: [&str; 3] = ["watchdog", "bios_context", "backends"];

/// Makes the checks and prints the table.
pub fn run(args: &Args, ctx: &mut Context<'_>) -> Exit {
    let system = ctx.system;
    if let Err(error) = system.check_platform() {
        return fail(ctx, COMMAND, togi_render::error_chain(&error));
    }
    let renderer = Renderer::new(system.stderr_facts());
    let (recorded, recording) = match recorded_session(ctx, renderer) {
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
    let lock = if privileged {
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
            Err(error) => return fail(ctx, COMMAND, togi_render::error_chain(&error)),
        }
    } else {
        None
    };
    let diagnosis = if args.day {
        system.diagnose_day(&config, lock.as_ref())
    } else {
        system.diagnose(&config, recorded.as_ref(), lock.as_ref())
    };
    let mut diagnosis = match diagnosis {
        Ok(diagnosis) => diagnosis,
        Err(error) => return fail(ctx, COMMAND, togi_render::error_chain(&error)),
    };
    let topology = match system.topology() {
        Ok(topology) => topology,
        Err(error) => return fail(ctx, COMMAND, togi_render::error_chain(&error)),
    };
    if let Err(error) = togi_tuner::Topology::new(&topology) {
        diagnosis.ran.push(togi_machine::Check {
            name: "topology".into(),
            detail: format!("this machine's topology is not supported: {error}"),
            ok: false,
        });
    }
    if args.day {
        let boot = match system.boot_id() {
            Ok(boot) => boot,
            Err(error) => return fail(ctx, COMMAND, togi_render::error_chain(&error)),
        };
        diagnosis.ran.push(bootloader_check(system));
        diagnosis.ran.push(baseline_check(
            recording.as_ref().map_or(&[], Recording::events),
            &boot,
            &topology,
        ));
    }
    let (table, failed) = table(&diagnosis, args.day);
    if let Err(exit) = ctx.out.write_all(table.as_bytes()).map_err(|error| {
        ctx.error(format!(
            "write output: {}",
            togi_render::error_chain(&error)
        ))
    }) {
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
    let verdict = match (privileged, args.day) {
        (true, false) => "ready",
        (true, true) => "day checks complete; no stability verdict",
        (false, false) => "not fully checked: run sudo togi doctor",
        (false, true) => "not fully checked: run sudo togi doctor --day",
    };
    match emit(ctx, verdict) {
        Ok(()) => Exit::Ok,
        Err(exit) => exit,
    }
}

/// The BIOS context of the session `run` would continue (its latest `session.context`), if one is recorded; `Err` is the exit of
/// a journal `run` would refuse or cannot read.
fn recorded_session(
    ctx: &mut Context<'_>,
    renderer: Renderer,
) -> Result<(Option<BiosContext>, Option<Recording>), Exit> {
    let current = current_build();
    let recorded = match ctx.system.recorded_build(ctx.state_dir) {
        Ok(Some(recorded)) => recorded,
        Ok(None) => return Ok((None, None)),
        Err(error) => return Err(fail(ctx, COMMAND, togi_render::error_chain(&error))),
    };
    match current.verdict(&recorded, Operation::Resume) {
        Verdict::Archive => return Ok((None, None)),
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
    let context = recording.events().iter().rev().find_map(|event| {
        if let Payload::SessionContext(context) = &event.payload {
            Some(context.bios_context())
        } else {
            None
        }
    });
    Ok((context, Some(recording)))
}

fn bootloader_check(system: &dyn crate::system::System) -> Check {
    use togi_tuningboot::night::{SETUP_PATH, Setup};

    let setup = system.read_config(std::path::Path::new(SETUP_PATH));
    let (ok, detail) = match setup {
        Ok(text) => match text.parse::<Setup>() {
            Ok(Setup::Grub { .. }) => (true, "installed setup selects GRUB".to_owned()),
            Ok(Setup::SystemdBoot { .. }) => {
                (true, "installed setup selects systemd-boot".to_owned())
            }
            Err(error) => (
                false,
                format!("decode installed boot-loader setup: {error}"),
            ),
        },
        Err(error) => (
            false,
            format!(
                "read installed boot-loader setup: {}",
                togi_render::error_chain(&error)
            ),
        ),
    };
    Check {
        name: "bootloader".into(),
        detail,
        ok,
    }
}

/// A historical receipt needs its complete, same-boot readback chain, not today's sampled offsets.
fn baseline_check(events: &[Event], boot: &BootId, topology: &[CoreInfo]) -> Check {
    let receipt = events.iter().rev().find(|event| {
        if &event.boot != boot || topology.is_empty() {
            return false;
        }
        let complete_zero = if let Payload::SessionBaseline(baseline) = &event.payload {
            baseline.offsets.len() == topology.len()
                && topology.iter().all(|info| {
                    baseline
                        .offsets
                        .get(info.core)
                        .is_some_and(|offset| offset.0 == 0)
                })
        } else if let Payload::ProfileRestored(restored) = &event.payload {
            restored.offsets.len() == topology.len()
                && topology.iter().all(|info| {
                    restored
                        .offsets
                        .get(info.core)
                        .is_some_and(|offset| offset.get() == 0)
                })
        } else {
            return false;
        };
        complete_zero
            && topology.iter().all(|info| {
                event.cause.iter().any(|seq| {
                    let Ok(index) = events.binary_search_by_key(seq, |reading| reading.seq) else {
                        return false;
                    };
                    let readback = &events[index];
                    readback.seq < event.seq
                        && readback.boot == *boot
                        && matches!(&readback.payload, Payload::SmuReadback(reading)
                        if reading.core == info.core && reading.offset.0 == 0)
                })
            })
    });
    let (ok, detail) = match receipt {
        Some(receipt) => (
            true,
            format!(
                "this boot's verified CO-0 receipt at seq {}; complete cited zero readbacks, distinct from applied D and current readback",
                receipt.seq
            ),
        ),
        None => (
            false,
            "no complete, cited CO-0 baseline/restoration receipt for this boot; applied D and current zero readback are not proof".into(),
        ),
    };
    Check {
        name: "co0_baseline".into(),
        detail,
        ok,
    }
}

fn refuse(ctx: &mut Context<'_>, renderer: Renderer, message: &str) -> Exit {
    let line = format!("togi {COMMAND}: {}", escape_text(message));
    let _ = writeln!(ctx.err, "{}", renderer.text(Style::RedBold, &line));
    Exit::Incompatible
}

/// The check table and the names of the failed checks.
fn table(diagnosis: &Diagnosis, day: bool) -> (String, Vec<&str>) {
    let mut failed = Vec::new();
    let mut rows: Vec<(&str, &str, String)> = Vec::new();
    for check in &diagnosis.ran {
        let result = if check.ok {
            "ok"
        } else if WARNINGS.contains(&check.name.as_str()) && !(day && check.name == "watchdog") {
            "warn"
        } else {
            failed.push(check.name.as_str());
            "FAIL"
        };
        rows.push((
            check.name.as_str(),
            result,
            escape_text(&check.detail).to_string(),
        ));
    }
    for skipped in &diagnosis.skipped {
        rows.push((
            skipped.name.as_str(),
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
