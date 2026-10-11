//! Everything decoded from journal or state JSON is an object: serde's positional array form of a
//! struct, which `deny_unknown_fields` does not constrain, is refused even when correctly sized.

use serde::Deserialize;
use serde_json::{Value, json};

use super::fixtures::{at, sample, samples};
use crate::fs::MemFs;
use crate::*;

fn line_of(payload: Payload) -> Value {
    let event = at(2, "boot-a", 0, &[1], payload);
    serde_json::from_str(&event.encode().unwrap()).unwrap()
}

fn assert_expected_an_object(error: &str) {
    assert!(error.contains("expected an object"), "{error}");
}

/// `value`'s members positionally, in the order of `fields`.
fn positional(value: &Value, fields: &[&str]) -> Value {
    Value::Array(
        fields
            .iter()
            .map(|field| value.get(field).cloned().unwrap_or(Value::Null))
            .collect(),
    )
}

/// Every object nested in `value` (not `value` itself), as a JSON pointer.
fn nested_objects(value: &Value, at: &str, found: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, member) in map {
                let pointer = format!("{at}/{key}");
                if member.is_object() {
                    found.push(pointer.clone());
                }
                nested_objects(member, &pointer, found);
            }
        }
        Value::Array(items) => {
            for (index, member) in items.iter().enumerate() {
                let pointer = format!("{at}/{index}");
                if member.is_object() {
                    found.push(pointer.clone());
                }
                nested_objects(member, &pointer, found);
            }
        }
        _ => {}
    }
}

fn as_array(object: &Value) -> Value {
    Value::Array(object.as_object().unwrap().values().cloned().collect())
}

#[test]
fn an_envelope_line_that_is_an_array_is_refused() {
    let line = line_of(sample(EventKind::SessionArchived));
    // The envelope's eight fields in declaration order.
    let array = json!([
        line["seq"],
        line["time"],
        line["boot"],
        null,
        line["cause"],
        line["kind"],
        line["msg"],
        line["data"],
    ])
    .to_string();
    let error = Event::decode(&array).unwrap_err();
    assert!(
        matches!(error, DecodeError::Envelope { kind: None, .. }),
        "{error}"
    );
    assert_expected_an_object(&error.to_string());
    let read = EventReader::new(format!("{array}\n").as_bytes())
        .next()
        .unwrap()
        .unwrap_err();
    assert_expected_an_object(&read.to_string());
    assert_expected_an_object(
        &serde_json::from_str::<Event>(&array)
            .unwrap_err()
            .to_string(),
    );
}

#[test]
fn a_correctly_sized_positional_data_is_refused() {
    // `config.loaded`: build, config, path, file, kernel_cursor, kernel_error.
    let mut line = line_of(sample(EventKind::ConfigLoaded));
    let data = line["data"].clone();
    let array = json!([
        data["build"],
        data["config"],
        data["path"],
        data["file"],
        null,
        null
    ]);
    assert!(payload::ConfigLoaded::deserialize(array.clone()).is_err());
    line["data"] = array;
    let error = Event::decode(&line.to_string()).unwrap_err();
    assert!(
        matches!(
            error,
            DecodeError::Payload {
                kind: EventKind::ConfigLoaded,
                ..
            }
        ),
        "{error}"
    );
    assert_expected_an_object(&error.to_string());

    // `session.archived`: session.
    let mut line = line_of(sample(EventKind::SessionArchived));
    let array = json!([line["data"]["session"]]);
    assert!(payload::SessionArchived::deserialize(array.clone()).is_err());
    line["data"] = array;
    let error = Event::decode(&line.to_string()).unwrap_err();
    assert!(
        matches!(
            error,
            DecodeError::Payload {
                kind: EventKind::SessionArchived,
                ..
            }
        ),
        "{error}"
    );
    assert_expected_an_object(&error.to_string());
}

#[test]
fn every_payload_kind_refuses_a_positional_data() {
    for payload in samples() {
        let kind = payload.kind();
        let mut line = line_of(payload);
        assert!(Event::decode(&line.to_string()).is_ok(), "{kind}");
        line["data"] = as_array(&line["data"]);
        let error = Event::decode(&line.to_string()).unwrap_err();
        assert!(
            matches!(error, DecodeError::Payload { kind: named, .. } if named == kind),
            "{kind}: {error}"
        );
    }
}

#[test]
fn every_struct_nested_in_a_payload_refuses_a_positional_array() {
    let mut checked = 0;
    for payload in samples() {
        let kind = payload.kind();
        let line = line_of(payload);
        let mut found = Vec::new();
        nested_objects(&line["data"], "/data", &mut found);
        for pointer in found {
            let mut broken = line.clone();
            let nested = broken.pointer_mut(&pointer).unwrap();
            *nested = as_array(nested);
            let error = Event::decode(&broken.to_string()).unwrap_err();
            assert!(
                matches!(error, DecodeError::Payload { kind: named, .. } if named == kind),
                "{kind} {pointer}: {error}"
            );
            checked += 1;
        }
    }
    assert!(checked > 50, "only {checked} nested objects were checked");
}

