use std::fs;
use std::io;
use std::path::Path;

use jiff::Timestamp;
use serde::Serialize;
use sha2::{Digest, Sha256};
use togi_journal::fs::{FileKind, Fs};
use togi_journal::{
    Build, Event, EventReader, FileError, FixStamp, Operation, Payload, ReadError, Refusal, SCHEMA,
    Seq, SessionId, Skipped, Verdict,
};
use togi_machine::object_only;
use togi_tuner::{EVIDENCE_EPOCH, RULESET};

use crate::error::Error;

const ARCHIVE_DIR: &str = "archive";
const LIVE_JOURNAL: &str = "events.jsonl";
const JOURNAL_SUFFIX: &str = ".jsonl";

object_only! {
    /// The last event of the copied live journal; a forecast describes only what follows it.
    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Anchor {
        pub session: SessionId,
        pub seq: Seq,
        pub time: Timestamp,
        /// SHA-256, in lower-case hex, of the anchor session's journal lines through the anchor event, newlines
        /// included, exactly as the file holds them.
        pub sha256: String,
    }
}

impl Anchor {
    /// The anchor of a copied state directory: the last complete event of its live journal. It reads the
    /// journal without locking or writing the directory.
    pub fn of(dir: impl AsRef<Path>) -> Result<Self, Error> {
        let dir = dir.as_ref();
        Self::of_session(dir, Session::read(&dir.join(LIVE_JOURNAL))?)
    }

    /// Reads the anchor through an explicitly supplied filesystem, without locking or writing it.
    pub fn of_fs(fs: &dyn Fs, dir: impl AsRef<Path>) -> Result<Self, Error> {
        let dir = dir.as_ref();
        Self::of_session(
            dir,
            Session::read_with_fs(&dir.join(LIVE_JOURNAL), Reading::Strict, Some(fs))?,
        )
    }

    fn of_session(dir: &Path, session: Option<Session>) -> Result<Self, Error> {
        let session = session.ok_or_else(|| Error::NoEvents {
            dir: dir.to_path_buf(),
        })?;
        Ok(session.anchor_through(session.events.len() - 1))
    }

    /// The index of the anchor's session among `sessions` and of the anchor event in it, with the journal
    /// through the anchor checked against the hash.
    pub(crate) fn find(&self, sessions: &[Session]) -> Result<(usize, usize), Error> {
        let session = self.session;
        let (index, held) = sessions
            .iter()
            .enumerate()
            .find(|(_, held)| held.id == session)
            .ok_or(Error::NoSession { session })?;
        let at = held
            .events
            .iter()
            .position(|event| event.seq == self.seq)
            .ok_or(Error::NoEvent {
                session,
                seq: self.seq,
            })?;
        if hex(&held.prefixes[at]) != self.sha256 {
            return Err(Error::ChangedAnchor {
                session,
                seq: self.seq,
            });
        }
        Ok((index, at))
    }
}

/// One session's journal.
#[derive(Clone, Debug)]
pub struct Session {
    id: SessionId,
    build: Build,
    events: Vec<Event>,
    prefixes: Vec<[u8; 32]>,
    refusal: Option<Refusal>,
    skipped: Skipped,
}

/// How a journal is decoded: strictly, or leniently as `journal.md` describes for read-only consumers.
#[derive(Clone, Copy)]
enum Reading {
    Strict,
    Lenient,
}

impl Session {
    pub const fn id(&self) -> SessionId {
        self.id
    }

