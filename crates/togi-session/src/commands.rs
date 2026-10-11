//! The two journal commands that change what a later run does: resetting one core's search and discarding the
//! session. Each is a single durable `command.reset`; neither records a shutdown, because the boot that wrote them
//! may hold offsets a crashed run left, and a shutdown would hide that crash from the next run.

use togi_journal::{
    ArchiveError, ArchiveKind, Build, CommandReset, Payload, Refusal, ResetTarget, SessionId,
    Stamp, StateDir,
};
use togi_machine::CoreId;

use crate::appender::Appender;
use crate::error::AppendFailure;
use crate::session::refusal;

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("no session in the journal")]
    NoSession,
    #[error(transparent)]
    Compatibility(#[from] Refusal),
    #[error("reset core {core:02}: the session has no such core")]
    NoSuchCore { core: CoreId },
    #[error(transparent)]
    Append(#[from] AppendFailure),
    #[error("archive the session: {0}")]
    Archive(#[from] ArchiveError),
}

fn check(build: &Build, appender: &Appender) -> Result<(), CommandError> {
    let journal = appender.journal();
    if journal.events().is_empty() {
        return Err(CommandError::NoSession);
    }
    refusal(build, journal).map_or(Ok(()), |refusal| Err(refusal.into()))
}

/// Queues the reset of one core's search for the next run.
pub fn reset_core(
    build: &Build,
    appender: &mut Appender,
    core: CoreId,
) -> Result<togi_journal::Seq, CommandError> {
    check(build, appender)?;
    if appender.recovery().phase(core).is_none() {
        return Err(CommandError::NoSuchCore { core });
    }
    let payload = CommandReset {
        target: ResetTarget::Core(core),
    };
    Ok(appender.append(Payload::CommandReset(payload), &[])?)
}

/// Records the reset and archives the session; the journal is spent afterwards and its directory is handed back.
pub fn reset_all(
    build: &Build,
    mut appender: Appender,
) -> Result<(SessionId, StateDir), CommandError> {
    check(build, &appender)?;
    let session = appender
        .journal()
        .events()
        .iter()
        .find_map(|event| match &event.payload {
            Payload::SessionStart(start) => Some(start.session),
            _ => None,
        })
        .ok_or(CommandError::NoSession)?;
    let payload = CommandReset {
        target: ResetTarget::All,
    };
    let seq = appender.append(Payload::CommandReset(payload), &[])?;
    let clock = appender.clock();
    let stamp = Stamp {
        time: clock.now(),
        mono: Some(clock.monotonic()),
    };
    let dir = appender
        .into_journal()
        .archive(stamp, ArchiveKind::ResetAll, &[seq])?;
    Ok((session, dir))
}

#[cfg(test)]
mod tests;