#[test]
fn nested_journal_structs_refuse_a_correctly_sized_array() {
    // `session.start`'s build: version, rev, ruleset, schema, fixes, evidence.
    let mut line = line_of(sample(EventKind::SessionStart));
    line["data"]["build"] = positional(
        &line["data"]["build"].clone(),
        &["version", "rev", "ruleset", "schema", "fixes", "evidence"],
    );
    assert_eq!(line["data"]["build"].as_array().unwrap().len(), 6);
    assert!(Build::deserialize(line["data"]["build"].clone()).is_err());
    let error = Event::decode(&line.to_string()).unwrap_err();
    assert_expected_an_object(&error.to_string());

    // `session.start`'s core: core, ccd, cpus.
    let mut line = line_of(sample(EventKind::SessionStart));
    line["data"]["cores"][0] =
        positional(&line["data"]["cores"][0].clone(), &["core", "ccd", "cpus"]);
    assert_eq!(line["data"]["cores"][0].as_array().unwrap().len(), 3);
    let error = Event::decode(&line.to_string()).unwrap_err();
    assert_expected_an_object(&error.to_string());

    // `trial.carried`'s fact source: session, seq, build, trial, time, boot, backend.
    let mut line = line_of(sample(EventKind::TrialCarried));
    line["data"]["source"] = positional(
        &line["data"]["source"].clone(),
        &[
            "session", "seq", "build", "trial", "time", "boot", "backend",
        ],
    );
    assert_eq!(line["data"]["source"].as_array().unwrap().len(), 7);
    let error = Event::decode(&line.to_string()).unwrap_err();
    assert_expected_an_object(&error.to_string());

    // `trial.signal`'s schedule is internally tagged: the tag leads the sequence.
    let mut line = line_of(sample(EventKind::TrialSignal));
    assert!(line["data"]["schedule"].is_object());
    line["data"]["schedule"] = json!(["random", 7]);
    let error = Event::decode(&line.to_string()).unwrap_err();
    assert_expected_an_object(&error.to_string());

    // `smu.intent`'s command is internally tagged too.
    let mut line = line_of(sample(EventKind::SmuIntent));
    line["data"] = json!(["set_all", -5]);
    let error = Event::decode(&line.to_string()).unwrap_err();
    assert_expected_an_object(&error.to_string());
}

fn state_value() -> Value {
    let state = State::replay(&[at(1, "boot-a", 0, &[], sample(EventKind::SessionStart))]).unwrap();
    serde_json::to_value(state).unwrap()
}

fn read_state(document: &Value) -> Result<Option<State>, StateError> {
    let fs = MemFs::new();
    let dir = crate::testing::simulated(&fs);
    fs.put("/state/state.json", document.to_string());
    dir.read_state::<State>()
}

#[test]
fn state_decodes_only_from_an_object() {
    let object = state_value();
    assert!(read_state(&object).unwrap().is_some());

    let fields = [
        "schema", "session", "cores", "last_seq", "open", "dead_end", "backends",
    ];
    let array = positional(&object, &fields);
    assert!(State::deserialize(array.clone()).is_err());
    assert_expected_an_object(&read_state(&array).unwrap_err().to_string());
}

#[test]
fn backend_paths_decode_only_from_an_object() {
    let mut object = state_value();
    object["backends"] = json!({"mprime": "/opt/mprime", "ycruncher": null});
    assert_eq!(
        read_state(&object).unwrap().unwrap().backends,
        BackendPaths {
            mprime: Some("/opt/mprime".into()),
            ycruncher: None,
        }
    );

    assert!(BackendPaths::deserialize(json!(["/opt/mprime", null])).is_err());
    object["backends"] = json!(["/opt/mprime", null]);
    assert_expected_an_object(&read_state(&object).unwrap_err().to_string());
    assert!(serde_json::from_str::<BackendPaths>(r#"["/opt/mprime",null]"#).is_err());
}

#[test]
fn every_struct_nested_in_state_refuses_a_positional_array() {
    let object = state_value();
    let mut found = Vec::new();
    nested_objects(&object, "", &mut found);
    assert!(found.len() >= 4, "{found:?}");
    for pointer in found {
        let mut broken = object.clone();
        let nested = broken.pointer_mut(&pointer).unwrap();
        *nested = as_array(nested);
        let error = read_state(&broken).unwrap_err();
        assert!(matches!(error, StateError::Decode(_)), "{pointer}: {error}");
    }
}
