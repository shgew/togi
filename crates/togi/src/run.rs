//! `togi run`: start or resume the tuning session in the foreground.
//!
//! The order of a run is fixed by `docs/spec/runtime.md`. The platform is checked first, then the journal's
//! compatibility (a refusal appends nothing), then the configuration. The process's own signal listener
//! starts, the host lock is taken, and only then the boot id read, the hardware opened, the carry prepared
//! and the session run. Once the host lock is held and the hardware set up, the session owns the cleanup of
//! every exit; a panic in it comes back with that cleanup done, and is resumed after the dashboard, the
//! journal and the host lock are put away, in that order.

use std::fmt::{self, Display};
use std::io::{self, Write};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::LazyLock;
use std::sync::{Arc, Mutex, PoisonError};

use clap::{Args as ClapArgs, ValueHint};
use jiff::tz::TimeZone;
use togi_config::{Config, LoadError};
use togi_defect::KNOWN_DEFECTS;
use togi_hostlock::HostLock;
use togi_journal::{
    Build, FileError, Operation, ReadError, Recording, Refusal, StateDir, UnknownKind, Verdict,
};
use togi_machine::CancellationToken;
use togi_render::{Renderer, Style};
use togi_session::{
    ConfigSource, PrepareError, Prompt, PublicationPolicy, RunInput, StopPolicy, prepare,
};
use togi_tuningboot::{Bootloader, LeaveReason, RestartCount};

use crate::args::ConfigArgs;
use crate::cli::{Context, examples};
use crate::dashboard::Dashboard;
use crate::exit::Exit;
use crate::{build, signals};

mod outcome;
mod prompt;

static EXAMPLES: LazyLock<String> = LazyLock::new(|| {
    examples(&[
        (
            "sudo togi run",
            "Tune this machine until a signal or a dead end",
        ),
        (
            "sudo togi run --cycles 1",
            "Stop once phase 2's confirmation cycle has passed",
        ),
        (
            "sudo togi run --first-result",
            "Stop at the first confirmed BIOS profile, after phase 1's passed cycle",
        ),
        (
            "sudo togi run --no-tui",
            "Print one line per event instead of the dashboard",
        ),
    ])
});

/// Start or resume the tuning session in the foreground
///
/// Searches for each core's solo limit, hunts the core or combination behind unattributed failures with
/// parked trials, checks the profile from one count shallower than each solo limit (phase 1), moves cores
/// back toward their solo limits one count per round and confirms the result with a full cycle (phase 2),
/// then keeps checking it with full cycles. After a crash the next run attributes it from the journal and
/// continues. On resume, known defects affecting past decisions name the cores; a run at a terminal offers to
/// reset them. An unanswered too-aggressive defect stops an unattended run. It needs root.
///
/// A journal from an older ruleset, schema or evidence epoch, and no newer one, is archived. A newer ruleset,
/// schema or evidence epoch stops the run before another event is written; reset --all archives that
/// session. Journal lines are colored on terminals and in the system journal unless the environment asks for no colour.
///
/// Only one run or reset can own this machine, even with different state directories. A busy
/// /run/lock/togi.lock stops the command before any hardware access or event.
///
/// When stdin and stderr are terminals, run shows the session as the watch dashboard instead of one line per
/// event, and prints the outcome when it stops: the restored offsets and why it stopped, or the dead end or
/// error; events.jsonl still records every event. --no-tui prints the lines instead.
///
/// --tuning-boot runs the unattended service with an armed hardware watchdog. The first durable journal
/// append resets its consecutive restart-limit count and records a pending leave reason once. Dead ends and
/// incompatible journals persist a short leave reason before clearing the saved GRUB entry.
#[derive(ClapArgs, Debug)]
#[command(after_help = EXAMPLES.as_str())]
pub struct Args {
    #[command(flatten)]
    pub config: ConfigArgs,

    /// Stop after N clean cycles, each a passed full cycle ending at or after phase 2's confirmation cycle
    /// (default endless)
    #[arg(long, value_name = "N", value_parser = positive, conflicts_with = "first_result")]
    pub cycles: Option<NonZeroU32>,

    /// Stop once the BIOS profile is first confirmed, after phase 1's passed cycle and before phase 2 moves
    /// a core
    #[arg(long)]
    pub first_result: bool,

    /// Run as the tuning boot service: persist a leave reason before clearing the saved boot entry in this GRUB
    /// environment file, and reboot after a boot loop
    #[arg(long, value_name = "file", value_hint = ValueHint::FilePath)]
    pub tuning_boot: Option<PathBuf>,

    /// Print one line per event instead of the dashboard on a terminal
    #[arg(long)]
    pub no_tui: bool,
}

