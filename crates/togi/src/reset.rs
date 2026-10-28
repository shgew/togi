//! `togi reset`: archive the whole session.

use std::sync::LazyLock;

use clap::Args as ClapArgs;
use togi_hostlock::Error as HostLockError;
use togi_journal::{
    ArchiveKind, Build, DirError, Journal, Lifetime, LockError, OpenError, Operation, SessionId,
    StateDir, Verdict,
};
use togi_machine::BootId;
use togi_session::{CommandError, reset_all};

use crate::cli::{Context, examples};
use crate::exit::Exit;
use crate::report::{
    current_build, emit, exit_with, fail, log_events, refusal_message, unknown_event_message,
};
use crate::system::SystemClock;

static EXAMPLES: LazyLock<String> = LazyLock::new(|| {
    examples(&[(
        "sudo togi reset --all",
        "Archive the session and start over",
    )])
});

/// Archive the whole session
///
/// Archive the whole session, so the next run starts a new one. The reset is a permanent boundary:
/// the new session imports no record from the archived session or anything before it.
///
/// --all archives a journal of any ruleset, schema or evidence epoch, but on a journal of the same
/// schema refuses an unknown event kind or field; install the build that wrote them. Only one run
/// or reset can own this machine, even with different state directories. A busy host lock stops
/// reset before it changes the session.
#[derive(ClapArgs, Debug)]
#[command(after_help = EXAMPLES.as_str())]
pub struct Args {
    #[command(flatten)]
    pub target: Target,
}

/// What to reset: the whole session.
#[derive(ClapArgs, Debug)]
pub struct Target {
    /// Archive the session; the next run starts a new one
    #[arg(long, required = true)]
    pub all: bool,
}

const COMMAND: &str = "reset";
const JOURNAL: &str = "events.jsonl";

/// Resets under the host lock and the state directory's writer lock.
pub fn run(_args: &Args, ctx: &mut Context<'_>) -> Exit {
    let system = ctx.system;
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
    let missing = matches!(system.journal_exists(&journal), Ok(false));
    let pending = system.journal_markers(ctx.state_dir).is_ok_and(|markers| {
        markers
            .iter()
            .any(|(marker, _)| marker.lifetime() == Lifetime::Pending)
    });
    if missing && !pending {
        return fail(ctx, COMMAND, format!("no journal at {}", journal.display()));
    }
    // Read alone, before the journal lock: another build's journal, or a header torn by a crash, is refused without
    // creating the lock file, as the read-only commands refuse it.
    let recorded = system.recorded_build(ctx.state_dir);
    if let Err(error @ DirError::ForeignJournal { .. }) = &recorded {
        return fail(ctx, COMMAND, togi_render::error_chain(error));
    }
    if matches!(recorded, Ok(None)) && !pending {
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
    reset_everything(ctx, dir, boot)
}

fn no_session(ctx: &mut Context<'_>) -> Exit {
    let message = format!("no session in {}", ctx.state_dir.display());
    fail(ctx, COMMAND, message)
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
    let path = if ctx.system.journal_exists(&live).is_ok_and(|exists| exists) {
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
        CommandError::Compatibility(_) => exit_with(ctx, COMMAND, error, Exit::Incompatible),
        CommandError::Append(_)
        | CommandError::Archive(_)
        | CommandError::Close(_)
        | CommandError::Clock(_)
        | CommandError::Dir(_) => fail(ctx, COMMAND, togi_render::error_chain(error)),
    }
}

fn archived_line(session: &SessionId) -> String {
    format!("session {session} archived to archive/{session}.jsonl")
}

fn reset_everything(ctx: &mut Context<'_>, mut dir: StateDir, boot: BootId) -> Exit {
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
    let dropped = match dir.drop_pending_import() {
        Ok(dropped) => dropped,
        Err(error) => return fail(ctx, COMMAND, togi_render::error_chain(&error)),
    };
    let Some(recorded) = recorded else {
        return finish_without_journal(ctx, dir, dropped);
    };
    let session = match build.verdict(&recorded, Operation::ResetAll) {
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
            match reset_all(&build, journal, &mut SystemClock(system)) {
                Ok((session, _)) => {
                    log_recorded(ctx, Some(&session), opened);
                    session
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
    Exit::Ok
}

fn finish_without_journal(
    ctx: &mut Context<'_>,
    mut dir: StateDir,
    dropped: Option<SessionId>,
) -> Exit {
    let interrupted = match dir
        .mark_reset_all()
        .and_then(|_| dir.complete_pending_archive())
    {
        Ok(session) => session,
        Err(error) => return fail(ctx, COMMAND, togi_render::error_chain(&error)),
    };
    if let Some(session) = interrupted {
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
                "import from session {session} dropped; the next togi run starts a new session with nothing imported"
            ),
        )
        .map_or_else(|exit| exit, |()| Exit::Ok),
        None => no_session(ctx),
    }
}
