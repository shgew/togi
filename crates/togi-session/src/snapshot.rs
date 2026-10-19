//! Read-only restoration of the same reducers the run keeps, with full replay as the fallback.

use std::path::Path;

use togi_journal::fs::Fs;
use togi_journal::{Event, StateDir};

use crate::FoldError;
use crate::appender::Folds;
use crate::publication::StateDocument;

/// Complete folded state for readers. A cache is optional and never changes the journal's interpretation.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadState {
    folds: Folds,
}

/// Independent audit of a persisted snapshot against the authoritative log.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SnapshotStatus {
    Missing,
    Stale(Option<togi_journal::Seq>),
    Matches,
    Different,
}

/// A snapshot that cannot be checked.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error(transparent)]
    Rejected(#[from] togi_journal::snapshot::Rejected),
    #[error(transparent)]
    Fold(#[from] FoldError),
}

/// Replays independently and checks every persisted fold; auditing never trusts the cache it checks.
///
/// # Errors
///
/// Returns a cache rejection or a journal fold failure.
pub fn verify_snapshot(
    directory: &StateDir,
    events: &[Event],
) -> Result<SnapshotStatus, SnapshotError> {
    let Some((prefix, saved)) = directory.read_snapshot::<StateDocument>()? else {
        return Ok(SnapshotStatus::Missing);
    };
    if prefix.last_seq != saved.facts.last_seq {
        return Ok(SnapshotStatus::Different);
    }
    let fresh = Folds::replay(events)?;
    let restored = ReadState::restore(Some((prefix, saved.clone())), events)?;
    if restored.folds != fresh {
        return Ok(SnapshotStatus::Different);
    }
    if saved.facts.last_seq != events.last().map(|event| event.seq) {
        return Ok(SnapshotStatus::Stale(saved.facts.last_seq));
    }
    let fresh = StateDocument::of(&fresh);
    Ok(if saved == fresh {
        SnapshotStatus::Matches
    } else {
        SnapshotStatus::Different
    })
}

impl StateDocument {
    fn into_folds(self) -> Folds {
        Folds {
            facts: self.facts,
            tuner: self.incremental,
            recovery: self.recovery,
            defects: self.defects,
            presentation: self.presentation,
        }
    }
}

impl ReadState {
    /// Folds all events independently of any cache.
    ///
    /// # Errors
    ///
    /// Returns the journal or tuner fold failure.
    pub fn replay(events: &[Event]) -> Result<Self, FoldError> {
        Folds::replay(events).map(|folds| Self { folds })
    }

    /// The readable document, without its journal-prefix verification stamp.
    pub fn document(&self) -> impl serde::Serialize + '_ {
        crate::publication::StateDocumentRef::of(&self.folds)
    }

    pub(crate) fn into_folds(self) -> Folds {
        self.folds
    }

    /// Loads from a read-only filesystem and folds only the uncovered tail. Invalid caches are discarded.
    ///
    /// # Errors
    ///
    /// Returns a fold failure from the authoritative journal, never from the optional cache.
    pub fn read_in(fs: &dyn Fs, directory: &Path, events: &[Event]) -> Result<Self, FoldError> {
        Self::restore(
            togi_journal::snapshot::read_in(fs, directory)
                .ok()
                .flatten(),
            events,
        )
    }

    /// Loads a disk snapshot without taking a writer lock.
    ///
    /// # Errors
    ///
    /// Returns a fold failure from the authoritative journal.
    pub fn read(directory: &Path, events: &[Event]) -> Result<Self, FoldError> {
        Self::restore(
            togi_journal::snapshot::read(directory).ok().flatten(),
            events,
        )
    }

    /// Loads through an already-owned directory without changing it.
    ///
    /// # Errors
    ///
    /// Returns a fold failure from the authoritative journal.
    pub fn in_dir(directory: &StateDir, events: &[Event]) -> Result<Self, FoldError> {
        Self::restore(directory.read_snapshot().ok().flatten(), events)
    }

    pub(crate) fn restore(
        saved: Option<(togi_journal::snapshot::Prefix, StateDocument)>,
        events: &[Event],
    ) -> Result<Self, FoldError> {
        if let Some((prefix, saved)) = saved {
            let same_session = saved.facts.session.as_ref().map(|session| session.id)
                == events.first().and_then(|event| match &event.payload {
                    togi_journal::Payload::SessionStart(start) => Some(start.session),
                    _ => None,
                });
            if same_session
                && saved.facts.last_seq == prefix.last_seq
                && saved.tuner == saved.incremental.project()
                && prefix.last_seq <= events.last().map(|event| event.seq)
            {
                let mut folds = saved.into_folds();
                let tail = events.partition_point(|event| Some(event.seq) <= prefix.last_seq);
                if events[tail..]
                    .iter()
                    .try_for_each(|event| folds.apply(event))
                    .is_ok()
                {
                    return Ok(Self { folds });
                }
            }
        }
        Self::replay(events)
    }

    /// Shared journal facts at the end of the journal, not merely at the snapshot boundary.
    pub const fn facts(&self) -> &togi_journal::State {
        &self.folds.facts
    }

    /// The complete incremental tuner state, including the state needed by forecast premises.
    pub const fn tuner(&self) -> &togi_tuner::State {
        &self.folds.tuner
    }

    /// The current defect index.
    pub const fn defects(&self) -> &togi_defect::DefectState {
        &self.folds.defects
    }

    /// Independently checks every persisted reducer against a full replay.
    ///
    /// # Errors
    ///
    /// Returns a fold failure from the journal.
    pub fn matches_replay(&self, events: &[Event]) -> Result<bool, FoldError> {
        Ok(self.folds == Folds::replay(events)?)
    }
}
