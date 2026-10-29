//! What a run needs from the state directory before it opens: the live journal archived when this build can no
//! longer continue it, the markers of earlier archives settled, and original-identity observations and
//! irreversible campaign history selected for the successor.

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

/// A transition whose original observations, successor import and campaign enrollment are not yet complete.
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
/// A live journal at or before the reset boundary is archived unread. Raw tuning observations stop
/// at that boundary; campaign consumption and permanent stops do not. A changed build archives
/// behind a marker naming its successor, while a changed BIOS context continues the live session.
/// A newer schema, ruleset or evidence epoch is judged from its first line and refused before the
/// journal opens. The import marker clears only once campaign enrollment is durable.
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
        let journal = dir.open_at(boot.clone(), now)?;
        return import_into(journal, boot, now);
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
    let marker = journal.dir().pending_import()?;
    let boundary = journal.dir().reset_boundary()?;
    let committed = journal
        .events()
        .iter()
        .any(|event| matches!(event.payload, Payload::CampaignStart(_)));
    if committed {
        if marker.is_some() {
            let mut dir = journal.close()?;
            dir.clear_pending(Marker::ImportPending)?;
            return Ok(Prepared {
                journal: dir.open_at(boot, now)?,
                import: None,
            });
        }
        return Ok(Prepared {
            journal,
            import: None,
        });
    }
    let source = if let Some(marker) = marker {
        marker.source
    } else {
        let Some(source) = journal.dir().archives()?.last().copied() else {
            return Ok(Prepared {
                journal,
                import: None,
            });
        };
        source
    };
    let destination = match journal.session() {
        Some(session) => session,
        None => match marker {
            Some(marker) if boundary.is_none_or(|boundary| marker.destination > boundary) => {
                marker.destination
            }
            Some(_) | None => journal.dir().import_destination(&source, now)?,
        },
    };
    let manifest = manifest(journal.dir(), boundary, &destination)?;
    Ok(Prepared {
        journal,
        import: Some(PendingImport {
            source,
            destination,
            manifest,
        }),
    })
}

/// All campaign history survives reset and import commitments. Raw tuning observations are selected
/// only after the permanent reset boundary; the destination itself is never an archive source.
fn manifest(
    dir: &StateDir,
    boundary: Option<SessionId>,
    destination: &SessionId,
) -> Result<Manifest, DirError> {
    let mut sources = Vec::new();
    for session in dir.archives()?.into_iter().rev() {
        if session == *destination {
            continue;
        }
        let Some(bytes) = dir.archived_raw(&session)? else {
            continue;
        };
        sources.push(import::Source {
            session,
            bytes,
            carry_raw: boundary.is_none_or(|boundary| session > boundary),
        });
    }
    Ok(import::manifest(&sources))
}

/// A caller that opened a new journal directly still cannot renew an archived campaign.
pub(crate) fn inherit(journal: &Journal) -> Result<Option<PendingImport>, DirError> {
    let Some(destination) = journal.session() else {
        return Ok(None);
    };
    let Some(source) = journal.dir().archives()?.last().copied() else {
        return Ok(None);
    };
    let boundary = journal.dir().reset_boundary()?;
    Ok(Some(PendingImport {
        source,
        destination,
        manifest: manifest(journal.dir(), boundary, &destination)?,
    }))
}

#[cfg(test)]
mod tests;
