//! `togi reset`: restart one core's search, or archive the whole session.

use std::fs;
use std::io::{ErrorKind, Read as _};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use clap::{Args as ClapArgs, ValueHint};
use togi_config::{Config, InitialOverride};
use togi_hostlock::Error as HostLockError;
use togi_journal::{
    ArchiveKind, Build, DirError, Event, Journal, LockError, Marker, OpenError, Operation,
    SessionId, StateDir, Verdict,
};
use togi_machine::{BootId, CoreId};
use togi_render::escape_text;
use togi_session::{CommandError, reset_all, reset_core};

use crate::args::ConfigArgs;
use crate::cli::{Context, examples};
use crate::exit::Exit;
use crate::report::{
    current_build, emit, exit_with, fail, log_events, refusal_message, unknown_event_message,
};
use crate::system::{System, SystemClock};

static EXAMPLES: LazyLock<String> = LazyLock::new(|| {
    examples(&[
        (
            "sudo togi reset --core 3",
            "Search core 3 again from its baseline",
        ),
        (
            "sudo togi reset --all",
            "Archive the session and start over",
        ),
    ])
});

/// Reset one core, or archive the whole session
///
/// Reset one core, so the next run restarts its search from the baseline, or archive the whole
/// session, so the next run starts a new one that carries nothing from it. Give exactly one of the
/// two. --core clears its failure point and every combination that includes it. --all warns if a
/// configured candidate solo limit reached a failure point in the archived session; missing or
/// invalid configuration does not prevent archiving.
///
/// --core refuses a different journal ruleset, schema or evidence epoch; --all archives any of
/// them. --core, and --all on a journal of the same schema, refuse an unknown event kind or field;
/// install the build that wrote them. Only one run or reset can own this machine, even with
/// different state directories. A busy host lock stops reset before it changes the session.
#[derive(ClapArgs, Debug)]
#[command(after_help = EXAMPLES.as_str())]
pub struct Args {
    #[command(flatten)]
    pub target: Target,

    #[command(flatten)]
    pub config: ConfigArgs,
}

/// What to reset: exactly one of the two.
#[derive(ClapArgs, Debug)]
#[group(required = true, multiple = false)]
pub struct Target {
    /// Reset core N: clear its failure point and every combination that includes it, and restart
    /// its search from the baseline
    #[arg(long, value_name = "N", value_hint = ValueHint::Other)]
    pub core: Option<u8>,

    /// Archive the session; the next run starts a new one
    #[arg(long)]
    pub all: bool,
}

const COMMAND: &str = "reset";
const JOURNAL: &str = "events.jsonl";

/// Resets under the host lock and the state directory's writer lock.
pub fn run(args: &Args, ctx: &mut Context<'_>) -> Exit {
    let system = ctx.system;
    let all = args.target.all;
    let (path, _) = system.host_lock();
    let _host = match system.acquire_host_lock() {
        Ok(lock) => lock,
        Err(HostLockError::Busy { .. }) => {
            let message = format!(
                "acquire host lock {}: another togi process holds the host lock",
                path.display()
            );
            return exit_with(ctx, COMMAND, message, Exit::Locked);
        }
        Err(error) => return fail(ctx, COMMAND, togi_render::error_chain(&error)),
    };
    let journal = ctx.state_dir.join(JOURNAL);
    if matches!(journal_nonempty(system, &journal), Err(error) if error.kind() == ErrorKind::NotFound)
        && !(all && pending_marker(system, ctx.state_dir))
    {
        return fail(ctx, COMMAND, format!("no journal at {}", journal.display()));
    }
    // Read alone, before the journal lock: another build's journal, or a header torn by a crash, is refused without
    // creating the lock file, as the read-only commands refuse it.
    let recorded = system.recorded_build(ctx.state_dir);
    if let Err(error @ DirError::ForeignJournal { .. }) = &recorded {
        return fail(ctx, COMMAND, togi_render::error_chain(error));
    }
    if matches!(recorded, Ok(None)) && !(all && pending_marker(system, ctx.state_dir)) {
        return no_session(ctx);
    }
    let boot = match system.boot_id() {
        Ok(boot) => boot,
        Err(error) => return fail(ctx, COMMAND, togi_render::error_chain(&error)),
    };
    let dir = match system.lock_state_dir(ctx.state_dir) {
        Ok(dir) => dir,
        Err(LockError::Held { .. }) => {
            let message = "another togi process holds the journal lock";
            return exit_with(ctx, COMMAND, message, Exit::Locked);
        }
        Err(error) => return fail(ctx, COMMAND, togi_render::error_chain(&error)),
    };
    match args.target.core {
        Some(core) => reset_one(ctx, dir, boot, core),
        None => reset_everything(ctx, args, dir, boot),
    }
}