fn positive(text: &str) -> Result<NonZeroU32, String> {
    text.parse::<NonZeroU32>()
        .map_err(|_| "must be a positive integer".to_owned())
}

/// Runs the command.
pub fn run(args: &Args, ctx: &mut Context<'_>) -> Exit {
    if let Err(error) = ctx.system.check_platform() {
        return failure(ctx, Exit::Error, error);
    }
    let renderer = ctx.system.renderer();
    let build = build::current();
    let mut bootloader = args
        .tuning_boot
        .as_deref()
        .map(|grubenv| ctx.system.bootloader(grubenv));

    let dir = match StateDir::lock(ctx.state_dir) {
        Ok(dir) => dir,
        Err(error @ togi_journal::LockError::Held { .. }) => {
            return failure(ctx, Exit::Locked, error);
        }
        Err(error) => return failure(ctx, Exit::Error, error),
    };
    match dir.live_build() {
        Ok(Some(recorded)) => match build.verdict(&recorded, Operation::Resume) {
            Verdict::Refuse(refusal) => {
                return refuse(
                    ctx,
                    Refused::Stamp(&refusal),
                    bootloader.as_deref_mut(),
                    renderer,
                    None,
                );
            }
            Verdict::Use => {
                if let Some(unknown) = unknown_kind(&ctx.state_dir.join(JOURNAL_FILE)) {
                    let refused = Refused::Kind {
                        unknown: &unknown,
                        written_by: &recorded,
                        this: &build,
                    };
                    return refuse(ctx, refused, bootloader.as_deref_mut(), renderer, None);
                }
            }
            Verdict::Archive => {}
        },
        Ok(None) => {}
        Err(error) => return failure(ctx, Exit::Error, error),
    }

    let (config, file) = match Config::load(args.config.path()) {
        Ok(config) => (config, true),
        Err(LoadError::Read { source, .. })
            if source.kind() == io::ErrorKind::NotFound && !args.config.is_explicit() =>
        {
            (Config::default(), false)
        }
        Err(error) => return failure(ctx, Exit::Usage, error),
    };

    let cancel = CancellationToken::new();
    let _listening = match signals::listen(&cancel) {
        Ok(listening) => listening,
        Err(error) => return failure(ctx, Exit::Error, format!("listen for signals: {error}")),
    };

    let (lock_path, policy) = ctx.system.host_lock();
    let lock = match HostLock::acquire(lock_path, policy) {
        Ok(lock) => lock,
        Err(error @ togi_hostlock::Error::Busy { .. }) => {
            return failure(ctx, Exit::Locked, error);
        }
        Err(error) => return failure(ctx, Exit::Error, error),
    };
    let boot = match ctx.system.boot_id() {
        Ok(boot) => boot,
        Err(error) => return failure(ctx, Exit::Error, error),
    };
    let mut machine = match ctx.system.open_machine(&config, ctx.state_dir, &lock) {
        Ok(machine) => machine,
        Err(error) => return failure(ctx, Exit::Error, error),
    };
    let bios = machine
        .host
        .validate_smu()
        .ok()
        .and_then(|_| machine.host.bios_context().ok());
    let prepared = match prepare(
        dir,
        &build,
        boot.clone(),
        ctx.system.now(),
        bios.as_ref(),
        KNOWN_DEFECTS,
    ) {
        Ok(prepared) => prepared,
        Err(PrepareError::Refused(refusal)) => {
            return refuse(
                ctx,
                Refused::Stamp(&refusal),
                bootloader.as_deref_mut(),
                renderer,
                Some(&lock),
            );
        }
        Err(error) => return failure(ctx, Exit::Error, error),
    };
    let journal = prepared.journal;

    let interactive = ctx.system.interactive();
    let dashboard = (interactive && !args.no_tui).then(|| {
        let since = journal.events().last().map_or(0, |event| event.seq.get());
        Rc::new(Dashboard::new(ctx.system.surface(), ctx.state_dir, since))
    });
    let hidden = Hidden::default();
    let (log, report): (Box<dyn Write>, Box<dyn Write>) = match &dashboard {
        Some(dashboard) => (
            dashboard.gate(ctx.system.stderr()),
            Box::new(hidden.clone()),
        ),
        None => (ctx.system.stderr(), ctx.system.stderr()),
    };
    let prompt: Option<Box<dyn Prompt>> = interactive.then(|| {
        Box::new(prompt::Terminal::new(cancel.clone(), dashboard.clone())) as Box<dyn Prompt>
    });
    let zone = TimeZone::system();
    let input = RunInput {
        config,
        source: ConfigSource {
            path: args.config.path().display().to_string(),
            file,
        },
        build,
        boot,
        journal,
        machine,
        policy: StopPolicy {
            cycles: args.cycles,
            first_result: args.first_result,
        },
        publication: PublicationPolicy::default(),
        bootloader,
        prompt,
        defects: KNOWN_DEFECTS,
        resume: None,
        carry: prepared.carry,
        log: Some(log),
        on_append: None,
        renderer,
        zone: zone.clone(),
        cancel,
        report,
    };
    if let Some(dashboard) = &dashboard {
        dashboard.show();
    }
    let mut ended = togi_session::run(input);
    if let Some(dashboard) = &dashboard {
        dashboard.hide();
        let _ = ctx.err.write_all(&hidden.take());
    }
    if let Some(panic) = ended.panic.take() {
        let _ = ended.journal.close();
        drop(lock);
        panic.resume();
    }
    let clean =
        ended.errors.is_empty() && !matches!(ended.stop, Some(togi_session::Stop::DeadEnd { .. }));
    if dashboard.is_some() && clean {
        outcome::print_clean_stop(ctx.err, renderer, &zone, ended.journal.events());
    }
    let result = outcome::conclude(
        ctx.err,
        renderer,
        &zone,
        ended.stop.as_ref(),
        &ended.errors,
        ended.journal.events(),
    );
    let mut exit = result.exit;
    if let Err(error) = ended.journal.close() {
        let _ = writeln!(ctx.err, "togi run: close the journal: {error}");
        if exit == Exit::Ok {
            exit = Exit::Error;
        }
    }
    drop(lock);
    if result.reboot {
        let _ = writeln!(ctx.err, "togi: rebooting into the normal system");
        if let Err(error) = ctx.system.reboot() {
            let _ = writeln!(ctx.err, "togi: systemctl reboot: {error}");
        }
    }
    exit
}

