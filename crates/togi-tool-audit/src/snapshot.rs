use serde::Serialize;
use togi_journal::{Event, StateDir, StateError};
use togi_machine::object_only;
use togi_tuner::Projection;

use crate::AuditError;
use crate::violation::{Check, Violation};

object_only! {
    #[derive(Debug, Serialize)]
    #[serde(deny_unknown_fields)]
    struct Snapshot {
        facts: togi_journal::State,
        tuner: Projection,
    }
}

/// Compares the published `state.json` with a fresh fold of the live journal: the journal's facts replayed and the
/// tuner's projection of `togi_tuner::replay`. A document that does not decode as this build's shape is a violation.
/// One published at another sequence than the journal's last is stale by design, since a crash between the
/// journal's fsync and the state write leaves it behind, and is not compared; neither is a directory with none.
pub(crate) fn audit(dir: &StateDir, events: &[Event]) -> Result<Vec<Violation>, AuditError> {
    let Some(last) = events.last().map(|event| event.seq) else {
        return Ok(Vec::new());
    };
    let violation = |reason: String| Violation {
        seq: last,
        check: Check::Projection,
        reason,
    };
    let snapshot = match dir.read_state::<Snapshot>() {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => return Ok(Vec::new()),
        Err(StateError::Decode(error)) => {
            return Ok(vec![violation(format!(
                "state.json is not this build's document: {error}"
            ))]);
        }
        Err(error) => return Err(AuditError::State(error)),
    };
    if snapshot.facts.last_seq != Some(last) {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    match togi_journal::State::replay(events) {
        Ok(facts) if facts != snapshot.facts => found.push(violation(
            "state.json facts differ from a fresh fold of the journal".to_owned(),
        )),
        Ok(_) => {}
        Err(error) => found.push(violation(format!(
            "the journal's facts do not fold: {error}"
        ))),
    }
    match togi_tuner::replay(events) {
        Ok(tuner) if tuner.project() != snapshot.tuner => found.push(violation(
            "state.json tuner projection differs from a fresh replay of the journal".to_owned(),
        )),
        Ok(_) => {}
        Err(error) => found.push(violation(format!(
            "the tuner does not replay the journal: {error}"
        ))),
    }
    Ok(found)
}
