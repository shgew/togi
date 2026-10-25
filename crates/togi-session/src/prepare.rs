//! What a run needs from the state directory before it opens: the live journal archived when this build can no
//! longer continue it, the markers of earlier archives settled, and the raw day records
//! a fresh transition imports into the new session.

use jiff::Timestamp;
use togi_journal::{
    ArchiveError, ArchiveKind, Build, CloseError, DirError, Journal, Marker, OpenError, Operation,
    Payload, Refusal, SessionId, StateDir, Verdict,
};
use togi_machine::BootId;

mod import;

pub use import::Manifest;

/// The journal a run appends to, and the import waiting to be recorded in its session.
#[derive(Debug)]
pub struct Prepared {
    pub journal: Journal,
    pub import: Option<PendingImport>,
}

/// A fresh transition whose `day.imported` events and `session.imported` commitment the new session has yet to
/// hold in full.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingImport {
    source: SessionId,
    destination: SessionId,
    manifest: Manifest,
}

impl PendingImport {
    /// The oldest archive of the transition, the one its marker is named for.
    pub const fn source(&self) -> SessionId {
        self.source
    }

    /// The session that imports: the live journal's, or the id the marker reserved for it.
    pub const fn destination(&self) -> SessionId {
        self.destination
    }

    pub const fn manifest(&self) -> &Manifest {
        &self.manifest
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    #[error(transparent)]
    Dir(#[from] DirError),
    #[error("open the journal")]
    Open(#[from] OpenError),
    #[error("close the journal")]
    Close(#[from] CloseError),
    #[error("archive the session")]
    Archive(#[from] ArchiveError),
    #[error("the live journal is left for the build that wrote it")]
    Refused(#[source] Refusal),
}

/// Opens the directory's journal for `boot`, first archiving what a run cannot resume.
///
/// A live journal that ends at or before the reset boundary is archived unread and its import marker dropped. A
/// journal whose build only differs from this one's is
/// archived as a fresh transition behind a marker naming the session that will import its day records. A changed
/// BIOS context never archives: the live session continues. One this
/// build refuses, a newer schema, ruleset or evidence epoch, is judged from its first line alone and returned as
/// [`PrepareError::Refused`] before anything opens it: opening would record a torn tail in it and decode events
/// only the build that wrote it may read. The newest pending marker then becomes the import unless the boundary
/// passed it or the live journal already committed it, in which case its marker is cleared.
///
/// # Errors
///
/// The failing step; whatever an earlier step archived stays archived.
pub fn prepare(
    mut dir: StateDir,
    build: &Build,
    boot: BootId,
    now: Timestamp,
) -> Result<Prepared, PrepareError> {
    if let (Some(live), Some(boundary)) = (dir.live_session()?, dir.reset_boundary()?)
        && live <= boundary
    {
        dir.archive_unreadable(ArchiveKind::ResetAll)?;
        dir.clear_pending(Marker::ImportPending)?;
        return Ok(Prepared {
            journal: dir.open_at(boot, now)?,
            import: None,
        });
    }
    let verdict = dir
        .live_build()?
        .map(|recorded| build.verdict(&recorded, Operation::Resume));
    match verdict {
        Some(Verdict::Refuse(refusal)) => return Err(PrepareError::Refused(refusal)),
        Some(Verdict::Archive) => {
            if let Some(live) = dir.live_session()? {
                let destination = dir.import_destination(&live, now)?;
                dir.archive_unreadable(ArchiveKind::Transition { destination })?;
            }
        }
        Some(Verdict::Use) | None => {}
    }
    let journal = dir.open_at(boot.clone(), now)?;
    import_into(journal, boot, now)
}

/// [`prepare`] for a journal that is already open and was rebooted into `boot` ([`Journal::reboot`]): the same
/// decisions, made from its events instead of a read of the file. A journal [`prepare`] would archive before opening,
/// one at or before the reset boundary or whose build only differs from this one's, or one this build refuses, is
/// closed and handed to [`prepare`], which acts on the directory exactly as it does for a closed one.
///
/// # Errors
///
/// As [`prepare`].
pub fn prepare_kept(
    journal: Journal,
    build: &Build,
    boot: BootId,
    now: Timestamp,
) -> Result<Prepared, PrepareError> {
    let reset = match (journal.session(), journal.dir().reset_boundary()?) {
        (Some(live), Some(boundary)) => live <= boundary,
        _ => false,
    };
    let verdict = journal.events().first().and_then(|event| {
        if let Payload::SessionStart(start) = &event.payload {
            Some(build.verdict(&start.build, Operation::Resume))
        } else {
            None
        }
    });
    if reset || !matches!(verdict, Some(Verdict::Use) | None) {
        return prepare(journal.close()?, build, boot, now);
    }
    import_into(journal, boot, now)
}

/// What [`prepare`] does once the journal is open: finds the import the new session starts with. A changed BIOS
/// context is not a transition: the tuner handles it inside the live session.
fn import_into(journal: Journal, boot: BootId, now: Timestamp) -> Result<Prepared, PrepareError> {
    let Some(marker) = journal.dir().pending_import()? else {
        return Ok(Prepared {
            journal,
            import: None,
        });
    };
    let boundary = journal.dir().reset_boundary()?;
    let committed = journal
        .events()
        .iter()
        .any(|event| matches!(event.payload, Payload::SessionImported(_)));
    if boundary.is_some_and(|boundary| marker.source <= boundary) || committed {
        let mut dir = journal.close()?;
        dir.clear_pending(Marker::ImportPending)?;
        return Ok(Prepared {
            journal: dir.open_at(boot, now)?,
            import: None,
        });
    }
    let destination = journal.session().unwrap_or(marker.destination);
    let manifest = manifest(journal.dir(), boundary, &destination)?;
    Ok(Prepared {
        journal,
        import: Some(PendingImport {
            source: marker.source,
            destination,
            manifest,
        }),
    })
}

/// The day records of every archive after the reset boundary, newest first, as far back as the newest archive that
/// already holds a committed import; the session that will import is not one of them.
fn manifest(
    dir: &StateDir,
    boundary: Option<SessionId>,
    destination: &SessionId,
) -> Result<Manifest, DirError> {
    let mut sources = Vec::new();
    for session in dir.archives()?.into_iter().rev() {
        if session == *destination || boundary.is_some_and(|boundary| session <= boundary) {
            continue;
        }
        let Some(bytes) = dir.archived_raw(&session)? else {
            continue;
        };
        let committed = import::commits_import(&bytes);
        sources.push(import::Source { session, bytes });
        if committed {
            break;
        }
    }
    Ok(import::manifest(&sources))
}

#[cfg(test)]
mod tests;
