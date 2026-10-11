//! Moving a session into `archive/`, the markers that make each step resumable, and the reset boundary.

use std::fmt;
use std::io::{self, BufRead, BufReader};
use std::path::PathBuf;
use std::str::FromStr;

use serde::Deserialize;

use super::{DirError, EVENTS_FILE, STATE_FILE, STATE_TMP, StateDir, TRIALS_DIR, io_at, not_found};
use crate::fs::FileKind;
use crate::session_id::SessionId;

const JOURNAL_SUFFIX: &str = ".jsonl";
const TRIALS_SUFFIX: &str = "-trials";

/// A zero-byte file in `archive/` named after a session, whose existence is the record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Marker {
    /// An archive of a journal this build cannot append to is under way.
    CompatPending,
    /// The session's evidence is waiting to be carried into a session that has not committed it yet.
    CarryPending,
    /// Nothing at or before the session may feed a later session.
    ResetAll,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifetime {
    Pending,
    Permanent,
}

impl Marker {
    const ALL: [Self; 3] = [Self::CompatPending, Self::CarryPending, Self::ResetAll];

    const fn suffix(self) -> &'static str {
        match self {
            Self::CompatPending => "-compat-pending",
            Self::CarryPending => "-carry-pending",
            Self::ResetAll => "-reset-all",
        }
    }

    pub const fn lifetime(self) -> Lifetime {
        match self {
            Self::CompatPending | Self::CarryPending => Lifetime::Pending,
            Self::ResetAll => Lifetime::Permanent,
        }
    }
}

impl fmt::Display for Marker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.suffix().trim_start_matches('-'))
    }
}

/// Why a session's journal leaves the state directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveKind {
    /// A new session will carry this session's evidence; a pending carry from an earlier session stays the source.
    Transition,
    /// The session and everything before it is discarded for good.
    ResetAll,
}

#[derive(Deserialize)]
struct Peek {
    kind: String,
    #[serde(default)]
    data: PeekData,
}

#[derive(Default, Deserialize)]
struct PeekData {
    session: Option<String>,
}

impl StateDir {
    pub(crate) fn archive_journal(&self, session: &SessionId) -> PathBuf {
        self.archive_dir()
            .join(format!("{session}{JOURNAL_SUFFIX}"))
    }

    pub(crate) fn archive_trials(&self, session: &SessionId) -> PathBuf {
        self.archive_dir().join(format!("{session}{TRIALS_SUFFIX}"))
    }

    fn marker_path(&self, marker: Marker, session: &SessionId) -> PathBuf {
        self.archive_dir()
            .join(format!("{session}{}", marker.suffix()))
    }

    fn archive_entries(&self) -> Result<Vec<crate::fs::DirEntry>, DirError> {
        let dir = self.archive_dir();
        match self.fs().read_dir(&dir) {
            Ok(entries) => Ok(entries),
            Err(error) if not_found(&error) => Ok(Vec::new()),
            Err(error) => Err(io_at("list archive", &dir)(error)),
        }
    }

    /// The archived sessions: regular `<session>.jsonl` files, oldest first.
    pub fn archives(&self) -> Result<Vec<SessionId>, DirError> {
        let mut sessions: Vec<SessionId> = self
            .archive_entries()?
            .iter()
            .filter(|entry| entry.kind == FileKind::File)
            .filter_map(|entry| {
                entry
                    .name
                    .to_str()?
                    .strip_suffix(JOURNAL_SUFFIX)?
                    .parse()
                    .ok()
            })
            .collect();
        sessions.sort();
        Ok(sessions)
    }

    /// The markers present, ordered by session then by kind. A marker that is not a regular file is an error.
    pub fn markers(&self) -> Result<Vec<(Marker, SessionId)>, DirError> {
        let mut markers = Vec::new();
        for entry in self.archive_entries()? {
            let Some(name) = entry.name.to_str() else {
                continue;
            };
            let found = Marker::ALL.into_iter().find_map(|marker| {
                let session = name.strip_suffix(marker.suffix())?.parse().ok()?;
                Some((marker, session))
            });
            let Some(found) = found else { continue };
            if entry.kind != FileKind::File {
                return Err(DirError::NotRegular {
                    path: self.archive_dir().join(&entry.name),
                });
            }
            markers.push(found);
        }
        markers.sort_by_key(|(marker, session)| (*session, *marker));
        Ok(markers)
    }

    fn marked(&self, marker: Marker) -> Result<Vec<SessionId>, DirError> {
        Ok(self
            .markers()?
            .into_iter()
            .filter(|(found, _)| *found == marker)
            .map(|(_, session)| session)
            .collect())
    }

    /// The newest session a reset-all marker names: nothing at or before it may feed a later session.
    pub fn reset_boundary(&self) -> Result<Option<SessionId>, DirError> {
        Ok(self.marked(Marker::ResetAll)?.pop())
    }

    /// The newest session whose carry no journal has committed yet.
    pub fn pending_carry(&self) -> Result<Option<SessionId>, DirError> {
        Ok(self.marked(Marker::CarryPending)?.pop())
    }

