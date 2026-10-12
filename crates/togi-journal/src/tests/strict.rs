use serde_json::{Value, json};

use super::fixtures::{at, samples};
use crate::fs::MemFs;
use crate::testing::{boot, live, simulated};
use crate::*;

fn line_of(payload: Payload) -> Value {
    let event = at(2, "boot-a", 0, &[1], payload);
    serde_json::from_str(&event.encode().unwrap()).unwrap()
}

#[test]
fn unknown_kind_is_named() {
    let mut line = line_of(samples().remove(0));
    line["kind"] = json!("trial.nope");
    let error = Event::decode(&line.to_string()).unwrap_err();
    assert!(matches!(error, DecodeError::UnknownKind(_)), "{error}");
    assert!(error.to_string().contains("trial.nope"), "{error}");
}

/// The first sample (`session.start`) as the first journal line, mutated by `corrupt`.
fn corrupted_first_line(corrupt: impl FnOnce(&mut Value)) -> String {
    let mut line = line_of(samples().remove(0));
    line["seq"] = json!(1);
    line.as_object_mut().unwrap().remove("cause");
    corrupt(&mut line);
    line.to_string()
}

fn assert_names_session_start(error: &str) {
    assert!(
        error.contains("session.start"),
        "kind missing from: {error}"
    );
}

#[test]
fn unknown_envelope_field_is_an_error_that_names_the_kind() {
    let line = corrupted_first_line(|line| line["extra"] = json!(1));
    let error = Event::decode(&line).unwrap_err();
    assert!(
        matches!(
            error,
            DecodeError::Envelope {
                kind: Some(EventKind::SessionStart),
                ..
            }
        ),
        "{error}"
    );
    assert!(error.to_string().contains("extra"), "{error}");
    assert_names_session_start(&error.to_string());

    let read = EventReader::new(format!("{line}\n").as_bytes())
        .next()
        .unwrap()
        .unwrap_err();
    assert!(read.to_string().starts_with("line 1:"), "{read}");
    assert!(read.to_string().contains("extra"), "{read}");
    assert_names_session_start(&read.to_string());

    // A kind spelled with a JSON escape is the same kind.
    let escaped = line.replacen("session.start", r"session\u002estart", 1);
    assert_ne!(escaped, line);
    let error = Event::decode(&escaped).unwrap_err();
    assert!(
        matches!(
            error,
            DecodeError::Envelope {
                kind: Some(EventKind::SessionStart),
                ..
            }
        ),
        "{error}"
    );
    assert_names_session_start(&error.to_string());
    let read = EventReader::new(format!("{escaped}\n").as_bytes())
        .next()
        .unwrap()
        .unwrap_err();
    assert_names_session_start(&read.to_string());
}

#[test]
fn invalid_envelope_value_names_the_kind_when_the_kind_is_known() {
    let line = corrupted_first_line(|line| line["seq"] = json!(0));
    let error = Event::decode(&line).unwrap_err();
    assert!(
        matches!(error, DecodeError::Envelope { kind: Some(_), .. }),
        "{error}"
    );
    assert_names_session_start(&error.to_string());
    let read = EventReader::new(format!("{line}\n").as_bytes())
        .next()
        .unwrap()
        .unwrap_err();
    assert_names_session_start(&read.to_string());

    let escaped = line.replacen("session.start", r"session\u002estart", 1);
    assert_ne!(escaped, line);
    let error = Event::decode(&escaped).unwrap_err();
    assert!(
        matches!(
            error,
            DecodeError::Envelope {
                kind: Some(EventKind::SessionStart),
                ..
            }
        ),
        "{error}"
    );
    let read = EventReader::new(format!("{escaped}\n").as_bytes())
        .next()
        .unwrap()
        .unwrap_err();
    assert_names_session_start(&read.to_string());
}

#[test]
fn envelope_errors_without_a_known_kind_do_not_invent_one() {
    let line = corrupted_first_line(|line| {
        line["kind"] = json!("trial.nope");
        line["extra"] = json!(1);
    });
    let error = Event::decode(&line).unwrap_err();
    assert!(
        matches!(error, DecodeError::Envelope { kind: None, .. }),
        "{error}"
    );
    assert!(error.to_string().starts_with("malformed event:"), "{error}");
}

fn kindless_first_line() -> String {
    corrupted_first_line(|line| {
        line.as_object_mut().unwrap().remove("kind");
    })
}

#[test]
fn a_line_without_a_kind_is_an_envelope_error() {
    let error = Event::decode(&kindless_first_line()).unwrap_err();
    assert!(
        matches!(error, DecodeError::Envelope { kind: None, .. }),
        "{error}"
    );
    let message = error.to_string();
    assert!(message.starts_with("malformed event:"), "{message}");
    assert!(message.contains("kind"), "{message}");
}

#[test]
fn open_refuses_a_journal_line_without_a_kind_and_leaves_the_file_unchanged() {
    let mut bytes = kindless_first_line().into_bytes();
    bytes.push(b'\n');
    let fs = MemFs::new();
    fs.put("/state/events.jsonl", bytes.clone());
    let Err(OpenError::Read(error)) = simulated(&fs).open(boot(2)) else {
        panic!("a journal line without a kind must be refused by open");
    };
    assert!(
        matches!(error.source, ReadError::Decode { line: 1, .. }),
        "{error}"
    );
    assert_eq!(live(&fs), bytes);
}

#[test]
fn unknown_payload_field_names_the_kind() {
    for payload in samples() {
        let kind = payload.kind();
        let mut line = line_of(payload);
        line["data"]["bogus_field"] = json!(1);
        let error = Event::decode(&line.to_string()).unwrap_err();
        match &error {
            DecodeError::Payload { kind: named, .. } => assert_eq!(*named, kind),
            other => panic!("{kind}: {other}"),
        }
        assert!(error.to_string().contains(kind.as_str()), "{error}");
    }
}

