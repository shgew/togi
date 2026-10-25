use serde_json::{Value, json};
use togi_journal::fs::MemFs;
use togi_journal::{Event, StateDir};
use togi_tool_audit::Check::Projection;
use togi_tool_audit::Completion::{self, Finished, InProgress};
use togi_tool_audit::audit_recordings;

use crate::fixtures::*;

fn events() -> Vec<Event> {
    vec![header(), phase(2, 0)]
}

fn fresh(events: &[Event]) -> Value {
    state_document(events)
}

fn found(state: Option<Value>) -> Vec<(u64, togi_tool_audit::Check)> {
    found_in(&events(), state, InProgress)
}

fn found_in(
    events: &[Event],
    state: Option<Value>,
    completion: Completion,
) -> Vec<(u64, togi_tool_audit::Check)> {
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
    #[expect(
        clippy::disallowed_methods,
        reason = "projection content fixtures open only the injected in-memory journal"
    )]
    let dir = StateDir::simulated(fs).unwrap();
    audit_recordings(&dir, completion)
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
    let tuner = changed(|state| state["tuner"] = json!({}));
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

#[derive(Clone, Copy)]
enum StateShape {
    Unknown,
    Missing,
    Nested,
    Positional,
}

#[test_case::test_case(StateShape::Unknown; "Unknown")]
#[test_case::test_case(StateShape::Missing; "Missing")]
#[test_case::test_case(StateShape::Nested; "Nested")]
#[test_case::test_case(StateShape::Positional; "Positional")]
fn a_document_not_shaped_as_this_build_writes_it_is_a_violation(scenario: StateShape) {
    let unknown = changed(|state| state["extra"] = json!(true));
    let missing = changed(|state| {
        state.as_object_mut().unwrap().remove("tuner");
    });
    let nested = changed(|state| {
        state["facts"].as_object_mut().unwrap().remove("session");
    });
    let fresh = fresh(&events());
    let positional = json!([fresh["facts"], fresh["tuner"]]);
    let state = match scenario {
        StateShape::Unknown => unknown,
        StateShape::Missing => missing,
        StateShape::Nested => nested,
        StateShape::Positional => positional,
    };

    assert_eq!(found(Some(state.clone())), [(2, Projection)], "{state}");
}

#[derive(Clone, Copy)]
enum NestedStateShape {
    UnknownFacts,
    UnknownTuner,
    MissingFacts,
    MissingTuner,
    PositionalTuner,
}

#[test_case::test_case(NestedStateShape::UnknownFacts; "UnknownFacts")]
#[test_case::test_case(NestedStateShape::UnknownTuner; "UnknownTuner")]
#[test_case::test_case(NestedStateShape::MissingFacts; "MissingFacts")]
#[test_case::test_case(NestedStateShape::MissingTuner; "MissingTuner")]
#[test_case::test_case(NestedStateShape::PositionalTuner; "PositionalTuner")]
fn a_nested_key_this_build_does_not_write_or_leaves_out_is_a_violation(scenario: NestedStateShape) {
    let fresh = fresh(&events());
    assert_eq!(fresh["facts"]["dead_end"], Value::Null);
    let tuner_keys: Vec<String> = fresh["tuner"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    let unknown_fact = changed(|state| state["facts"]["extra"] = json!(true));
    let unknown_tuner = changed(|state| state["tuner"]["extra"] = json!(1));
    let missing_fact = changed(|state| {
        state["facts"].as_object_mut().unwrap().remove("dead_end");
    });
    let missing_tuner = changed(|state| {
        state["tuner"]
            .as_object_mut()
            .unwrap()
            .remove(&tuner_keys[0]);
    });
    let positional_tuner = changed(|state| {
        state["tuner"] = tuner_keys
            .iter()
            .map(|field| fresh["tuner"][field].clone())
            .collect();
    });
    let (name, state) = match scenario {
        NestedStateShape::UnknownFacts => ("an unknown key in facts", unknown_fact),
        NestedStateShape::UnknownTuner => ("an unknown key in tuner", unknown_tuner),
        NestedStateShape::MissingFacts => ("a missing None facts key", missing_fact),
        NestedStateShape::MissingTuner => ("a missing None tuner key", missing_tuner),
        NestedStateShape::PositionalTuner => ("a positional tuner", positional_tuner),
    };
    assert_eq!(found(Some(state)), [(2, Projection)], "{name}");
}

#[test]
fn a_finished_run_needs_a_state_document_at_its_last_event() {
    let events = vec![header(), stop(2)];
    let current = fresh(&events);
    assert_eq!(found_in(&events, Some(current.clone()), Finished), []);
    assert_eq!(found_in(&events, None, Finished), [(2, Projection)]);
    let mut behind = current.clone();
    behind["facts"]["last_seq"] = json!(1);
    assert_eq!(found_in(&events, Some(behind), Finished), [(2, Projection)]);
    let mut empty = current;
    empty["facts"]["last_seq"] = Value::Null;
    assert_eq!(found_in(&events, Some(empty), Finished), [(2, Projection)]);
    assert_eq!(found_in(&events, None, InProgress), []);
}

#[test]
fn a_resealed_full_reducer_mismatch_reports_a_projection_violation() {
    let events = events();
    let mut state = fresh(&events);
    let prefix = serde_json::from_value(state["snapshot"]["prefix"].clone()).unwrap();
    state.as_object_mut().unwrap().remove("snapshot");
    state["recovery"]["trials"] = json!(999);
    let state = togi_journal::snapshot::seal(prefix, &state).unwrap();
    let fs = MemFs::new();
    let journal = events
        .iter()
        .map(|event| event.encode().unwrap() + "\n")
        .collect::<String>();
    fs.put("/state/events.jsonl", journal.into_bytes());
    fs.put("/state/state.json", serde_json::to_vec(&state).unwrap());
    #[expect(
        clippy::disallowed_methods,
        reason = "the injected memory journal is the audit fixture"
    )]
    let dir = StateDir::simulated(fs).unwrap();
    let violations: Vec<_> = audit_recordings(&dir, InProgress)
        .unwrap()
        .into_iter()
        .flat_map(|audit| audit.violations)
        .collect();
    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].seq, events.last().unwrap().seq);
    assert_eq!(violations[0].check, Projection);
    assert_eq!(
        violations[0].reason,
        "state.json full folded state differs: Different"
    );
}
