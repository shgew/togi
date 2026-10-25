use std::fmt::Write as _;
use std::io;

use jiff::Timestamp;
use togi_machine::BootId;

use super::{Journal, OpenError, Writer, encode, seq_after};
use crate::dir::{EVENTS_FILE, EVENTS_TMP, StateDir, io_at, not_found};
use crate::event::{Event, Seq, Stamp};
use crate::payload::{JournalTorn, Payload};
use crate::reader::{FileError, Recording, Torn};
use crate::receipt::JournalId;
use crate::session_id::SessionId;

fn repair_step(step: &'static str) -> impl FnOnce(io::Error) -> OpenError {
    move |source| OpenError::Repair { step, source }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
}

fn started_session(events: &[Event]) -> Option<SessionId> {
    match events.first().map(|event| &event.payload) {
        Some(Payload::SessionStart(start)) => Some(start.session),
        _ => None,
    }
}

fn recorded_archive(events: &[Event]) -> Result<Option<SessionId>, OpenError> {
    let Some(live) = started_session(events) else {
        return Ok(None);
    };
    let archived = events.iter().find_map(|event| {
        if let Payload::SessionArchived(archived) = &event.payload {
            Some(archived.session)
        } else {
            None
        }
    });
    match archived {
        Some(recorded) if recorded == live => Ok(Some(live)),
        Some(recorded) => Err(OpenError::ArchiveMismatch { recorded, live }),
        None => Ok(None),
    }
}

impl StateDir {
    /// Opens the directory's journal for writing: finishes an archive that was recorded but not completed, repairs a
    /// torn tail by recording it as a `journal.torn` event, and validates what is left. A failed open leaves no writer
    /// and releases the lock.
    pub fn open(self, boot: BootId) -> Result<Journal, OpenError> {
        self.open_at(boot, Timestamp::now())
    }

    /// [`StateDir::open`] with the time a repair record is stamped with, for simulations that own the clock.
    pub fn open_at(self, boot: BootId, now: Timestamp) -> Result<Journal, OpenError> {
        self.recover_compat()?;
        let path = self.join(EVENTS_FILE);
        let bytes = match self.fs().read(&path) {
            Ok(bytes) => bytes,
            Err(error) if not_found(&error) => Vec::new(),
            Err(error) => return Err(io_at("read journal", &path)(error).into()),
        };
        let recording = Recording::parse(&bytes).map_err(|source| FileError {
            path: path.clone(),
            source,
        })?;
        let (events, torn) = recording.into_parts();
        let (mut events, torn) = if let Some(session) = recorded_archive(&events)? {
            self.finish_archive(&session)?;
            (Vec::new(), None)
        } else {
            if let Some(session) = started_session(&events) {
                self.drop_stale_import(&session)?;
            }
            (events, torn)
        };
        let complete = torn
            .as_ref()
            .map_or(bytes.len(), |tail| bytes.len() - tail.bytes.len());
        let repaired = if let Some(torn) = torn {
            self.repair(&bytes, &mut events, &torn, &boot, now)?
        } else {
            None
        };
        let archive = self.archive_dir();
        if self.kind_of(&archive)?.is_some() {
            self.sync_dir(&archive)?;
        }
        let mut file = self
            .fs()
            .open_append(&path)
            .map_err(io_at("open journal", &path))?;
        file.sync().map_err(io_at("sync journal", &path))?;
        self.sync_dir(self.path())?;
        let session = started_session(&events);
        let mut prefix = crate::snapshot::PrefixHash::default();
        if !events.is_empty() {
            prefix.append(&bytes[..complete]);
            if repaired.is_some() {
                if let Some(event) = events.last() {
                    prefix.append(&encode(event).map_err(OpenError::Encode)?);
                }
            }
        }
        let writer = Writer {
            file,
            buffer: Vec::new(),
            buffered: self.buffered(),
        };
        Ok(Journal {
            writer,
            events,
            boot,
            id: JournalId::next(),
            repaired,
            session,
            poison: None,
            prefix,
            dir: self,
        })
    }

    fn repair(
        &self,
        bytes: &[u8],
        events: &mut Vec<Event>,
        torn: &Torn,
        boot: &BootId,
        now: Timestamp,
    ) -> Result<Option<Seq>, OpenError> {
        let path = self.join(EVENTS_FILE);
        if events.is_empty() {
            let mut file = self
                .fs()
                .open_append(&path)
                .map_err(repair_step("open journal"))?;
            file.set_len(0).map_err(repair_step("truncate"))?;
            file.sync().map_err(repair_step("sync truncation"))?;
            return Ok(None);
        }
        let seq = seq_after(events.last()).ok_or(OpenError::SeqExhausted)?;
        let payload = Payload::JournalTorn(JournalTorn {
            offset: torn.offset,
            bytes_hex: hex(&torn.bytes),
        });
        let event = Event::new(
            seq,
            Stamp {
                time: now,
                mono: None,
            },
            boot.clone(),
            Vec::new(),
            payload,
        );
        let line = encode(&event).map_err(OpenError::Encode)?;
        let complete = &bytes[..bytes.len() - torn.bytes.len()];
        let mut repaired = Vec::with_capacity(complete.len() + line.len());
        repaired.extend_from_slice(complete);
        repaired.extend_from_slice(&line);
        let tmp = self.join(EVENTS_TMP);
        let mut file = self
            .fs()
            .create_truncate(&tmp)
            .map_err(repair_step("create temporary journal"))?;
        file.write_all(&repaired)
            .map_err(repair_step("write temporary journal"))?;
        file.sync().map_err(repair_step("sync temporary journal"))?;
        drop(file);
        self.fs()
            .rename(&tmp, &path)
            .map_err(repair_step("replace journal"))?;
        self.fs()
            .sync_dir(self.path())
            .map_err(repair_step("sync state directory"))?;
        events.push(event);
        Ok(Some(seq))
    }
}
