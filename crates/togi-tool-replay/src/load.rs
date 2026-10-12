//! Reading the recording to play.

use std::path::Path;

use togi_journal::{Build, Event, Operation, Payload, Recording, SCHEMA, Verdict};
use togi_tuner::{EVIDENCE_EPOCH, RULESET};

use crate::Error;

/// The events of the journal in `state_dir`, without locking or changing it. A recording written under another
/// ruleset or a newer schema is refused, since the dashboard projects it with the current tuning rules.
pub(crate) fn events(state_dir: &Path) -> Result<Vec<Event>, Error> {
    let events = Recording::read(&state_dir.join("events.jsonl"))
        .map_err(Error::Read)?
        .into_events();
    let Some(Payload::SessionStart(start)) = events.first().map(|event| &event.payload) else {
        return Err(Error::Empty(state_dir.to_path_buf()));
    };
    let reader = Build {
        version: String::new(),
        rev: None,
        ruleset: RULESET,
        schema: SCHEMA,
        fixes: 0,
        evidence: EVIDENCE_EPOCH,
    };
    match reader.verdict(&start.build, Operation::Replay) {
        Verdict::Refuse(refusal) => Err(Error::Refused(refusal)),
        Verdict::Use | Verdict::Archive => Ok(events),
    }
}