fn no_session(ctx: &mut Context<'_>) -> Exit {
    let message = format!("no session in {}", ctx.state_dir.display());
    fail(ctx, COMMAND, message)
}

fn pending_marker(system: &dyn System, state_dir: &Path) -> bool {
    let archive = state_dir.join("archive");
    match system.journal_fs() {
        Some(fs) => fs.read_dir(&archive).is_ok_and(|entries| {
            entries
                .iter()
                .any(|entry| entry.name.to_string_lossy().ends_with("-pending"))
        }),
        None => fs::read_dir(archive).is_ok_and(|entries| {
            entries
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().ends_with("-pending"))
        }),
    }
}

fn journal_nonempty(system: &dyn System, path: &Path) -> std::io::Result<bool> {
    match system.journal_fs() {
        Some(fs) => fs.open_read(path)?.read(&mut [0]).map(|len| len != 0),
        None => fs::metadata(path).map(|metadata| metadata.len() != 0),
    }
}

fn reset_one(ctx: &mut Context<'_>, dir: StateDir, boot: BootId, core: u8) -> Exit {
    let system = ctx.system;
    let build = current_build();
    let recorded = match dir.live_build() {
        Ok(recorded) => recorded,
        Err(error) => return fail(ctx, COMMAND, togi_render::error_chain(&error)),
    };
    if let Some(recorded) = &recorded
        && let Verdict::Refuse(refusal) = build.verdict(recorded, Operation::Append)
    {
        let message = refusal_message(&refusal, recorded, &build);
        return exit_with(ctx, COMMAND, message, Exit::Incompatible);
    }
    let Ok(id) = CoreId::try_from(core) else {
        return no_such_core(ctx, core);
    };
    let mut journal = match dir.open_at(boot, system.now()) {
        Ok(journal) => journal,
        Err(error) => return open_failure(ctx, &error, recorded.as_ref(), &build),
    };
    log_torn(ctx, &journal);
    let opened = journal.events().len();
    let reset = reset_core(&build, &mut journal, &mut SystemClock(system), id);
    log_events(ctx, &journal.events()[opened..]);
    if let Err(error) = reset {
        return match error {
            CommandError::NoSuchCore { .. } => no_such_core(ctx, core),
            error @ (CommandError::NoSession
            | CommandError::Compatibility(_)
            | CommandError::Append(_)
            | CommandError::Fold(_)
            | CommandError::Archive(_)
            | CommandError::Close(_)
            | CommandError::Clock(_)
            | CommandError::Dir(_)) => command_failure(ctx, &error),
        };
    }
    if let Err(error) = journal.close() {
        return fail(ctx, COMMAND, togi_render::error_chain(&error));
    }
    let queued = format!(
        "reset of core {core:02} queued; the next togi run restarts its search from the baseline"
    );
    emit(ctx, queued).map_or_else(|exit| exit, |()| Exit::Ok)
}

fn no_such_core(ctx: &mut Context<'_>, core: u8) -> Exit {
    exit_with(
        ctx,
        COMMAND,
        format!("reset core {core}: no such core"),
        Exit::Usage,
    )
}

fn open_failure(
    ctx: &mut Context<'_>,
    error: &OpenError,
    recorded: Option<&Build>,
    build: &Build,
) -> Exit {
    if let (OpenError::Read(file), Some(recorded)) = (error, recorded)
        && let Some(message) = unknown_event_message(file, recorded, build)
    {
        return exit_with(ctx, COMMAND, message, Exit::Incompatible);
    }
    fail(ctx, COMMAND, togi_render::error_chain(error))
}

fn log_recorded(ctx: &mut Context<'_>, session: Option<&SessionId>, opened: usize) {
    let Some(session) = session else { return };
    let live = ctx.state_dir.join(JOURNAL);
    let path = if journal_nonempty(ctx.system, &live).is_ok() {
        live
    } else {
        ctx.state_dir
            .join("archive")
            .join(format!("{session}.jsonl"))
    };
    if let Ok(recording) = ctx.system.recording(&path)
        && let Some(recorded) = recording.events().get(opened..)
    {
        log_events(ctx, recorded);
    }
}

