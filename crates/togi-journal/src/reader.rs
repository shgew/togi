//! Reading journals without writing them: a streaming reader, a whole-file recording and the lookups over them.

use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::dir::{DirError, EVENTS_FILE, StateDir, not_found};
use crate::event::{DecodeError, Decoded, Decoding, Event, Read, Seq, decode};
use crate::fs::{DiskFs, Fs};
use crate::header;
use crate::payload::{EventKind, Payload, SessionContext, UnknownKind};
use crate::skipped::Skipped;

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("read")]
    Io(#[from] io::Error),
    #[error("line {line}")]
    Decode {
        line: u64,
        #[source]
        source: DecodeError,
    },
    #[error("line {line}")]
    Utf8 {
        line: u64,
        #[source]
        source: std::str::Utf8Error,
    },
    #[error("line {line}: expected seq {expected}, found {found}")]
    Gap {
        line: u64,
        expected: u64,
        found: Seq,
    },
    #[error("line {line}")]
    UnknownKind {
        line: u64,
        #[source]
        source: UnknownKind,
    },
    #[error(
        "first line is not a complete header of this build; either another build's journal or a header torn by a crash; see docs/howto.md#go-to-rust-cutover"
    )]
    Foreign,
    #[error("line {line}: {kind} event: unknown field {field:?}")]
    UnknownField {
        line: u64,
        kind: EventKind,
        /// Where the field sits in the line: `extra` in the envelope, `data.extra` in the payload.
        field: String,
    },
    #[error("line 1: the first event is {}, not session.start", found.as_str())]
    FirstEvent { found: EventKind },
    #[error("line {line}: cause {cause} does not precede seq {seq}")]
    Cause { line: u64, seq: Seq, cause: Seq },
}

#[derive(Debug, thiserror::Error)]
#[error("{}", path.display())]
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

impl ReadError {
    fn from_decode(line: u64, error: DecodeError) -> Self {
        match error {
            DecodeError::UnknownKind(source) => Self::UnknownKind { line, source },
            DecodeError::UnknownField { kind, field } => Self::UnknownField { line, kind, field },
            error @ (DecodeError::Envelope { .. } | DecodeError::Payload { .. }) => Self::Decode {
                line,
                source: error,
            },
        }
    }
}

/// Streams the complete lines of a journal as events, checking what a journal promises: seqs count up from 1, the
/// first event is `session.start`, and every cause precedes its event. Bytes after the last newline are a torn tail,
/// never an event. The first error ends the stream.
///
/// [`new`](Self::new) reads strictly: an event of a kind this build does not know, or with a field it does not know,
/// is an error. [`lenient`](Self::lenient) skips the former and ignores the latter, and [`skipped`](Self::skipped)
/// says what it did; every other fault is still an error. A skipped event still has its place in the seq order.
#[derive(Debug)]
pub struct EventReader<R> {
    reader: R,
    decoding: Decoding,
    line: Vec<u8>,
    lines: u64,
    accepted: u64,
    torn: Option<Vec<u8>>,
    skipped: Skipped,
    done: bool,
}

impl<R: BufRead> EventReader<R> {
    pub const fn new(reader: R) -> Self {
        Self::with(reader, Decoding::Strict)
    }

    pub const fn lenient(reader: R) -> Self {
        Self::with(reader, Decoding::Lenient)
    }

    const fn with(reader: R, decoding: Decoding) -> Self {
        Self {
            reader,
            decoding,
            line: Vec::new(),
            lines: 0,
            accepted: 0,
            torn: None,
            skipped: Skipped::new(),
            done: false,
        }
    }

    /// The bytes of the complete lines accepted so far; the end of the last event.
    pub const fn accepted(&self) -> u64 {
        self.accepted
    }

    /// The unterminated bytes that ended the stream, once it has.
    pub fn torn(&self) -> Option<&[u8]> {
        self.torn.as_deref()
    }

    /// What a lenient reader has skipped or ignored so far; always empty for a strict one.
    pub const fn skipped(&self) -> &Skipped {
        &self.skipped
    }

