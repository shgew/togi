use serde_json::{Value, json};
use test_case::test_case;

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
    assert_eq!(error.to_string(), "decode event kind");
    assert_eq!(
        togi_machine::error_chain(&error),
        r#"decode event kind: unknown event kind "trial.nope""#
    );
    let kind = std::error::Error::source(&error)
        .and_then(|source| source.downcast_ref::<UnknownKind>())
        .expect("the decode error keeps the unknown kind as its source");
    assert_eq!(kind.0, "trial.nope");
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
            DecodeError::UnknownField {
                kind: EventKind::SessionStart,
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
            DecodeError::UnknownField {
                kind: EventKind::SessionStart,
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
    assert_names_session_start(&togi_machine::error_chain(&error));
    let read = EventReader::new(format!("{line}\n").as_bytes())
        .next()
        .unwrap()
        .unwrap_err();
    assert_names_session_start(&togi_machine::error_chain(&read));

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
    assert_names_session_start(&togi_machine::error_chain(&read));
}

#[test]
fn envelope_errors_without_a_known_kind_do_not_invent_one() {
    let line = corrupted_first_line(|line| {
        line["kind"] = json!("trial.nope");
        line["seq"] = json!(0);
    });
    let error = Event::decode(&line).unwrap_err();
    assert!(
        matches!(error, DecodeError::Envelope { kind: None, .. }),
        "{error}"
    );
    let source = std::error::Error::source(&error)
        .and_then(|source| source.downcast_ref::<serde_json::Error>())
        .expect("the envelope error keeps the serde error as its source");
    assert_eq!(error.to_string(), "malformed event");
    assert_eq!(
        togi_machine::error_chain(&error),
        format!("malformed event: {source}")
    );
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
    let message = togi_machine::error_chain(&error);
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
    assert!(matches!(error.source, ReadError::Foreign), "{error}");
    assert_eq!(live(&fs), bytes);
}

#[test]
fn unknown_payload_field_names_the_kind() {
    for payload in samples() {
        let kind = payload.kind();
        let mut line = line_of(payload);
        line["data"]["bogus_field"] = json!(1);
        let error = Event::decode(&line.to_string()).unwrap_err();
        let DecodeError::UnknownField { kind: named, field } = &error else {
            panic!("{kind}: {error}");
        };
        assert_eq!(
            (*named, field.as_str()),
            (kind, "data.bogus_field"),
            "{kind}"
        );
        assert!(error.to_string().contains(kind.as_str()), "{kind}: {error}");
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
    let mut zero = bare;
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

#[test_case(EventKind::SessionStart, "cores", Some(0); "session start core")]
#[test_case(EventKind::CrashDetected, "pstore", None; "crash pstore")]
#[test_case(EventKind::TrialSignal, "schedule", None; "trial signal schedule")]
fn unknown_fields_in_nested_machine_types_are_refused(
    kind: EventKind,
    field: &str,
    index: Option<usize>,
) {
    let mut line = line_of(super::fixtures::sample(kind));
    let nested = match index {
        Some(index) => &mut line["data"][field][index],
        None => &mut line["data"][field],
    };
    assert!(nested.is_object(), "{kind}.{field}");
    nested["bogus"] = json!(1);
    let error = Event::decode(&line.to_string()).unwrap_err();
    assert!(
        matches!(error, DecodeError::UnknownField { kind: named, .. } if named == kind),
        "{kind}: {error}"
    );
    let path = index.map_or_else(
        || format!("data.{field}.bogus"),
        |index| format!("data.{field}[{index}].bogus"),
    );
    assert!(error.to_string().contains(&path), "{kind}: {error}");
}

#[test_case(EventKind::TrialIntent, "/data/workload"; "trial intent")]
#[test_case(EventKind::HuntStart, "/data/workload"; "hunt start")]
#[test_case(EventKind::CheckingChain, "/data/workload"; "checking chain")]
#[test_case(EventKind::TrialCarried, "/data/class/workload"; "trial carried class")]
#[test_case(EventKind::TunerDecision, "/data/workloads/0"; "tuner decision workloads")]
fn unknown_workload_ids_are_refused_wherever_a_payload_names_one(kind: EventKind, pointer: &str) {
    let mut line = line_of(super::fixtures::sample(kind));
    *line.pointer_mut(pointer).unwrap() = json!("not-a-workload");
    let error = Event::decode(&line.to_string()).unwrap_err();
    assert!(
        togi_machine::error_chain(&error).contains("not-a-workload"),
        "{kind}: {error}"
    );
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

#[test_case(EventKind::DefectFound; "defect found")]
#[test_case(EventKind::DefectAnswered; "defect answered")]
fn a_defect_id_of_zero_is_refused(kind: EventKind) {
    let mut line = line_of(super::fixtures::sample(kind));
    line["data"]["id"] = json!(0);
    assert!(Event::decode(&line.to_string()).is_err(), "{kind}");
}

#[test_case("-1.0"; "negative rate")]
#[test_case("1e999"; "nonfinite rate")]
fn a_cap_budget_that_is_not_a_nonnegative_finite_rate_is_refused(budget: &str) {
    let cap = samples()
        .into_iter()
        .find(|payload| matches!(payload, Payload::CcdCap(_)))
        .unwrap();
    let line = line_of(cap).to_string();
    let corrupted = line.replacen(
        r#""budget":0.00016666666666666666"#,
        &format!(r#""budget":{budget}"#),
        1,
    );
    assert_ne!(line, corrupted, "the sample's budget text changed");
    assert!(Event::decode(&corrupted).is_err());
}

#[test]
fn an_unknown_day_step_kind_is_refused() {
    let step = samples()
        .into_iter()
        .find(|payload| matches!(payload, Payload::DayStep(_)))
        .unwrap();
    let line = line_of(step)
        .to_string()
        .replacen(r#""kind":"promote""#, r#""kind":"leap""#, 1);
    assert!(Event::decode(&line).is_err());
}
