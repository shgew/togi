//! Reading the recording to play.

use std::path::Path;

use togi_journal::{
    Build, Event, EventReader, FileError, Operation, Payload, ReadError, SCHEMA, Verdict,
};
use togi_tuner::{EVIDENCE_EPOCH, RULESET};

use crate::Error;

/// A recording read leniently: the events to play and what the read left out.
#[derive(Debug)]
pub(crate) struct Loaded {
    pub events: Vec<Event>,
    /// The journal crate's one-line report of the events of unknown kinds skipped and the unknown fields ignored;
    /// `None` when the read left nothing out.
    pub skipped: Option<String>,
}

/// The events of the journal in `state_dir`, without locking or changing it, and without its unfinished last line.
/// The read is lenient: an event of a kind this build does not know is skipped and an unknown field is ignored, and
/// [`Loaded::skipped`] says which. A recording written under another ruleset or a newer schema is refused once its
/// first event is read, before any other event is decoded, since the dashboard projects it with the current tuning
/// rules.
pub(crate) fn events(state_dir: &Path) -> Result<Loaded, Error> {
    let path = state_dir.join("events.jsonl");
    let read = |source| {
        Error::Read(FileError {
            path: path.clone(),
            source,
        })
    };
    let bytes = std::fs::read(&path).map_err(|source| read(ReadError::Io(source)))?;
    let mut reader = EventReader::lenient(bytes.as_slice());
    let Some(first) = reader.next().transpose().map_err(read)? else {
        return Err(Error::Empty(state_dir.to_path_buf()));
    };
    let current = Build {
        version: String::new(),
        rev: None,
        ruleset: RULESET,
        schema: SCHEMA,
        fixes: 0,
        evidence: EVIDENCE_EPOCH,
    };
    if let Payload::SessionStart(start) = &first.payload
        && let Verdict::Refuse(refusal) = current.verdict(&start.build, Operation::Replay)
    {
        return Err(Error::Refused(refusal));
    }
    let mut events = vec![first];
    for event in reader.by_ref() {
        events.push(event.map_err(read)?);
    }
    let skipped = reader.skipped();
    Ok(Loaded {
        events,
        skipped: (!skipped.is_empty()).then(|| skipped.to_string()),
    })
}