    /// The next line as an event; `None` for a skipped one.
    fn decode(&mut self) -> Result<Option<Event>, ReadError> {
        self.lines += 1;
        let line = self.lines;
        let bytes = &self.line[..self.line.len() - 1];
        // The first line is classified before anything decodes it, strict or lenient: a line that is not a complete
        // header of this build is refused, never read as an unknown kind or an unknown field.
        if line == 1 && header::classify(bytes).is_none() {
            return Err(ReadError::Foreign);
        }
        let text = std::str::from_utf8(bytes).map_err(|source| ReadError::Utf8 { line, source })?;
        let Read { decoded, ignored } =
            decode(text, self.decoding).map_err(|error| ReadError::from_decode(line, error))?;
        match decoded {
            Decoded::Event(event) => {
                check_seq(line, event.seq)?;
                check_causes(line, event.seq, &event.cause)?;
                self.skipped
                    .ignore(line, event.seq, event.kind().as_str(), ignored);
                Ok(Some(event))
            }
            Decoded::Unrecognized(unrecognized) => {
                check_seq(line, unrecognized.seq)?;
                check_causes(line, unrecognized.seq, &unrecognized.cause)?;
                self.skipped
                    .ignore(line, unrecognized.seq, &unrecognized.kind, ignored);
                self.skipped.skip(line, unrecognized.seq, unrecognized.kind);
                Ok(None)
            }
        }
    }
}

fn check_seq(line: u64, seq: Seq) -> Result<(), ReadError> {
    if seq.get() == line {
        return Ok(());
    }
    Err(ReadError::Gap {
        line,
        expected: line,
        found: seq,
    })
}

fn check_causes(line: u64, seq: Seq, causes: &[Seq]) -> Result<(), ReadError> {
    match causes.iter().find(|cause| cause.get() >= seq.get()) {
        Some(&cause) => Err(ReadError::Cause { line, seq, cause }),
        None => Ok(()),
    }
}

impl<R: BufRead> Iterator for EventReader<R> {
    type Item = Result<Event, ReadError>;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.done {
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
                if self.lines == 0 && header::classify(&self.line).is_none() {
                    return Some(Err(ReadError::Foreign));
                }
                self.torn = Some(std::mem::take(&mut self.line));
                return None;
            }
            match self.decode() {
                Ok(event) => {
                    self.accepted += self.line.len() as u64;
                    if let Some(event) = event {
                        return Some(Ok(event));
                    }
                }
                Err(error) => {
                    self.done = true;
                    return Some(Err(error));
                }
            }
        }
        None
    }
}

/// Where a journal ends mid-line: the byte offset of the first unterminated byte, and those bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Torn {
    pub offset: u64,
    pub bytes: Vec<u8>,
}

/// A journal read whole, without taking the lock and without writing anything.
///
/// [`read`](Self::read) and [`read_in`](Self::read_in) read strictly. The `_lenient` readers skip events of kinds this
/// build does not know and ignore unknown fields, and [`skipped`](Self::skipped) says what they did. A skipped event
/// is not in [`events`](Self::events), so seqs there may skip numbers; [`event`](Self::event) finds events by seq
/// all the same.
#[derive(Clone, Debug)]
pub struct Recording {
    events: Vec<Event>,
    torn: Option<Torn>,
    skipped: Skipped,
}

impl Recording {
    pub fn read(path: &Path) -> Result<Self, FileError> {
        Self::read_in(&DiskFs::default(), path)
    }

    pub fn read_in(fs: &dyn Fs, path: &Path) -> Result<Self, FileError> {
        Self::read_with(fs, path, Decoding::Strict)
    }

    pub fn read_lenient(path: &Path) -> Result<Self, FileError> {
        Self::read_in_lenient(&DiskFs::default(), path)
    }

    pub fn read_in_lenient(fs: &dyn Fs, path: &Path) -> Result<Self, FileError> {
        Self::read_with(fs, path, Decoding::Lenient)
    }

    fn read_with(fs: &dyn Fs, path: &Path, decoding: Decoding) -> Result<Self, FileError> {
        let file_error = |source: ReadError| FileError {
            path: path.to_path_buf(),
            source,
        };
        let bytes = fs.read(path).map_err(|error| file_error(error.into()))?;
        Self::parse_with(&bytes, decoding).map_err(file_error)
    }

    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, ReadError> {
        Self::parse_with(bytes, Decoding::Strict)
    }