fn log_torn(ctx: &mut Context<'_>, journal: &Journal) {
    if let Some(event) = journal.repaired().and_then(|seq| journal.event(seq)) {
        log_events(ctx, std::slice::from_ref(event));
    }
}

fn command_failure(ctx: &mut Context<'_>, error: &CommandError) -> Exit {
    match error {
        CommandError::NoSession => no_session(ctx),
        CommandError::NoSuchCore { .. } => exit_with(ctx, COMMAND, error, Exit::Usage),
        CommandError::Compatibility(_) => exit_with(ctx, COMMAND, error, Exit::Incompatible),
        CommandError::Append(_)
        | CommandError::Fold(_)
        | CommandError::Archive(_)
        | CommandError::Close(_)
        | CommandError::Clock(_)
        | CommandError::Dir(_) => fail(ctx, COMMAND, togi_render::error_chain(error)),
    }
}

fn archived_line(session: &SessionId) -> String {
    format!("session {session} archived to archive/{session}.jsonl")
}

fn reset_everything(ctx: &mut Context<'_>, args: &Args, mut dir: StateDir, boot: BootId) -> Exit {
    let system = ctx.system;
    let build = current_build();
    let recorded = match dir.live_build() {
        Ok(recorded) => recorded,
        Err(error) => return fail(ctx, COMMAND, togi_render::error_chain(&error)),
    };
    if let Some(recorded) = &recorded
        && build.verdict(recorded, Operation::ResetAll) == Verdict::Use
        && let Err(error) = system.recording(&ctx.state_dir.join(JOURNAL))
        && let Some(message) = unknown_event_message(&error, recorded, &build)
    {
        return exit_with(ctx, COMMAND, message, Exit::Incompatible);
    }
    let live = dir.live_session().ok().flatten();
    let dropped = match dir.drop_pending_carry() {
        Ok(dropped) => dropped,
        Err(error) => return fail(ctx, COMMAND, togi_render::error_chain(&error)),
    };
    let Some(recorded) = recorded else {
        return finish_without_journal(ctx, dir, boot, dropped);
    };
    let (session, warnings) = match build.verdict(&recorded, Operation::ResetAll) {
        Verdict::Archive => {
            return match dir.archive_unreadable(ArchiveKind::ResetAll) {
                Ok(Some(session)) => emit(
                    ctx,
                    format!(
                        "{} without appending to the incompatible journal; the next togi run starts a new session",
                        archived_line(&session)
                    ),
                )
                .map_or_else(|exit| exit, |()| Exit::Ok),
                Ok(None) => no_session(ctx),
                Err(error) => fail(ctx, COMMAND, togi_render::error_chain(&error)),
            };
        }
        Verdict::Refuse(refusal) => {
            let message = refusal_message(&refusal, &recorded, &build);
            return exit_with(ctx, COMMAND, message, Exit::Incompatible);
        }
        Verdict::Use => {
            let journal = match dir.open_at(boot, system.now()) {
                Ok(journal) => journal,
                Err(error) => return open_failure(ctx, &error, Some(&recorded), &build),
            };
            log_torn(ctx, &journal);
            let opened = journal.events().len();
            let warnings = solo_limit_warnings(journal.events(), &args.config, system);
            match reset_all(&build, journal, &mut SystemClock(system)) {
                Ok((session, _)) => {
                    log_recorded(ctx, Some(&session), opened);
                    (session, warnings)
                }
                Err(error) => {
                    log_recorded(ctx, live.as_ref(), opened);
                    return command_failure(ctx, &error);
                }
            }
        }
    };
    if let Err(exit) = emit(
        ctx,
        format!(
            "{}; the next togi run starts a new session",
            archived_line(&session)
        ),
    ) {
        return exit;
    }
    for warning in warnings {
        let _ = writeln!(ctx.err, "{}", escape_text(&warning));
    }
    Exit::Ok
}