#[test]
fn missing_required_field_is_an_error() {
    let mut line = line_of(samples().remove(0));
    line["data"].as_object_mut().unwrap().remove("session");
    assert!(matches!(
        Event::decode(&line.to_string()),
        Err(DecodeError::Payload { .. })
    ));
}

#[test]
fn seq_zero_is_refused() {
    let mut line = line_of(samples().remove(0));
    line["seq"] = json!(0);
    assert!(Event::decode(&line.to_string()).is_err());
}

#[test]
fn mono_and_cause_are_omitted_when_empty() {
    let bare = at(1, "boot-a", 0, &[], samples().remove(0));
    let line: Value = serde_json::from_str(&bare.encode().unwrap()).unwrap();
    assert!(line.get("mono_ms").is_none());
    assert!(line.get("cause").is_none());
    let mut zero = bare.clone();
    zero.mono = Some(std::time::Duration::ZERO);
    let line: Value = serde_json::from_str(&zero.encode().unwrap()).unwrap();
    assert_eq!(line["mono_ms"], json!(0));
    assert_eq!(
        Event::decode(&zero.encode().unwrap()).unwrap().mono,
        zero.mono
    );
}

#[test]
fn smu_records_use_the_flat_command_shape() {
    let line = line_of(super::fixtures::sample(EventKind::SmuIntent));
    assert_eq!(line["data"], json!({"op": "set", "core": 3, "offset": -20}));
    let line = line_of(super::fixtures::sample(EventKind::SmuWrite));
    assert_eq!(line["data"], json!({"op": "set_all", "offset": 0}));
}

#[test]
fn out_of_range_offsets_and_cores_are_refused() {
    let mut line = line_of(super::fixtures::sample(EventKind::ProfileRestored));
    line["data"]["offsets"]["0"] = json!(-51);
    assert!(Event::decode(&line.to_string()).is_err());
    let mut line = line_of(super::fixtures::sample(EventKind::ProfileRestored));
    line["data"]["offsets"] = json!({"16": -5});
    assert!(Event::decode(&line.to_string()).is_err());
    let mut line = line_of(super::fixtures::sample(EventKind::SmuIntent));
    line["data"]["offset"] = json!(5);
    assert!(Event::decode(&line.to_string()).is_err());
}

#[test]
fn unknown_decision_vocabulary_is_refused() {
    let mut line = line_of(super::fixtures::sample(EventKind::TunerDecision));
    line["data"]["decision"] = json!("");
    assert!(Event::decode(&line.to_string()).is_err());
    line["data"]["decision"] = json!("check_edge");
    assert!(Event::decode(&line.to_string()).is_err());
}

#[test]
fn hold_is_an_explicit_decision() {
    let mut line = line_of(super::fixtures::sample(EventKind::TunerDecision));
    line["data"]["decision"] = json!("hold");
    let event = Event::decode(&line.to_string()).unwrap();
    let Payload::TunerDecision(decision) = event.payload else {
        panic!("not a decision")
    };
    assert_eq!(decision.decision, Decision::Hold);
}

#[test]
fn unknown_fields_in_nested_machine_types_are_refused() {
    let cases = [
        (EventKind::SessionStart, "cores", Some(0)),
        (EventKind::CrashDetected, "pstore", None),
        (EventKind::TrialSignal, "schedule", None),
    ];
    for (kind, field, index) in cases {
        let mut line = line_of(super::fixtures::sample(kind));
        let nested = match index {
            Some(index) => &mut line["data"][field][index],
            None => &mut line["data"][field],
        };
        assert!(nested.is_object(), "{kind}.{field}");
        nested["bogus"] = json!(1);
        let error = Event::decode(&line.to_string()).unwrap_err();
        assert!(
            matches!(error, DecodeError::Payload { kind: named, .. } if named == kind),
            "{kind}: {error}"
        );
    }
}

#[test]
fn unknown_workload_ids_are_refused_wherever_a_payload_names_one() {
    let cases = [
        (EventKind::TrialIntent, "/data/workload"),
        (EventKind::HuntStart, "/data/workload"),
        (EventKind::CheckingChain, "/data/workload"),
        (EventKind::TrialCarried, "/data/class/workload"),
        (EventKind::TunerDecision, "/data/workloads/0"),
    ];
    for (kind, pointer) in cases {
        let mut line = line_of(super::fixtures::sample(kind));
        *line.pointer_mut(pointer).unwrap() = json!("not-a-workload");
        let error = Event::decode(&line.to_string()).unwrap_err();
        assert!(
            error.to_string().contains("not-a-workload"),
            "{kind}: {error}"
        );
    }
}

#[test]
fn an_idle_class_has_no_workload_on_the_wire() {
    let line = line_of(super::fixtures::sample(EventKind::FailureCarried));
    assert!(line["data"]["class"].get("workload").is_none());
}

#[test]
fn a_config_that_does_not_fit_the_snapshot_is_refused() {
    let mut line = line_of(super::fixtures::sample(EventKind::ConfigLoaded));
    line["data"]["config"]["start_offsets"] = json!({"3": 10});
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
    let mut line = line_of(super::fixtures::sample(EventKind::ConfigLoaded));
    line["data"]["config"]["bogus"] = json!(1);
    assert!(Event::decode(&line.to_string()).is_err());
}

#[test]
fn a_defect_id_of_zero_is_refused() {
    for kind in [EventKind::DefectFound, EventKind::DefectAnswered] {
        let mut line = line_of(super::fixtures::sample(kind));
        line["data"]["id"] = json!(0);
        assert!(Event::decode(&line.to_string()).is_err(), "{kind}");
    }
}
