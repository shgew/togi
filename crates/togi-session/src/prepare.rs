//! What a run needs from the state directory before it opens: the live journal archived when this build or the
//! machine's BIOS context can no longer continue it, the markers of earlier archives settled, and the evidence a
//! transition carries into the new session.

use jiff::Timestamp;
use togi_carry::{Archives, CarryError, PendingCarry, select, settled};
use togi_defect::Entry;
use togi_journal::{
    ArchiveError, ArchiveKind, Build, CloseError, DirError, Event, FileError, Journal, Marker,
    OpenError, Operation, Payload, SessionId, Stamp, StateDir, Verdict,
};
use togi_machine::{BiosContext, BootId};

use crate::session::verdict;

/// The journal a run appends to, and the evidence waiting to be carried into its session.
#[derive(Debug)]
pub struct Prepared {
    pub journal: Journal,
    pub carry: Option<PendingCarry>,
}

#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    #[error(transparent)]
    Dir(#[from] DirError),
    #[error("open the journal: {0}")]
    Open(#[from] OpenError),
    #[error("close the journal: {0}")]
    Close(#[from] CloseError),
    #[error("archive the session: {0}")]
    Archive(#[from] ArchiveError),
    #[error("select the carry: {0}")]
    Carry(#[from] CarryError<FileError>),
}

/// Opens the directory's journal for `boot`, first archiving what a run cannot resume.
///
/// A live journal that ends at or before the reset boundary is archived unread and its carry marker dropped. A
/// journal whose build only differs from this one's, or whose recorded BIOS context differs from `context`, is
/// archived as a transition; one this build refuses stays for the run to refuse. The newest pending carry then
/// becomes the carry unless the boundary passed it or the live journal already committed it, in which case its
/// marker is cleared.
///
/// # Errors
///
/// The failing step; whatever an earlier step archived stays archived.
pub fn prepare(
    mut dir: StateDir,
    build: &Build,
    boot: BootId,
    now: Timestamp,
    context: Option<&BiosContext>,
    defects: &'static [Entry],
) -> Result<Prepared, PrepareError> {
    if let (Some(live), Some(boundary)) = (dir.live_session()?, dir.reset_boundary()?)
        && live <= boundary
    {
        dir.archive_unreadable(ArchiveKind::ResetAll)?;
        dir.clear_pending(Marker::CarryPending)?;
        return Ok(Prepared {
            journal: dir.open_at(boot, now)?,
            carry: None,
        });
    }
    let journal = dir.open_at(boot.clone(), now)?;
    let stamp = Stamp {
        time: now,
        mono: None,
    };
    let journal = match verdict(build, &journal, Operation::Resume) {
        Some(Verdict::Refuse(_)) => {
            return Ok(Prepared {
                journal,
                carry: None,
            });
        }
        Some(Verdict::Archive) => {
            let mut dir = journal.close()?;
            dir.archive_unreadable(ArchiveKind::Transition)?;
            dir.open_at(boot.clone(), now)?
        }
        Some(Verdict::Use) if context_changed(&journal, context) => journal
            .archive(stamp, ArchiveKind::Transition, &[])?
            .open_at(boot.clone(), now)?,
        Some(Verdict::Use) | None => journal,
    };
    let Some(pending) = journal.dir().pending_carry()? else {
        return Ok(Prepared {
            journal,
            carry: None,
        });
    };
    let boundary = journal.dir().reset_boundary()?;
    if boundary.is_some_and(|boundary| pending <= boundary) || settled(pending, journal.events()) {
        let mut dir = journal.close()?;
        dir.clear_pending(Marker::CarryPending)?;
        return Ok(Prepared {
            journal: dir.open_at(boot, now)?,
            carry: None,
        });
    }
    let dir = journal.dir();
    let chain: Vec<SessionId> = std::iter::once(pending)
        .chain(
            dir.archives()?
                .into_iter()
                .rev()
                .filter(|session| *session < pending),
        )
        .collect();
    let carry = select(&chain, boundary, defects, &mut Archived(dir))?;
    Ok(Prepared { journal, carry })
}

fn context_changed(journal: &Journal, current: Option<&BiosContext>) -> bool {
    let Some(current) = current else {
        return false;
    };
    journal
        .events()
        .iter()
        .find_map(|event| match &event.payload {
            Payload::SessionContext(recorded) => Some(recorded.bios_context()),
            _ => None,
        })
        .is_some_and(|recorded| recorded != *current)
}

struct Archived<'a>(&'a StateDir);

impl Archives for Archived<'_> {
    type Error = FileError;
    type Events = Vec<Result<Event, FileError>>;

    fn events(&mut self, session: SessionId) -> Result<Option<Self::Events>, FileError> {
        Ok(self
            .0
            .archived_recording(&session)?
            .map(|recording| recording.into_events().into_iter().map(Ok).collect()))
    }
}

#[cfg(test)]
mod tests;