fn failure(ctx: &mut Context<'_>, exit: Exit, message: impl Display) -> Exit {
    let _ = writeln!(ctx.err, "togi run: {message}");
    exit
}

/// A journal this build may not append to: prints why, and in the tuning boot persists a leave reason and
/// clears the saved boot entry, under the host lock, without appending anything. Never reboots.
fn refuse(
    ctx: &mut Context<'_>,
    refused: Refused<'_>,
    bootloader: Option<&mut (dyn Bootloader + '_)>,
    renderer: Renderer,
    held: Option<&HostLock>,
) -> Exit {
    let line = format!("togi run: {refused}");
    let _ = writeln!(ctx.err, "{}", renderer.text(Style::RedBold, &line));
    let Some(bootloader) = bootloader else {
        return Exit::Incompatible;
    };
    let _lock;
    if held.is_none() {
        let (path, policy) = ctx.system.host_lock();
        _lock = match HostLock::acquire(path, policy) {
            Ok(lock) => lock,
            Err(error @ togi_hostlock::Error::Busy { .. }) => {
                return failure(ctx, Exit::Locked, error);
            }
            Err(error) => return failure(ctx, Exit::Error, error),
        };
    }
    let reason = LeaveReason::new(refused.leave_reason(), RestartCount::ZERO)
        .and_then(|reason| togi_tuningboot::write_reason(bootloader, &reason));
    if let Err(error) = reason {
        let _ = writeln!(ctx.err, "togi: persist GRUB leave reason: {error}");
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
                "togi: clear GRUB saved entry: {error}; no reboot requested"
            );
        }
    }
    Exit::Incompatible
}

const JOURNAL_FILE: &str = "events.jsonl";

/// Why a journal may not be appended to.
#[derive(Clone, Copy)]
enum Refused<'a> {
    /// Its build stamp is newer than this build's.
    Stamp(&'a Refusal),
    /// A current journal holds an event kind this build does not know.
    Kind {
        unknown: &'a UnknownKind,
        written_by: &'a Build,
        this: &'a Build,
    },
}

impl Refused<'_> {
    const fn leave_reason(&self) -> &'static str {
        match self {
            Self::Stamp(_) => "journal incompatible",
            Self::Kind { .. } => "unknown event kind",
        }
    }
}

impl Display for Refused<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stamp(refusal) => write!(f, "{refusal}"),
            Self::Kind {
                unknown,
                written_by,
                this,
            } => write!(
                f,
                "the journal holds an event this build cannot read ({unknown}); it was written by {written_by} and this is {this}; install the build that wrote it"
            ),
        }
    }
}

/// The unknown kind a readable current journal holds, if it holds one.
fn unknown_kind(path: &Path) -> Option<UnknownKind> {
    match Recording::read(path) {
        Err(FileError {
            source: ReadError::UnknownKind { source, .. },
            ..
        }) => Some(source),
        _ => None,
    }
}

/// What the session wrote to its report while the dashboard was up, kept for after it.
#[derive(Clone, Default)]
struct Hidden(Arc<Mutex<Vec<u8>>>);

impl Hidden {
    fn take(&self) -> Vec<u8> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl Write for Hidden {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