    fn ensure_archive_dir(&self) -> Result<(), DirError> {
        let dir = self.archive_dir();
        if self.kind_of(&dir)?.is_none() {
            match self.fs().create_dir(&dir) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(io_at("create archive directory", &dir)(error)),
            }
        }
        self.sync_dir(&self.path)
    }

    fn mark(&self, marker: Marker, session: &SessionId) -> Result<(), DirError> {
        self.ensure_archive_dir()?;
        let path = self.marker_path(marker, session);
        if self
            .kind_of(&path)?
            .is_some_and(|kind| kind != FileKind::File)
        {
            return Err(DirError::NotRegular { path });
        }
        let mut file = self
            .fs()
            .create_truncate(&path)
            .map_err(io_at("create marker", &path))?;
        file.sync().map_err(io_at("sync marker", &path))?;
        drop(file);
        self.sync_dirs()
    }

    /// Removes every pending marker of the kind and syncs the archive directory. A permanent marker cannot be removed.
    pub fn clear_pending(&mut self, marker: Marker) -> Result<(), DirError> {
        if marker.lifetime() == Lifetime::Permanent {
            return Err(DirError::PermanentMarker(marker));
        }
        for session in self.marked(marker)? {
            let path = self.marker_path(marker, &session);
            match self.fs().remove_file(&path) {
                Ok(()) => {}
                Err(error) if not_found(&error) => {}
                Err(error) => return Err(io_at("remove marker", &path)(error)),
            }
        }
        match self.sync_dir(&self.archive_dir()) {
            Err(DirError::Io { source, .. }) if not_found(&source) => Ok(()),
            result => result,
        }
    }

    /// Makes the boundary durable at the newest session there is: an archive, a marker or the live journal. A
    /// directory with none of them gets no boundary.
    pub fn mark_reset_all(&mut self) -> Result<Option<SessionId>, DirError> {
        let live = self.live_session()?;
        self.mark_reset_through(live)
    }

    pub(crate) fn mark_reset_through(
        &self,
        live: Option<SessionId>,
    ) -> Result<Option<SessionId>, DirError> {
        let newest = self
            .archives()?
            .into_iter()
            .chain(self.markers()?.into_iter().map(|(_, session)| session))
            .chain(live)
            .max();
        if let Some(session) = &newest {
            self.mark(Marker::ResetAll, session)?;
        }
        Ok(newest)
    }

    /// Discards the pending carry: the reset boundary becomes durable before the carry markers go.
    pub fn drop_pending_carry(&mut self) -> Result<Option<SessionId>, DirError> {
        let Some(dropped) = self.pending_carry()? else {
            return Ok(None);
        };
        self.mark_reset_through(Some(dropped))?;
        self.clear_pending(Marker::CarryPending)?;
        Ok(Some(dropped))
    }

    /// Refuses a session whose archived journal or archived trial directory already exists. Its own transaction
    /// markers do not count: they are how an interrupted archive resumes.
    pub(crate) fn check_archive_free(&self, session: &SessionId) -> Result<(), DirError> {
        for path in [self.archive_journal(session), self.archive_trials(session)] {
            if self.kind_of(&path)?.is_some() {
                return Err(DirError::ArchiveExists { session: *session });
            }
        }
        Ok(())
    }

    /// Whether anything in `archive/` already carries the session's id, markers included.
    pub(crate) fn id_reserved(&self, session: &SessionId) -> Result<bool, DirError> {
        let paths = [self.archive_journal(session), self.archive_trials(session)]
            .into_iter()
            .chain(
                Marker::ALL
                    .into_iter()
                    .map(|marker| self.marker_path(marker, session)),
            );
        for path in paths {
            if self.kind_of(&path)?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The marker a transaction of this kind leaves behind before anything is moved.
    pub(crate) fn mark_archive_kind(
        &self,
        kind: ArchiveKind,
        session: &SessionId,
    ) -> Result<(), DirError> {
        match kind {
            ArchiveKind::ResetAll => self.mark_reset_through(Some(*session)).map(drop),
            ArchiveKind::Transition => {
                if self.pending_carry()?.is_none() {
                    self.mark(Marker::CarryPending, session)?;
                }
                Ok(())
            }
        }
    }

    /// Moves the session out of the live paths, each step durable before the next and each safe to repeat.
    pub(crate) fn finish_archive(&self, session: &SessionId) -> Result<(), DirError> {
        self.ensure_archive_dir()?;
        for name in [STATE_FILE, STATE_TMP] {
            let path = self.join(name);
            match self.fs().remove_file(&path) {
                Ok(()) => {}
                Err(error) if not_found(&error) => {}
                Err(error) => return Err(io_at("remove state", &path)(error)),
            }
        }
        self.sync_dir(&self.path)?;
        let trials = self.join(TRIALS_DIR);
        let archived_trials = self.archive_trials(session);
        match self.fs().rename_noreplace(&trials, &archived_trials) {
            Ok(()) => {}
            Err(error) if not_found(&error) => {}
            Err(error) => return Err(io_at("archive trials", &trials)(error)),
        }
        self.sync_dirs()?;
        let events = self.join(EVENTS_FILE);
        let target = self.archive_journal(session);
        match self.fs().rename_noreplace(&events, &target) {
            Ok(()) => {}
            Err(error) if not_found(&error) => {
                if self.kind_of(&target)? != Some(FileKind::File) {
                    return Err(DirError::MissingArchive { session: *session });
                }
            }
            Err(error) => return Err(io_at("archive journal", &events)(error)),
        }
        self.sync_dirs()
    }

    /// Finishes every archive a compat marker left pending, then removes the marker.
    pub(crate) fn recover_compat(&self) -> Result<(), DirError> {
        for session in self.marked(Marker::CompatPending)? {
            let events = self.join(EVENTS_FILE);
            if self.kind_of(&events)?.is_some() {
                if self.live_session()? != Some(session) {
                    return Err(DirError::NotAJournal { path: events });
                }
                self.finish_archive(&session)?;
            } else if self.kind_of(&self.archive_journal(&session))? == Some(FileKind::File) {
                self.sync_dirs()?;
            } else {
                return Err(DirError::MissingArchive { session });
            }
            self.remove_marker(Marker::CompatPending, &session)?;
        }
        Ok(())
    }

    fn remove_marker(&self, marker: Marker, session: &SessionId) -> Result<(), DirError> {
        let path = self.marker_path(marker, session);
        match self.fs().remove_file(&path) {
            Ok(()) => {}
            Err(error) if not_found(&error) => {}
            Err(error) => return Err(io_at("remove marker", &path)(error)),
        }
        self.sync_dir(&self.archive_dir())
    }

    /// A carry marker for the session that still owns the live journal is a leftover of an archive that never started.
    pub(crate) fn drop_stale_carry(&self, live: &SessionId) -> Result<(), DirError> {
        if self.marked(Marker::CarryPending)?.contains(live) {
            self.remove_marker(Marker::CarryPending, live)?;
        }
        Ok(())
    }

    /// The session the live journal belongs to, read from its first complete line; `None` without one.
    pub fn live_session(&self) -> Result<Option<SessionId>, DirError> {
        let path = self.join(EVENTS_FILE);
        let reader = match self.fs().open_read(&path) {
            Ok(reader) => reader,
            Err(error) if not_found(&error) => return Ok(None),
            Err(error) => return Err(io_at("read journal", &path)(error)),
        };
        let mut line = Vec::new();
        BufReader::new(reader)
            .read_until(b'\n', &mut line)
            .map_err(io_at("read journal", &path))?;
        if line.pop() != Some(b'\n') {
            return Ok(None);
        }
        match peek_session(&line, "session.start") {
            Some(Ok(session)) => Ok(Some(session)),
            Some(Err(source)) => Err(DirError::BadSession { path, source }),
            None => Err(DirError::NotAJournal { path }),
        }
    }

    /// Archives a journal this build cannot read, without appending to it. A journal that already records its own
    /// archive is only moved, plus the reset boundary when the kind is `ResetAll`. Returns the session archived, or
    /// `None` when the directory holds no session.
    pub fn archive_unreadable(&mut self, kind: ArchiveKind) -> Result<Option<SessionId>, DirError> {
        let Some(session) = self.live_session()? else {
            return Ok(None);
        };
        if self.records_archive(&session)? {
            if kind == ArchiveKind::ResetAll {
                self.mark_reset_through(Some(session))?;
            }
            self.finish_archive(&session)?;
            return Ok(Some(session));
        }
        if self
            .kind_of(&self.marker_path(Marker::CompatPending, &session))?
            .is_none()
        {
            self.check_archive_free(&session)?;
        }
        self.mark_archive_kind(kind, &session)?;
        self.mark(Marker::CompatPending, &session)?;
        self.finish_archive(&session)?;
        self.remove_marker(Marker::CompatPending, &session)?;
        Ok(Some(session))
    }

    fn records_archive(&self, session: &SessionId) -> Result<bool, DirError> {
        let path = self.join(EVENTS_FILE);
        let reader = self
            .fs()
            .open_read(&path)
            .map_err(io_at("read journal", &path))?;
        let mut reader = BufReader::new(reader);
        let mut line = Vec::new();
        loop {
            line.clear();
            reader
                .read_until(b'\n', &mut line)
                .map_err(io_at("read journal", &path))?;
            if line.pop() != Some(b'\n') {
                return Ok(false);
            }
            if peek_session(&line, "session.archived")
                .and_then(Result::ok)
                .as_ref()
                == Some(session)
            {
                return Ok(true);
            }
        }
    }
}

fn peek_session(
    line: &[u8],
    kind: &str,
) -> Option<Result<SessionId, crate::session_id::InvalidSessionId>> {
    let peek: Peek = serde_json::from_slice(line).ok()?;
    if peek.kind != kind {
        return None;
    }
    Some(SessionId::from_str(&peek.data.session?))
}
