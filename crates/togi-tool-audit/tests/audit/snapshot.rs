use serde_json::{Value, json};
use togi_journal::fs::MemFs;
use togi_journal::{Event, StateDir};
use togi_tool_audit::Check::Projection;
use togi_tool_audit::Completion::{self, Finished, InProgress};
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

#[test]
fn a_nested_key_this_build_does_not_write_or_leaves_out_is_a_violation() {
    let fresh = fresh(&events());
    assert_eq!(fresh["facts"]["dead_end"], Value::Null);
    assert_eq!(fresh["tuner"]["deepening"], Value::Null);
    let unknown_fact = changed(|state| state["facts"]["extra"] = json!(true));
    let unknown_tuner = changed(|state| state["tuner"]["extra"] = json!(1));
    let missing_fact = changed(|state| {
        state["facts"].as_object_mut().unwrap().remove("dead_end");
    });
    let missing_tuner = changed(|state| {
        state["tuner"].as_object_mut().unwrap().remove("deepening");
    });
    let fields = [
        "cores",
        "constraints",
        "checking",
        "deepening",
        "cycle_plan",
        "search_turns",
        "hunt_plan",
        "phase_plan",
        "bios_profile",
        "r7_status",
        "clean_cycles",
        "first_result",
        "requirement",
        "trial_history",
    ];
    assert_eq!(fresh["tuner"].as_object().unwrap().len(), fields.len());
    let positional_tuner = changed(|state| {
        state["tuner"] = fields
            .iter()
            .map(|field| fresh["tuner"][field].clone())
            .collect();
    });
    let cases = [
        ("an unknown key in facts", unknown_fact),
        ("an unknown key in tuner", unknown_tuner),
        ("a missing None facts key", missing_fact),
        ("a missing None tuner key", missing_tuner),
        ("a positional tuner", positional_tuner),
    ];
    let undetected: Vec<&str> = cases
        .into_iter()
        .filter(|(_, state)| found(Some(state.clone())) != [(2, Projection)])
        .map(|(name, _)| name)
        .collect();
    assert_eq!(undetected, Vec::<&str>::new());
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