fn finish_without_journal(
    ctx: &mut Context<'_>,
    mut dir: StateDir,
    boot: BootId,
    dropped: Option<SessionId>,
) -> Exit {
    let system = ctx.system;
    let interrupted = match dir.mark_reset_all().and_then(|_| dir.markers()) {
        Ok(markers) => markers
            .into_iter()
            .find_map(|(marker, session)| (marker == Marker::CompatPending).then_some(session)),
        Err(error) => return fail(ctx, COMMAND, togi_render::error_chain(&error)),
    };
    if let Some(session) = interrupted {
        let journal = match dir.open_at(boot, system.now()) {
            Ok(journal) => journal,
            Err(error) => return fail(ctx, COMMAND, togi_render::error_chain(&error)),
        };
        if let Err(error) = journal.close() {
            return fail(ctx, COMMAND, togi_render::error_chain(&error));
        }
        let journal = ctx.state_dir.join(JOURNAL);
        if journal_nonempty(system, &journal).is_ok_and(|nonempty| !nonempty)
            && let Err(error) = remove_durably(system.journal_fs().as_deref(), &journal)
        {
            return fail(ctx, COMMAND, togi_render::error_chain(&error));
        }
        return emit(
            ctx,
            format!(
                "{} without appending to the incompatible journal; the next togi run starts a new session",
                archived_line(&session)
            ),
        )
        .map_or_else(|exit| exit, |()| Exit::Ok);
    }
    match dropped {
        Some(session) => emit(
            ctx,
            format!(
                "carry from session {session} dropped; the next togi run starts a new session with nothing carried"
            ),
        )
        .map_or_else(|exit| exit, |()| Exit::Ok),
        None => no_session(ctx),
    }
}

/// Why a removal did not survive a crash: the step that failed.
#[derive(Debug, thiserror::Error)]
enum RemoveError {
    #[error("remove {}", path.display())]
    Remove {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("sync {}", path.display())]
    Sync {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Removes `path` and syncs its directory, so the removal survives a crash.
fn remove_durably(
    filesystem: Option<&dyn togi_journal::fs::Fs>,
    path: &Path,
) -> Result<(), RemoveError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let removed = match filesystem {
        Some(fs) => fs.remove_file(path),
        None => fs::remove_file(path),
    };
    removed.map_err(|source| RemoveError::Remove {
        path: path.to_owned(),
        source,
    })?;
    let synced = match filesystem {
        Some(fs) => fs.sync_dir(dir),
        None => sync_directory(dir),
    };
    synced.map_err(|source| RemoveError::Sync {
        path: dir.to_owned(),
        source,
    })
}

#[expect(
    clippy::disallowed_methods,
    reason = "production reset removal must survive a crash"
)]
fn sync_directory(path: &Path) -> std::io::Result<()> {
    fs::File::open(path)?.sync_all()
}

/// A warning for each configured candidate solo limit at or deeper than its core's failure point
/// in the session about to be archived.
fn solo_limit_warnings(events: &[Event], config: &ConfigArgs, system: &dyn System) -> Vec<String> {
    let config: Config = match config
        .load_with_source_read(system.config_path(), |path| system.read_config(path))
        .map(|(config, _)| config)
    {
        Ok(config) => config,
        Err(error) if config.is_explicit() => {
            return vec![format!(
                "warning: cannot check candidate solo limits: {}",
                togi_render::error_chain(&error)
            )];
        }
        Err(_) => return Vec::new(),
    };
    let mut limits = config
        .overrides()
        .filter_map(|(core, entry)| match entry {
            InitialOverride::CandidateSoloLimit(offset) => Some((core, offset.get())),
            InitialOverride::Start(_) => None,
        })
        .peekable();
    if limits.peek().is_none() {
        return Vec::new();
    }
    let state = match togi_tuner::replay(events) {
        Ok(state) => state,
        Err(error) => {
            return vec![format!(
                "warning: cannot check candidate solo limits: {}",
                togi_render::error_chain(&error)
            )];
        }
    };
    let Some(constraints) = state.constraints() else {
        return Vec::new();
    };
    limits
        .filter_map(|(core, limit)| {
            let point = constraints.failure_points().get(core)?.offset().get();
            (limit <= point).then(|| {
                let remedy = if point < 0 {
                    format!("use {}, the failure point plus one, or remove it", point + 1)
                } else {
                    "remove it".to_owned()
                };
                format!(
                    "warning: candidate solo limit {limit} for core {core:02} is at or deeper than its failure point {point} in the archived session; {remedy}",
                    core = core.get()
                )
            })
        })
        .collect()
}

#[cfg(test)]
mod tests;