    /// The build that started the session.
    pub const fn build(&self) -> &Build {
        &self.build
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// What a lenient read of the journal skipped or ignored; empty after a strict one.
    pub const fn skipped(&self) -> &Skipped {
        &self.skipped
    }

    /// Whether this build's tuner can replay the session: the journal's schema is not newer and its ruleset is
    /// this build's ([`togi_tuner::RULESET`]). A session that cannot be replayed can still be counted over, but
    /// not folded, so it cannot hold a forecast's terminal point or supply its profile.
    pub const fn replayable(&self) -> bool {
        self.refusal.is_none()
    }

    /// Why this build's tuner cannot replay the session, if it cannot.
    pub(crate) const fn refusal(&self) -> Option<Refusal> {
        self.refusal
    }

    /// A session of events held in memory, as the journal would write them: each line is the event's own
    /// encoding. The first event must be `session.start`, which the journal reader guarantees of a file.
    pub fn from_events(events: Vec<Event>) -> Result<Self, Error> {
        let mut bytes = Vec::new();
        let mut ends = Vec::with_capacity(events.len());
        for event in &events {
            serde_json::to_writer(&mut bytes, event).map_err(|source| Error::Encode {
                seq: event.seq,
                source,
            })?;
            bytes.push(b'\n');
            ends.push(bytes.len());
        }
        Self::new(events, &bytes, &ends, Skipped::default()).ok_or(Error::NotStarted)
    }

    /// The session a journal holds; `None` for a journal without a complete line. Bytes after the last newline
    /// are a torn tail and never an event.
    pub fn read(path: &Path) -> Result<Option<Self>, Error> {
        Self::read_with(path, Reading::Strict)
    }

    /// [`read`](Self::read) skipping events of kinds this build does not know and ignoring unknown fields;
    /// [`skipped`](Self::skipped) says what it did.
    pub fn read_lenient(path: &Path) -> Result<Option<Self>, Error> {
        Self::read_with(path, Reading::Lenient)
    }

    fn read_with(path: &Path, reading: Reading) -> Result<Option<Self>, Error> {
        Self::read_with_fs(path, reading, None)
    }

    fn read_with_fs(
        path: &Path,
        reading: Reading,
        fs: Option<&dyn Fs>,
    ) -> Result<Option<Self>, Error> {
        let bytes = match fs {
            Some(fs) => fs.read(path),
            None => fs::read(path),
        }
        .map_err(|source| Error::ReadJournal {
            path: path.to_path_buf(),
            source,
        })?;
        Self::decode_with(path, &bytes, reading)
    }

    /// Decodes a journal's original bytes, retaining their exact prefix hashes.
    /// `path` labels errors and need not exist.
    pub fn decode(path: &Path, bytes: &[u8]) -> Result<Option<Self>, Error> {
        Self::decode_with(path, bytes, Reading::Strict)
    }

    fn decode_with(path: &Path, bytes: &[u8], reading: Reading) -> Result<Option<Self>, Error> {
        Self::parse(bytes, reading).map_err(|source| {
            Error::Journal(FileError {
                path: path.to_path_buf(),
                source,
            })
        })
    }

    fn parse(bytes: &[u8], reading: Reading) -> Result<Option<Self>, ReadError> {
        let mut reader = match reading {
            Reading::Strict => EventReader::new(bytes),
            Reading::Lenient => EventReader::lenient(bytes),
        };
        let mut events = Vec::new();
        let mut ends = Vec::new();
        while let Some(event) = reader.next() {
            events.push(event?);
            ends.push(usize::try_from(reader.accepted()).unwrap_or(usize::MAX));
        }
        let Some(first) = events.first() else {
            return Ok(None);
        };
        let found = first.kind();
        Self::new(events, bytes, &ends, reader.skipped().clone())
            .map(Some)
            .ok_or(ReadError::FirstEvent { found })
    }

    fn new(events: Vec<Event>, bytes: &[u8], ends: &[usize], skipped: Skipped) -> Option<Self> {
        let Some(Payload::SessionStart(start)) = events.first().map(|event| &event.payload) else {
            return None;
        };
        let (id, build) = (start.session, start.build.clone());
        let mut hasher = Sha256::new();
        let mut from = 0;
        let prefixes = ends
            .iter()
            .map(|&end| {
                hasher.update(&bytes[from..end]);
                from = end;
                hasher.clone().finalize().into()
            })
            .collect();
        let reader = Build {
            version: String::new(),
            rev: None,
            ruleset: RULESET,
            schema: SCHEMA,
            fixes: FixStamp::none(),
            evidence: EVIDENCE_EPOCH,
        };
        let refusal = match reader.verdict(&build, Operation::Replay) {
            Verdict::Refuse(refusal) => Some(refusal),
            Verdict::Use | Verdict::Archive => None,
        };
        Some(Self {
            id,
            build,
            events,
            prefixes,
            refusal,
            skipped,
        })
    }

    /// The anchor at event `seq` of the session; `None` when it has no such event.
    pub fn anchor_at(&self, seq: Seq) -> Option<Anchor> {
        let at = self.events.iter().position(|event| event.seq == seq)?;
        Some(self.anchor_through(at))
    }

    fn anchor_through(&self, at: usize) -> Anchor {
        Anchor {
            session: self.id,
            seq: self.events[at].seq,
            time: self.events[at].time,
            sha256: hex(&self.prefixes[at]),
        }
    }
}

/// The archived and live journals of a state directory, oldest session first, skipping archived sessions older
/// than `from`. It reads the directory without locking or writing it, so run it on a copy or while no run is
/// appending. An archived file whose name is not a session id is not a journal; a journal without a complete
/// line holds no session; a journal that fails the reader's checks is an error naming the file.
pub fn read_dir(dir: impl AsRef<Path>, from: Option<SessionId>) -> Result<Vec<Session>, Error> {
    read_dir_with(dir.as_ref(), from, Reading::Strict, None)
}

/// [`read_dir`] for a state directory nobody is resuming, such as an operator's: events of kinds this build does
/// not know are skipped and unknown fields ignored, and each session's [`skipped`](Session::skipped) says what
/// was.
pub fn read_dir_lenient(
    dir: impl AsRef<Path>,
    from: Option<SessionId>,
) -> Result<Vec<Session>, Error> {
    read_dir_with(dir.as_ref(), from, Reading::Lenient, None)
}

/// Reads archived and live journals through an explicitly supplied filesystem.
pub fn read_dir_fs(
    fs: &dyn Fs,
    dir: impl AsRef<Path>,
    from: Option<SessionId>,
) -> Result<Vec<Session>, Error> {
    read_dir_with(dir.as_ref(), from, Reading::Strict, Some(fs))
}

/// Lenient [`read_dir_fs`], preserving skipped events and original journal hashes.
pub fn read_dir_lenient_fs(
    fs: &dyn Fs,
    dir: impl AsRef<Path>,
    from: Option<SessionId>,
) -> Result<Vec<Session>, Error> {
    read_dir_with(dir.as_ref(), from, Reading::Lenient, Some(fs))
}

fn read_dir_with(
    dir: &Path,
    from: Option<SessionId>,
    reading: Reading,
    fs: Option<&dyn Fs>,
) -> Result<Vec<Session>, Error> {
    let archive = dir.join(ARCHIVE_DIR);
    let mut ids = Vec::new();
    let mut select = |name: std::ffi::OsString, is_file| {
        let id = name
            .to_str()
            .and_then(|name| name.strip_suffix(JOURNAL_SUFFIX)?.parse::<SessionId>().ok());
        if let (true, Some(id)) = (is_file, id) {
            ids.push(id);
        }
    };
    let listing = match fs {
        Some(fs) => fs.read_dir(&archive).map(|entries| {
            for entry in entries {
                select(entry.name, entry.kind == FileKind::File);
            }
        }),
        None => match fs::read_dir(&archive) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|source| Error::ListArchive {
                        path: archive.clone(),
                        source,
                    })?;
                    select(
                        entry.file_name(),
                        entry.file_type().is_ok_and(|kind| kind.is_file()),
                    );
                }
                Ok(())
            }
            Err(error) => Err(error),
        },
    };
    match listing {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(Error::ListArchive {
                path: archive,
                source,
            });
        }
    }
    ids.sort();
    let mut sessions = Vec::new();
    for id in ids
        .into_iter()
        .filter(|&id| from.is_none_or(|from| id >= from))
    {
        let path = archive.join(format!("{id}{JOURNAL_SUFFIX}"));
        sessions.extend(Session::read_with_fs(&path, reading, fs)?);
    }
    match Session::read_with_fs(&dir.join(LIVE_JOURNAL), reading, fs) {
        Ok(live) => sessions.extend(live),
        Err(Error::ReadJournal { source, .. }) if source.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(sessions)
}

fn hex(digest: &[u8; 32]) -> String {
    use std::fmt::Write;
    digest
        .iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}
