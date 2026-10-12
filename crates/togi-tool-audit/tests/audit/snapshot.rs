use serde_json::{Value, json};
use togi_journal::fs::MemFs;
use togi_journal::{Event, StateDir};
use togi_tool_audit::Check::Projection;
use togi_tool_audit::Completion::InProgress;
use togi_tool_audit::audit_recordings;

use crate::fixtures::*;

fn events() -> Vec<Event> {
    vec![header(), phase(2, 0, Some(-30))]
}

fn fresh(events: &[Event]) -> Value {
    json!({
        "facts": togi_journal::State::replay(events).unwrap(),
        "tuner": togi_tuner::replay(events).unwrap().project(),
    })
}

fn found(state: Option<Value>) -> Vec<(u64, togi_tool_audit::Check)> {
    let events = events();
    let fs = MemFs::new();
    let journal: Vec<u8> = events
        .iter()
        .flat_map(|event| {
            let mut line = event.encode().unwrap().into_bytes();
            line.push(b'\n');
            line
        })
        .collect();
    fs.put("/state/events.jsonl", journal);
    if let Some(state) = state {
        fs.put("/state/state.json", serde_json::to_vec(&state).unwrap());
    }
    let dir = StateDir::simulated(fs).unwrap();
    audit_recordings(&dir, InProgress)
        .unwrap()
        .into_iter()
        .flat_map(|audit| audit.violations)
        .map(|violation| (violation.seq.get(), violation.check))
        .collect()
}

fn changed(edit: impl FnOnce(&mut Value)) -> Value {
    let mut state = fresh(&events());
    edit(&mut state);
    state
}

#[test]
fn a_current_state_document_equals_a_fresh_fold_of_its_journal() {
    assert_eq!(found(Some(fresh(&events()))), []);
    assert_eq!(found(None), [], "no document is nothing to compare");
}

#[test]
fn a_current_document_that_disagrees_with_the_journal_is_a_violation() {
    let facts = changed(|state| state["facts"]["cores"] = json!([]));
    assert_eq!(found(Some(facts)), [(2, Projection)]);
    let tuner = changed(|state| state["tuner"] = fresh(&events()[..1])["tuner"].clone());
    assert_ne!(tuner, fresh(&events()));
    assert_eq!(found(Some(tuner)), [(2, Projection)]);
}

#[test]
fn a_document_from_another_sequence_is_stale_and_not_compared() {
    let behind = changed(|state| {
        state["facts"]["last_seq"] = json!(1);
        state["facts"]["cores"] = json!([]);
    });
    assert_eq!(found(Some(behind)), []);
    let ahead = changed(|state| state["facts"]["last_seq"] = json!(3));
    assert_eq!(found(Some(ahead)), []);
}

#[test]
fn a_document_not_shaped_as_this_build_writes_it_is_a_violation() {
    let unknown = changed(|state| state["extra"] = json!(true));
    let missing = changed(|state| {
        state.as_object_mut().unwrap().remove("tuner");
    });
    let nested = changed(|state| {
        state["facts"].as_object_mut().unwrap().remove("session");
    });
    let fresh = fresh(&events());
    let positional = json!([fresh["facts"], fresh["tuner"]]);
    for state in [unknown, missing, nested, positional] {
        assert_eq!(found(Some(state.clone())), [(2, Projection)], "{state}");
    }
}
