//! Moving a session into `archive/`, the markers that make each step resumable, and the reset boundary.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::Deserialize;
use togi_machine::serde_object::{self, Object};

use super::{
    ARCHIVE_DIR, DirError, EVENTS_FILE, STATE_FILE, STATE_TMP, StateDir, TRIALS_DIR, io_at,
    not_found,
};
use crate::build::Build;
use crate::fs::{DiskFs, FileKind, Fs};
use crate::header;
use crate::session_id::SessionId;

const JOURNAL_SUFFIX: &str = ".jsonl";
const TRIALS_SUFFIX: &str = "-trials";
const IMPORT_TMP: &str = ".import-pending.tmp";

/// A file in `archive/` named after a session, whose existence is the record. Only an import marker has content: the
/// destination session it names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Marker {
    /// An archive of a journal this build cannot append to is under way.
    CompatPending,
    /// The session's day records are waiting to be imported into a destination session that has not committed them.
    ImportPending,
    /// Nothing at or before the session may feed a later session.
    ResetAll,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifetime {
    Pending,
    Permanent,
}

impl Marker {
    const ALL: [Self; 3] = [Self::CompatPending, Self::ImportPending, Self::ResetAll];

    const fn suffix(self) -> &'static str {
        match self {
            Self::CompatPending => "-compat-pending",
            Self::ImportPending => "-import-pending",
            Self::ResetAll => "-reset-all",
        }
    }

    pub const fn lifetime(self) -> Lifetime {
        match self {
            Self::CompatPending | Self::ImportPending => Lifetime::Pending,
            Self::ResetAll => Lifetime::Permanent,
        }
    }
}

impl fmt::Display for Marker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.suffix().trim_start_matches('-'))
    }
}

/// A pending fresh-transition import: the immutable archived source session and the session that imports its day
/// records (`docs/spec/journal.md`, Transitions).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImportMarker {
    pub source: SessionId,
    pub destination: SessionId,
}

/// Why a session's journal leaves the state directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveKind {
    /// A new session will import this session's day records. A pending import from an earlier session keeps that
    /// session as its source and keeps its destination unless the destination is the session being archived.
    Transition { destination: SessionId },
    /// The session and everything before it is discarded for good.
    ResetAll,
}

#[derive(Deserialize)]
struct Peek {
    kind: String,
    #[serde(default, deserialize_with = "serde_object::deserialize")]
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
        list_archive(self.fs(), &self.archive_dir())
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
        markers_in(self.fs(), self.path())
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

    /// The newest pending import: the source whose day records no journal has committed yet, and the session that
    /// imports them.
    pub fn pending_import(&self) -> Result<Option<ImportMarker>, DirError> {
        let Some(source) = self.marked(Marker::ImportPending)?.pop() else {
            return Ok(None);
        };
        let path = self.marker_path(Marker::ImportPending, &source);
        let bytes = self.fs().read(&path).map_err(io_at("read marker", &path))?;
        let destination = std::str::from_utf8(&bytes)
            .ok()
            .and_then(|text| SessionId::from_str(text.trim_end()).ok())
            .ok_or(DirError::BadMarker { path })?;
        Ok(Some(ImportMarker {
            source,
            destination,
        }))
    }

