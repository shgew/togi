use togi_journal::{
    AppendError, ArchiveError, ArchiveKind, Build, CloseError, CommandReset, DirError, Journal,
    Operation, Payload, Refusal, ResetTarget, Seq, SessionId, Stamp, StateDir, Verdict,
};
use togi_machine::{Clock, CoreId};

use crate::appender::Folds;
use crate::error::FoldError;
use crate::session::{refusal, verdict};

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("no session in the journal")]
    NoSession,
    #[error(transparent)]
    Compatibility(#[from] Refusal),
    #[error("reset core {core:02}: the session has no such core")]
    NoSuchCore { core: CoreId },
    #[error(transparent)]
    Append(#[from] AppendError),
    #[error(transparent)]
    Fold(#[from] FoldError),
    #[error("archive the session: {0}")]
    Archive(#[from] ArchiveError),
    #[error(transparent)]
    Close(#[from] CloseError),
    #[error(transparent)]
    Dir(#[from] DirError),
}

/// Records `command.reset` for `core`, so a later run starts it over. Refuses a journal this build may not append
/// to before folding it, and a core the session was not recorded on. Appends nothing else; on an error the journal
/// holds what it held.
pub fn reset_core(
    build: &Build,
    journal: &mut Journal,
    clock: &mut dyn Clock,
    core: CoreId,
) -> Result<Seq, CommandError> {
    if journal.events().is_empty() {
        return Err(CommandError::NoSession);
    }
    if let Some(refusal) = refusal(build, journal) {
        return Err(refusal.into());
    }
    let folds = Folds::replay(journal.events())?;
    if !folds.recovery.topology().contains_key(&core) {
        return Err(CommandError::NoSuchCore { core });
    }
    let payload = CommandReset {
        target: ResetTarget::Core(core),
    };
    Ok(journal
        .append(stamp(clock), Payload::CommandReset(payload), &[])?
        .seq)
}

/// Records `command.reset` for every core and archives the session, which ends it. A journal whose ruleset,
/// evidence epoch or schema this build cannot read is archived as it is, unread. The journal is never folded, so
/// one no run can resume is still reset. Every outcome spends the journal: an error after the verdict leaves the
/// session wherever the failed step left it, and the caller reopens the state directory to see which.
pub fn reset_all(
    build: &Build,
    mut journal: Journal,
    clock: &mut dyn Clock,
) -> Result<(SessionId, StateDir), CommandError> {
    let operation = verdict(build, &journal, Operation::ResetAll).ok_or(CommandError::NoSession)?;
    if let Verdict::Refuse(refusal) = operation {
        return Err(refusal.into());
    }
    let session = journal
        .events()
        .iter()
        .find_map(|event| match &event.payload {
            Payload::SessionStart(start) => Some(start.session),
            _ => None,
        })
        .ok_or(CommandError::NoSession)?;
    if operation == Verdict::Archive {
        let mut dir = journal.close()?;
        let archived = dir.archive_unreadable(ArchiveKind::ResetAll)?;
        if archived.is_none() {
            return Err(CommandError::NoSession);
        }
        return Ok((session, dir));
    }
    let reset = journal
        .append(
            stamp(clock),
            Payload::CommandReset(CommandReset {
                target: ResetTarget::All,
            }),
            &[],
        )?
        .seq;
    let dir = journal.archive(stamp(clock), ArchiveKind::ResetAll, &[reset])?;
    Ok((session, dir))
}

fn stamp(clock: &dyn Clock) -> Stamp {
    Stamp {
        time: clock.now(),
        mono: Some(clock.monotonic()),
    }
}

#[cfg(test)]
mod tests;
