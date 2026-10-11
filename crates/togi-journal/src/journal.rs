//! The writer: every event is one write and one fsync before it is published.

mod open;

use std::fmt;
use std::io;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::dir::{ArchiveKind, DirError, StateDir, StateError};
use crate::event::{Event, Seq, Stamp};
use crate::fs::FsFile;
use crate::payload::{EventKind, Payload, SessionArchived, SmuCommand, SmuIntentRecord, SmuWriteRecord};
use crate::reader::{FileError, LookupError};
use crate::receipt::{JournalId, SmuIntentReceipt, SmuWritten};
use crate::session_id::SessionId;

const BUFFER_LIMIT: usize = 64 * 1024;

/// An I/O failure that made a journal unusable for writing.
#[derive(Debug)]
pub struct WriteFailure {
    pub op: &'static str,
    pub source: io::Error,
}

impl fmt::Display for WriteFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.op, self.source)
    }
}

impl std::error::Error for WriteFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AppendError {
    #[error("journal write failed: {0}")]
    Failed(#[source] Arc<WriteFailure>),
    #[error("journal refuses writes after an earlier failure: {0}")]
    Poisoned(#[source] Arc<WriteFailure>),
    #[error("{} is written only through its own append method", .0.as_str())]
    Refused(EventKind),
    #[error("the first event must be session.start, not {}", found.as_str())]
    FirstEvent { found: EventKind },
    #[error("cause {cause} does not precede seq {seq}")]
    Cause { seq: Seq, cause: Seq },
    #[error("the journal has no seq left")]
    SeqExhausted,
    #[error("encode event: {0}")]
    Encode(#[source] serde_json::Error),
    #[error("the event would not read back: {0}")]
    Unreadable(#[source] crate::DecodeError),
    #[error("the event would read back as a different payload")]
    Lossy,
    #[error("the receipt belongs to another journal")]
    ForeignReceipt,
}

#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    #[error(transparent)]
    Dir(#[from] DirError),
    #[error(transparent)]
    Read(#[from] FileError),
    #[error("the journal records the archive of {recorded} but belongs to {live}")]
    ArchiveMismatch { recorded: SessionId, live: SessionId },
    #[error("repair torn tail ({step}): {source}")]
    Repair {
        step: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("encode torn record: {0}")]
    Encode(#[source] serde_json::Error),
    #[error("the journal has no seq left")]
    SeqExhausted,
}

#[derive(Debug, thiserror::Error)]
pub enum CloseError {
    #[error("close journal: {0}")]
    Io(#[source] io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error(transparent)]
    Dir(#[from] DirError),
    #[error(transparent)]
    Append(#[from] AppendError),
    #[error("the journal has no session to archive")]
    NoSession,
    #[error("flush journal: {0}")]
    Flush(#[source] io::Error),
}

#[derive(Debug)]
struct Writer {
    file: Box<dyn FsFile>,
    buffer: Vec<u8>,
    buffered: bool,
}

impl Writer {
    fn write(&mut self, line: &[u8]) -> Result<(), WriteFailure> {
        if self.buffered {
            self.buffer.extend_from_slice(line);
            if self.buffer.len() >= BUFFER_LIMIT {
                self.flush()?;
            }
            return Ok(());
        }
        self.file.write_all(line).map_err(|source| WriteFailure { op: "write", source })?;
        self.file.sync().map_err(|source| WriteFailure { op: "sync", source })
    }

    fn flush(&mut self) -> Result<(), WriteFailure> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        self.file.write_all(&self.buffer).map_err(|source| WriteFailure { op: "write", source })?;
        self.buffer.clear();
        Ok(())
    }

    fn finish(&mut self) -> Result<(), WriteFailure> {
        self.flush()?;
        self.file.sync().map_err(|source| WriteFailure { op: "sync", source })
    }
}

/// The one writer of a state directory's live journal. A write or fsync failure poisons it: it refuses every later
/// append without touching the disk, and reopening the directory replays whatever survived.
#[derive(Debug)]
pub struct Journal {
    writer: Writer,
    events: Vec<Event>,
    boot: togi_machine::BootId,
    id: JournalId,
    repaired: Option<Seq>,
    session: Option<SessionId>,
    poison: Option<Arc<WriteFailure>>,
    dir: StateDir,
}

pub(crate) fn encode(event: &Event) -> Result<Vec<u8>, serde_json::Error> {
    let mut line = serde_json::to_vec(event)?;
    line.push(b'\n');
    Ok(line)
}

pub(crate) fn seq_after(last: Option<&Event>) -> Option<Seq> {
    last.map_or(Some(Seq::FIRST), |event| event.seq.next())
}

impl Journal {
    pub fn events(&self) -> &[Event] {
        &self.events
    }

    pub fn event(&self, seq: Seq) -> Option<&Event> {
        self.events.get(usize::try_from(seq.get() - 1).ok()?)
    }

    /// The seq of the `journal.torn` this open recorded, if the previous writer left a torn tail.
    pub const fn repaired(&self) -> Option<Seq> {
        self.repaired
    }

    pub const fn session(&self) -> Option<SessionId> {
        self.session
    }

    pub fn boot(&self) -> &togi_machine::BootId {
        &self.boot
    }

    pub const fn state_dir(&self) -> &StateDir {
        &self.dir
    }

    pub fn write_state<T: Serialize>(&mut self, state: &T) -> Result<(), StateError> {
        self.dir.write_state(state)
    }

    pub fn read_state<T: DeserializeOwned>(&self) -> Result<Option<T>, StateError> {
        self.dir.read_state()
    }

    /// Whether this journal or an archived one acknowledged the boot leave reason.
    pub fn boot_reason_recorded(&self, reason_id: &str) -> Result<bool, LookupError> {
        let live = self.events.iter().any(|event| {
            matches!(&event.payload, Payload::BootLeaveReason(leave) if leave.reason_id == reason_id)
        });
        if live { Ok(true) } else { self.dir.archived_boot_reason(reason_id) }
    }

    /// Appends one event: a single write of its whole line, an fsync, and only then it is published to `events()`.
    pub fn append(&mut self, stamp: Stamp, payload: Payload, cause: &[Seq]) -> Result<&Event, AppendError> {
        if let Payload::SmuIntent(_) | Payload::SmuWrite(_) = payload {
            return Err(AppendError::Refused(payload.kind()));
        }
        self.commit(stamp, payload, cause)
    }

    /// Records the intent to run `command` and returns the receipt that authorizes writing it.
    pub fn append_smu_intent(
        &mut self,
        stamp: Stamp,
        command: SmuCommand,
        cause: &[Seq],
    ) -> Result<SmuIntentReceipt, AppendError> {
        let seq = self.commit(stamp, Payload::SmuIntent(SmuIntentRecord::new(command)), cause)?.seq;
        Ok(SmuIntentReceipt::new(self.id, seq, command))
    }

    /// Records that the command the receipt authorizes was written. The receipt is spent whatever the outcome.
    pub fn append_smu_write(&mut self, stamp: Stamp, receipt: SmuIntentReceipt) -> Result<SmuWritten, AppendError> {
        let (journal, intent, command) = receipt.into_parts();
        if journal != self.id {
            return Err(AppendError::ForeignReceipt);
        }
        let seq = self.commit(stamp, Payload::SmuWrite(SmuWriteRecord::new(command)), &[intent])?.seq;
        Ok(SmuWritten::new(seq, intent, command))
    }

    fn commit(&mut self, stamp: Stamp, payload: Payload, cause: &[Seq]) -> Result<&Event, AppendError> {
        if let Some(failure) = &self.poison {
            return Err(AppendError::Poisoned(Arc::clone(failure)));
        }
        let seq = seq_after(self.events.last()).ok_or(AppendError::SeqExhausted)?;
        if seq.get() == 1 && !matches!(payload, Payload::SessionStart(_)) {
            return Err(AppendError::FirstEvent { found: payload.kind() });
        }
        if let Some(&cause) = cause.iter().find(|cause| cause.get() >= seq.get()) {
            return Err(AppendError::Cause { seq, cause });
        }
        let session = match &payload {
            Payload::SessionStart(start) => Some(start.session),
            _ => None,
        };
        let event = Event::new(seq, stamp, self.boot.clone(), cause.to_vec(), payload);
        let line = encode(&event).map_err(AppendError::Encode)?;
        std::str::from_utf8(&line[..line.len() - 1])
            .map_err(|error| AppendError::Encode(serde::ser::Error::custom(error)))
            .and_then(|text| Event::decode(text).map_err(AppendError::Unreadable))
            .and_then(|decoded| if decoded.payload == event.payload { Ok(()) } else { Err(AppendError::Lossy) })?;
        if let Err(failure) = self.writer.write(&line) {
            let failure = Arc::new(failure);
            self.poison = Some(Arc::clone(&failure));
            return Err(AppendError::Failed(failure));
        }
        self.session = self.session.or(session);
        self.events.push(event);
        Ok(&self.events[self.events.len() - 1])
    }

    /// Writes any buffered events to the file; a no-op for a journal that is not buffered.
    pub fn flush(&mut self) -> Result<(), AppendError> {
        if let Some(failure) = &self.poison {
            return Err(AppendError::Poisoned(Arc::clone(failure)));
        }
        self.writer.flush().map_err(|failure| {
            let failure = Arc::new(failure);
            self.poison = Some(Arc::clone(&failure));
            AppendError::Failed(failure)
        })
    }

    /// Syncs the journal and hands back the directory with its lock still held. A poisoned journal does no I/O: the
    /// directory is returned so it can be reopened and replayed.
    pub fn close(mut self) -> Result<StateDir, CloseError> {
        if self.poison.is_none() {
            self.writer.finish().map_err(|failure| CloseError::Io(failure.source))?;
        }
        Ok(self.dir)
    }

    /// Ends the session: the marker the kind needs, then a durable `session.archived`, then the state file, the trial
    /// directory and the journal move into `archive/`. The writer is spent; a failure after the event is recorded is
    /// finished by the next open of the directory.
    pub fn archive(mut self, stamp: Stamp, kind: ArchiveKind, cause: &[Seq]) -> Result<StateDir, ArchiveError> {
        let session = self.session.ok_or(ArchiveError::NoSession)?;
        if let Some(failure) = &self.poison {
            return Err(AppendError::Poisoned(Arc::clone(failure)).into());
        }
        self.dir.check_archive_free(&session)?;
        self.dir.mark_archive_kind(kind, &session)?;
        self.commit(stamp, Payload::SessionArchived(SessionArchived { session }), cause)?;
        self.writer.finish().map_err(|failure| ArchiveError::Flush(failure.source))?;
        let Self { writer, dir, .. } = self;
        drop(writer);
        dir.finish_archive(&session)?;
        Ok(dir)
    }
}

#[cfg(test)]
pub(crate) mod tests;