    /// The raw bytes of an archived journal; `None` when the directory holds no archive of the session.
    pub fn archived_raw(&self, session: &SessionId) -> Result<Option<Vec<u8>>, DirError> {
        let path = self.archive_journal(session);
        match self.fs().read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if not_found(&error) => Ok(None),
            Err(error) => Err(io_at("read archive", &path)(error)),
        }
    }

    /// The id a session importing `source`'s day records gets: the first free id for `now` that sorts after the
    /// source, every archive, every marker and the reset boundary, so a clock set back cannot sort it before them.
    pub fn import_destination(
        &self,
        source: &SessionId,
        now: jiff::Timestamp,
    ) -> Result<SessionId, DirError> {
        let floor = self
            .archives()?
            .into_iter()
            .chain(self.markers()?.into_iter().map(|(_, session)| session))
            .chain([*source])
            .max()
            .unwrap_or(*source);
        let mut candidate = self.session_id(now)?;
        if candidate <= floor {
            candidate = floor
                .next_suffix()
                .ok_or(DirError::SessionIdsExhausted { after: floor })?;
        }
        while self.id_reserved(&candidate)? {
            candidate = candidate
                .next_suffix()
                .ok_or(DirError::SessionIdsExhausted { after: candidate })?;
        }
        Ok(candidate)
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

    /// Discards the pending import: the reset boundary becomes durable before the import marker goes.
    pub fn drop_pending_import(&mut self) -> Result<Option<SessionId>, DirError> {
        let Some(dropped) = self.pending_import()? else {
            return Ok(None);
        };
        self.mark_reset_through(Some(dropped.source))?;
        self.clear_pending(Marker::ImportPending)?;
        Ok(Some(dropped.source))
    }

    /// Writes the import marker for `source` naming `destination`, replacing the one an earlier attempt left: the
    /// content reaches a temporary file, is synced, then renamed over the marker, so the marker is never seen empty,
    /// and both directories are synced before anything moves behind it.
    fn mark_import(&self, source: &SessionId, destination: &SessionId) -> Result<(), DirError> {
        self.ensure_archive_dir()?;
        let path = self.marker_path(Marker::ImportPending, source);
        if self
            .kind_of(&path)?
            .is_some_and(|kind| kind != FileKind::File)
        {
            return Err(DirError::NotRegular { path });
        }
        let tmp = self.archive_dir().join(IMPORT_TMP);
        let mut file = self
            .fs()
            .create_truncate(&tmp)
            .map_err(io_at("create marker", &tmp))?;
        file.write_all(format!("{destination}\n").as_bytes())
            .map_err(io_at("write marker", &tmp))?;
        file.sync().map_err(io_at("sync marker", &tmp))?;
        drop(file);
        self.fs()
            .rename(&tmp, &path)
            .map_err(io_at("publish marker", &path))?;
        self.sync_dirs()
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

    /// The marker a transaction of this kind leaves behind before anything is moved. A marker an earlier attempt left
    /// is written and synced again, so nothing moves behind a record that may not be durable.
    pub(crate) fn mark_archive_kind(
        &self,
        kind: ArchiveKind,
        session: &SessionId,
    ) -> Result<(), DirError> {
        match kind {
            ArchiveKind::ResetAll => self.mark_reset_through(Some(*session)).map(drop),
            ArchiveKind::Transition { destination } => {
                let source = self
                    .pending_import()?
                    .map_or(*session, |marker| marker.source);
                self.mark_import(&source, &destination)
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

    /// Completes compatibility archives under the writer lock without opening a replacement journal. Returns the
    /// first pending session, or `None` when no compatibility archive is pending.
    pub fn complete_pending_archive(&mut self) -> Result<Option<SessionId>, DirError> {
        let Some(session) = self.marked(Marker::CompatPending)?.into_iter().next() else {
            return Ok(None);
        };
        let events = self.join(EVENTS_FILE);
        if self.kind_of(&events)?.is_some() && first_line(self.fs(), &events)?.is_none() {
            self.fs()
                .remove_file(&events)
                .map_err(io_at("remove", &events))?;
            self.fs()
                .sync_dir(self.path())
                .map_err(io_at("sync", self.path()))?;
        }
        self.recover_compat()?;
        Ok(Some(session))
    }

    /// Finishes every archive a compat marker left pending, then removes the marker.
    pub(crate) fn recover_compat(&self) -> Result<(), DirError> {
        for session in self.marked(Marker::CompatPending)? {
            let events = self.join(EVENTS_FILE);
            if self.kind_of(&events)?.is_some() {
                if self.live_session()? != Some(session) {
                    return Err(DirError::NotAJournal { path: events });
                }
                self.mark(Marker::CompatPending, &session)?;
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

    /// An import marker for the session that still owns the live journal is a leftover of an archive that never
    /// started.
    pub(crate) fn drop_stale_import(&self, live: &SessionId) -> Result<(), DirError> {
        if self.marked(Marker::ImportPending)?.contains(live) {
            self.remove_marker(Marker::ImportPending, live)?;
        }
        Ok(())
    }

    /// The session the live journal belongs to, read from its first complete line; `None` without one. A first line
    /// that is not a `session.start` is another build's journal.
    pub fn live_session(&self) -> Result<Option<SessionId>, DirError> {
        let path = self.join(EVENTS_FILE);
        let Some((line, complete)) = first_line(self.fs(), &path)? else {
            return Ok(None);
        };
        if !complete {
            return match header::classify(&line) {
                Some(_) => Ok(None),
                None => Err(DirError::ForeignJournal { path }),
            };
        }
        match peek_session(&line, "session.start") {
            Some(Ok(session)) => Ok(Some(session)),
            Some(Err(source)) => Err(DirError::BadSession { path, source }),
            None => Err(DirError::ForeignJournal { path }),
        }
    }

    /// The build stamp of the live journal's `session.start`, decoded from its first complete line alone, so a
    /// journal whose later events this build cannot decode still has one; `None` without a live journal.
    pub fn live_build(&self) -> Result<Option<Build>, DirError> {
        recorded_build_in(self.fs(), self.path())
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

fn list_archive(fs: &dyn Fs, dir: &Path) -> Result<Vec<crate::fs::DirEntry>, DirError> {
    match fs.read_dir(dir) {
        Ok(entries) => Ok(entries),
        Err(error) if not_found(&error) => Ok(Vec::new()),
        Err(error) => Err(io_at("list archive", dir)(error)),
    }
}

/// The typed markers in `dir`'s archive, read without taking the writer lock.
pub fn markers(dir: &Path) -> Result<Vec<(Marker, SessionId)>, DirError> {
    markers_in(&DiskFs::default(), dir)
}

/// The typed markers in `dir`'s archive, ordered by session then kind, without taking the writer lock.
/// A marker that is not a regular file is an error.
pub fn markers_in(fs: &dyn Fs, dir: &Path) -> Result<Vec<(Marker, SessionId)>, DirError> {
    let archive = dir.join(ARCHIVE_DIR);
    let mut markers = Vec::new();
    for entry in list_archive(fs, &archive)? {
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
                path: archive.join(&entry.name),
            });
        }
        markers.push(found);
    }
    markers.sort_by_key(|(marker, session)| (*session, *marker));
    Ok(markers)
}

/// The names of the entries in the state directory `dir`'s `archive/` that belong to a session, sorted: archived
/// journals and markers, each named after a valid session id. Read without taking the lock or writing; an absent
/// archive holds none. An entry so named that is not a regular file is an error naming it.
pub fn session_entries_in(fs: &dyn Fs, dir: &Path) -> Result<Vec<OsString>, DirError> {
    let archive = dir.join(ARCHIVE_DIR);
    let mut names = Vec::new();
    for entry in list_archive(fs, &archive)? {
        let Some(name) = entry.name.to_str() else {
            continue;
        };
        let names_session = Marker::ALL
            .map(Marker::suffix)
            .into_iter()
            .chain([JOURNAL_SUFFIX])
            .filter_map(|suffix| name.strip_suffix(suffix))
            .any(|session| session.parse::<SessionId>().is_ok());
        if !names_session {
            continue;
        }
        if entry.kind != FileKind::File {
            return Err(DirError::NotRegular {
                path: archive.join(&entry.name),
            });
        }
        names.push(entry.name);
    }
    names.sort();
    Ok(names)
}

/// The first line of the journal at `path` without its newline, and whether the newline was there; `None` when the
/// journal is missing or has no byte.
fn first_line(fs: &dyn Fs, path: &Path) -> Result<Option<(Vec<u8>, bool)>, DirError> {
    let reader = match fs.open_read(path) {
        Ok(reader) => reader,
        Err(error) if not_found(&error) => return Ok(None),
        Err(error) => return Err(io_at("read journal", path)(error)),
    };
    let mut line = Vec::new();
    BufReader::new(reader)
        .read_until(b'\n', &mut line)
        .map_err(io_at("read journal", path))?;
    if line.is_empty() {
        return Ok(None);
    }
    let complete = line.last() == Some(&b'\n');
    if complete {
        line.pop();
    }
    Ok(Some((line, complete)))
}

/// The build stamp of the live journal in the state directory `dir`, decoded from its first complete line alone,
/// without taking the lock or writing; `None` when there is no journal or no complete first line. A first line,
/// complete or not, that is not a journal header is another build's journal, and no event after it is read.
pub fn recorded_build(dir: &Path) -> Result<Option<Build>, DirError> {
    recorded_build_in(&DiskFs::default(), dir)
}

/// [`recorded_build`] over `fs`.
pub fn recorded_build_in(fs: &dyn Fs, dir: &Path) -> Result<Option<Build>, DirError> {
    let path = dir.join(EVENTS_FILE);
    let Some((line, complete)) = first_line(fs, &path)? else {
        return Ok(None);
    };
    match header::classify(&line) {
        Some(build) => Ok(complete.then_some(build)),
        None => Err(DirError::ForeignJournal { path }),
    }
}

fn peek_session(
    line: &[u8],
    kind: &str,
) -> Option<Result<SessionId, crate::session_id::InvalidSessionId>> {
    let Object(peek) = serde_json::from_slice::<Object<Peek>>(line).ok()?;
    if peek.kind != kind {
        return None;
    }
    Some(SessionId::from_str(&peek.data.session?))
}
