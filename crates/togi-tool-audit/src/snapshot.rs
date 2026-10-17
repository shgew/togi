use serde::{Deserialize, Serialize};
use serde_json::Value;
use togi_journal::{Event, StateDir, StateError};
use togi_machine::object_only;
use togi_tuner::Projection;

use crate::AuditError;
use crate::violation::{Check, Completion, Violation};

object_only! {
    #[derive(Debug, Serialize)]
    #[serde(deny_unknown_fields)]
    struct Snapshot {
        facts: togi_journal::State,
        tuner: Projection,
    }
}

/// Compares the published `state.json` with a fresh fold of the live journal: the journal's facts replayed and the
/// tuner's projection of `togi_tuner::replay`. A document that does not decode as this build's shape is a violation,
/// and so is one whose JSON differs from what this build writes for the fresh fold, since the nested types drop
/// unknown keys, fill a missing optional one and accept positional arrays when decoded.
/// One published at another sequence than the journal's last is stale by design while the run is in progress, since
/// a crash between the journal's fsync and the state write leaves it behind, and is not compared; neither is a
/// directory with none. A finished run publishes its final document, so for it either is a violation.
pub(crate) fn audit(
    dir: &StateDir,
    events: &[Event],
    completion: Completion,
) -> Result<Vec<Violation>, AuditError> {
    let Some(last) = events.last().map(|event| event.seq) else {
        return Ok(Vec::new());
    };
    let violation = |reason: String| Violation {
        seq: last,
        check: Check::Projection,
        reason,
    };
    let finished = completion == Completion::Finished;
    let document = match dir.read_state::<Value>() {
        Ok(Some(document)) => document,
        Ok(None) if finished => {
            return Ok(vec![violation(
                "finished run has no published state.json".to_owned(),
            )]);
        }
        Ok(None) => return Ok(Vec::new()),
        Err(StateError::Decode(error)) => {
            return Ok(vec![violation(format!(
                "state.json is not this build's document: {}",
                togi_machine::error_chain(&error)
            ))]);
        }
        Err(error) => return Err(AuditError::State(error)),
    };
    let snapshot = match Snapshot::deserialize(&document) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return Ok(vec![violation(format!(
                "state.json is not this build's document: {}",
                togi_machine::error_chain(&error)
            ))]);
        }
    };
    if snapshot.facts.last_seq != Some(last) {
        return Ok(if finished {
            vec![violation(format!(
                "finished run's state.json is at {} rather than the journal's last event",
                snapshot
                    .facts
                    .last_seq
                    .map_or_else(|| "no event".to_owned(), |seq| format!("seq {seq}")),
            ))]
        } else {
            Vec::new()
        });
    }
    let mut found = Vec::new();
    match togi_journal::State::replay(events) {
        Ok(facts) if facts != snapshot.facts => found.push(violation(
            "state.json facts differ from a fresh fold of the journal".to_owned(),
        )),
        Ok(facts) if !written_as(&document["facts"], &facts) => found.push(violation(
            "state.json facts are not written as this build writes them".to_owned(),
        )),
        Ok(_) => {}
        Err(error) => found.push(violation(format!(
            "the journal's facts do not fold: {}",
            togi_machine::error_chain(&error)
        ))),
    }
    match togi_tuner::replay(events) {
        Ok(tuner) if tuner.project() != snapshot.tuner => found.push(violation(
            "state.json tuner projection differs from a fresh replay of the journal".to_owned(),
        )),
        Ok(tuner) if !written_as(&document["tuner"], &tuner.project()) => found.push(violation(
            "state.json tuner projection is not written as this build writes it".to_owned(),
        )),
        Ok(_) => {}
        Err(error) => found.push(violation(format!(
            "the tuner does not replay the journal: {}",
            togi_machine::error_chain(&error)
        ))),
    }
    Ok(found)
}

fn written_as(document: &Value, fresh: &impl Serialize) -> bool {
    serde_json::to_value(fresh).is_ok_and(|fresh| *document == fresh)
}