    pub(crate) fn parse_with(bytes: &[u8], decoding: Decoding) -> Result<Self, ReadError> {
        let mut reader = EventReader::with(bytes, decoding);
        let events = reader.by_ref().collect::<Result<Vec<_>, _>>()?;
        let torn = reader.torn().map(|tail| Torn {
            offset: reader.accepted(),
            bytes: tail.to_vec(),
        });
        Ok(Self {
            events,
            torn,
            skipped: reader.skipped,
        })
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    pub fn event(&self, seq: Seq) -> Option<&Event> {
        let skipped = self.skipped.events();
        let before = skipped.partition_point(|skipped| skipped.seq < seq);
        if skipped
            .get(before)
            .is_some_and(|skipped| skipped.seq == seq)
        {
            return None;
        }
        let index = usize::try_from(seq.get() - 1).ok()?.checked_sub(before)?;
        self.events.get(index)
    }

    pub const fn torn(&self) -> Option<&Torn> {
        self.torn.as_ref()
    }

    /// What a lenient read skipped or ignored; always empty after a strict one.
    pub const fn skipped(&self) -> &Skipped {
        &self.skipped
    }

    pub fn into_events(self) -> Vec<Event> {
        self.events
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
    let file_error = |source: ReadError| FileError {
        path: path.to_path_buf(),
        source,
    };
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
    find_in(fs, path, |event| {
        if let Payload::SessionContext(context) = event.payload {
            Some(context)
        } else {
            None
        }
    })
}

fn acknowledges(event: &Event, reason_id: &str) -> bool {
    matches!(&event.payload, Payload::BootLeaveReason(leave) if leave.reason_id == reason_id)
}

impl StateDir {
    /// Whether the live journal or any archived one acknowledged the boot leave reason, newest first. Only complete
    /// lines count.
    pub fn boot_reason_recorded(&self, reason_id: &str) -> Result<bool, LookupError> {
        if find_in(self.fs(), &self.join(EVENTS_FILE), |event| {
            acknowledges(&event, reason_id).then_some(())
        })?
        .is_some()
        {
            return Ok(true);
        }
        self.archived_boot_reason(reason_id)
    }

    pub(crate) fn archived_boot_reason(&self, reason_id: &str) -> Result<bool, LookupError> {
        for session in self.archives()?.iter().rev() {
            let path = self.archive_journal(session);
            if find_in(self.fs(), &path, |event| {
                acknowledges(&event, reason_id).then_some(())
            })?
            .is_some()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The archived session read whole; `None` when the directory holds no archive of it. Only complete lines count.
    pub fn archived_recording(
        &self,
        session: &crate::SessionId,
    ) -> Result<Option<Recording>, FileError> {
        let path = self.archive_journal(session);
        match self.fs().read(&path) {
            Ok(bytes) => Recording::parse(&bytes)
                .map(Some)
                .map_err(|source| FileError { path, source }),
            Err(error) if not_found(&error) => Ok(None),
            Err(error) => Err(FileError {
                path,
                source: error.into(),
            }),
        }
    }

    /// Every journal of the directory read whole: the archived sessions oldest first, then the live journal when
    /// there is one. Only complete lines count.
    pub fn recordings(&self) -> Result<Vec<Recording>, LookupError> {
        self.recordings_with(Decoding::Strict)
    }

    /// [`recordings`](Self::recordings) read leniently: each recording's [`skipped`](Recording::skipped) says what
    /// was skipped or ignored in it.
    pub fn recordings_lenient(&self) -> Result<Vec<Recording>, LookupError> {
        self.recordings_with(Decoding::Lenient)
    }

    fn recordings_with(&self, decoding: Decoding) -> Result<Vec<Recording>, LookupError> {
        let mut recordings = Vec::new();
        for session in self.archives()? {
            recordings.push(Recording::read_with(
                self.fs(),
                &self.archive_journal(&session),
                decoding,
            )?);
        }
        let live = self.join(EVENTS_FILE);
        let file_error = |source: ReadError| FileError {
            path: live.clone(),
            source,
        };
        match self.fs().read(&live) {
            Ok(bytes) => {
                recordings.push(Recording::parse_with(&bytes, decoding).map_err(file_error)?);
            }
            Err(error) if not_found(&error) => {}
            Err(error) => return Err(file_error(error.into()).into()),
        }
        Ok(recordings)
    }
}

#[cfg(test)]
mod tests;
