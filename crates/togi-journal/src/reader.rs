//! Reading journals without writing them: a streaming reader, a whole-file recording and the lookups over them.

use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::dir::{DirError, EVENTS_FILE, StateDir, not_found};
use crate::event::{Event, Seq};
use crate::fs::{DiskFs, Fs};
use crate::payload::{EventKind, Payload, SessionContext};

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("read: {0}")]
    Io(#[from] io::Error),
    #[error("line {line}: {source}")]
    Decode {
        line: u64,
        #[source]
        source: serde_json::Error,
    },
    #[error("line {line}: expected seq {expected}, found {found}")]
    Gap { line: u64, expected: u64, found: Seq },
    #[error("line 1: the first event is {}, not session.start", found.as_str())]
    FirstEvent { found: EventKind },
    #[error("line {line}: cause {cause} does not precede seq {seq}")]
    Cause { line: u64, seq: Seq, cause: Seq },
}

#[derive(Debug, thiserror::Error)]
#[error("{}: {source}", path.display())]
pub struct FileError {
    pub path: PathBuf,
    #[source]
    pub source: ReadError,
}

#[derive(Debug, thiserror::Error)]
pub enum LookupError {
    #[error(transparent)]
    Dir(#[from] DirError),
    #[error(transparent)]
    File(#[from] FileError),
}

/// Streams the complete lines of a journal as events, checking what a journal promises: seqs count up from 1, the
/// first event is `session.start`, and every cause precedes its event. Bytes after the last newline are a torn tail,
/// never an event. The first error ends the stream.
#[derive(Debug)]
pub struct EventReader<R> {
    reader: R,
    line: Vec<u8>,
    lines: u64,
    accepted: u64,
    torn: Option<Vec<u8>>,
    done: bool,
}

impl<R: BufRead> EventReader<R> {
    pub const fn new(reader: R) -> Self {
        Self { reader, line: Vec::new(), lines: 0, accepted: 0, torn: None, done: false }
    }

    /// The bytes of the complete lines accepted so far; the end of the last event.
    pub const fn accepted(&self) -> u64 {
        self.accepted
    }

    /// The unterminated bytes that ended the stream, once it has.
    pub fn torn(&self) -> Option<&[u8]> {
        self.torn.as_deref()
    }

    fn decode(&mut self) -> Result<Event, ReadError> {
        self.lines += 1;
        let line = self.lines;
        let event: Event = serde_json::from_slice(&self.line[..self.line.len() - 1])
            .map_err(|source| ReadError::Decode { line, source })?;
        let expected = line;
        if event.seq.get() != expected {
            return Err(ReadError::Gap { line, expected, found: event.seq });
        }
        if line == 1 && !matches!(event.payload, Payload::SessionStart(_)) {
            return Err(ReadError::FirstEvent { found: event.payload.kind() });
        }
        if let Some(&cause) = event.cause.iter().find(|cause| cause.get() >= event.seq.get()) {
            return Err(ReadError::Cause { line, seq: event.seq, cause });
        }
        Ok(event)
    }
}

impl<R: BufRead> Iterator for EventReader<R> {
    type Item = Result<Event, ReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        self.line.clear();
        match self.reader.read_until(b'\n', &mut self.line) {
            Err(error) => {
                self.done = true;
                return Some(Err(error.into()));
            }
            Ok(0) => {
                self.done = true;
                return None;
            }
            Ok(_) => {}
        }
        if self.line.last() != Some(&b'\n') {
            self.done = true;
            self.torn = Some(std::mem::take(&mut self.line));
            return None;
        }
        let event = self.decode();
        match &event {
            Ok(_) => self.accepted += self.line.len() as u64,
            Err(_) => self.done = true,
        }
        Some(event)
    }
}

/// Where a journal ends mid-line: the byte offset of the first unterminated byte, and those bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Torn {
    pub offset: u64,
    pub bytes: Vec<u8>,
}

/// A journal read whole, without taking the lock and without writing anything.
#[derive(Clone, Debug)]
pub struct Recording {
    events: Vec<Event>,
    torn: Option<Torn>,
}

impl Recording {
    pub fn read(path: &Path) -> Result<Self, FileError> {
        Self::read_in(&DiskFs::default(), path)
    }

    pub fn read_in(fs: &dyn Fs, path: &Path) -> Result<Self, FileError> {
        let file_error = |source: ReadError| FileError { path: path.to_path_buf(), source };
        let bytes = fs.read(path).map_err(|error| file_error(error.into()))?;
        Self::parse(&bytes).map_err(file_error)
    }

    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, ReadError> {
        let mut reader = EventReader::new(bytes);
        let events = reader.by_ref().collect::<Result<Vec<_>, _>>()?;
        let torn = reader.torn().map(|tail| Torn { offset: reader.accepted(), bytes: tail.to_vec() });
        Ok(Self { events, torn })
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    pub fn event(&self, seq: Seq) -> Option<&Event> {
        self.events.get(usize::try_from(seq.get() - 1).ok()?)
    }

    pub const fn torn(&self) -> Option<&Torn> {
        self.torn.as_ref()
    }

    pub(crate) fn into_parts(self) -> (Vec<Event>, Option<Torn>) {
        (self.events, self.torn)
    }
}

fn find_in<T>(
    fs: &dyn Fs,
    path: &Path,
    mut pick: impl FnMut(Event) -> Option<T>,
) -> Result<Option<T>, FileError> {
    let file_error = |source: ReadError| FileError { path: path.to_path_buf(), source };
    let file = match fs.open_read(path) {
        Ok(file) => file,
        Err(error) if not_found(&error) => return Ok(None),
        Err(error) => return Err(file_error(error.into())),
    };
    for event in EventReader::new(BufReader::new(file)) {
        if let Some(found) = pick(event.map_err(file_error)?) {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

/// The BIOS context the journal at `path` recorded first; `None` for a missing journal or one that never did.
pub fn recorded_context(path: &Path) -> Result<Option<SessionContext>, FileError> {
    recorded_context_in(&DiskFs::default(), path)
}

pub fn recorded_context_in(fs: &dyn Fs, path: &Path) -> Result<Option<SessionContext>, FileError> {
    find_in(fs, path, |event| match event.payload {
        Payload::SessionContext(context) => Some(context),
        _ => None,
    })
}

fn acknowledges(event: &Event, reason_id: &str) -> bool {
    matches!(&event.payload, Payload::BootLeaveReason(leave) if leave.reason_id == reason_id)
}

impl StateDir {
    /// Whether the live journal or any archived one acknowledged the boot leave reason, newest first. Only complete
    /// lines count.
    pub fn boot_reason_recorded(&self, reason_id: &str) -> Result<bool, LookupError> {
        if find_in(self.fs(), &self.join(EVENTS_FILE), |event| acknowledges(&event, reason_id).then_some(()))?
            .is_some()
        {
            return Ok(true);
        }
        self.archived_boot_reason(reason_id)
    }

    pub(crate) fn archived_boot_reason(&self, reason_id: &str) -> Result<bool, LookupError> {
        for session in self.archives()?.iter().rev() {
            let path = self.archive_journal(session);
            if find_in(self.fs(), &path, |event| acknowledges(&event, reason_id).then_some(()))?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests;
