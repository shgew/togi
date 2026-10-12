//! Strict and lenient decoding of one line: what each refuses, what lenient skips or ignores, and that the two
//! share every other rule.

use proptest::prelude::*;
use serde_json::{Value, json};

use super::fixtures::{at, sample, samples};
use super::wire::arbitrary_event;
use crate::event::{Decoded, Decoding, Read, decode};
use crate::*;

fn line_of(payload: Payload) -> Value {
    let event = at(2, "boot-a", 0, &[1], payload);
    serde_json::from_str(&event.encode().unwrap()).unwrap()
}

fn lenient(line: &str) -> Result<Read, DecodeError> {
    decode(line, Decoding::Lenient)
}

fn lenient_event(line: &str) -> (Event, Vec<String>) {
    match lenient(line).unwrap() {
        Read {
            decoded: Decoded::Event(event),
            ignored,
        } => (event, ignored),
        Read { .. } => panic!("an event of a known kind was skipped: {line}"),
    }
}

fn join(path: &str, field: &str) -> String {
    if path.is_empty() {
        field.to_owned()
    } else {
        format!("{path}.{field}")
    }
}

/// Where an unknown field goes in `kind`'s line, as a JSON pointer to the object that gets it, and the path the
/// field is reported under.
const POSITIONS: [(EventKind, &str, &str); 6] = [
    (EventKind::TrialEnd, "", ""),
    (EventKind::TrialEnd, "/data", "data"),
    (EventKind::SessionStart, "/data/cores/0", "data.cores[0]"),
    (EventKind::SmuIntent, "/data", "data"),
    (EventKind::TrialSignal, "/data/schedule", "data.schedule"),
    (EventKind::CrashDetected, "/data/pstore", "data.pstore"),
];

#[test]
fn an_unknown_field_is_refused_by_strict_and_ignored_by_lenient_wherever_it_sits() {
    for (kind, pointer, at_path) in POSITIONS {
        let payload = sample(kind);
        let event = at(2, "boot-a", 0, &[1], payload.clone());
        let mut line = line_of(payload);
        let target = line.pointer_mut(pointer).unwrap();
        assert!(target.is_object(), "{kind} {pointer}");
        target["extra"] = json!(1);
        let text = line.to_string();
        let path = join(at_path, "extra");

        let error = Event::decode(&text).unwrap_err();
        assert!(
            matches!(&error, DecodeError::UnknownField { kind: named, field } if *named == kind && *field == path),
            "{kind} {pointer}: {error}"
        );
        assert_eq!(
            error.to_string(),
            format!("{kind} event: unknown field {path:?}")
        );

        let (decoded, ignored) = lenient_event(&text);
        assert_eq!(decoded, event, "{kind} {pointer}");
        assert_eq!(ignored, [path], "{kind} {pointer}");
    }
}

#[test]
fn lenient_reports_every_unknown_field_of_a_line_in_the_order_it_met_them() {
    let mut line = line_of(sample(EventKind::SessionStart));
    line["extra"] = json!(1);
    line["data"]["cores"][0]["wild"] = json!(2);
    line["data"]["also"] = json!(3);
    let event = at(2, "boot-a", 0, &[1], sample(EventKind::SessionStart));
    let (decoded, ignored) = lenient_event(&line.to_string());
    assert_eq!(decoded, event);
    assert_eq!(ignored, ["extra", "data.also", "data.cores[0].wild"]);
}

#[test]
fn an_unknown_kind_is_refused_by_strict_and_skipped_by_lenient() {
    let mut line = line_of(sample(EventKind::TrialEnd));
    line["kind"] = json!("future.thing");
    line["data"] = json!({"anything": [1, 2]});
    let text = line.to_string();

    let error = Event::decode(&text).unwrap_err();
    assert!(
        matches!(&error, DecodeError::UnknownKind(unknown) if unknown.0 == "future.thing"),
        "{error}"
    );

    let Read {
        decoded: Decoded::Unrecognized(skipped),
        ignored,
    } = lenient(&text).unwrap()
    else {
        panic!("a kind this build does not know was decoded");
    };
    assert_eq!(skipped.kind, "future.thing");
    assert_eq!(skipped.seq.get(), 2);
    assert_eq!(skipped.cause, [Seq::FIRST]);
    assert!(ignored.is_empty());
}

#[test]
fn an_unknown_kind_with_an_unknown_envelope_field_is_still_an_unknown_kind() {
    let mut line = line_of(sample(EventKind::TrialEnd));
    line["kind"] = json!("future.thing");
    line["extra"] = json!(1);
    let text = line.to_string();
    let error = Event::decode(&text).unwrap_err();
    assert!(matches!(error, DecodeError::UnknownKind(_)), "{error}");
    let Read {
        decoded: Decoded::Unrecognized(skipped),
        ignored,
    } = lenient(&text).unwrap()
    else {
        panic!("a kind this build does not know was decoded");
    };
    assert_eq!(skipped.kind, "future.thing");
    assert_eq!(ignored, ["extra"]);
}

#[test]
fn lenient_keeps_every_other_refusal_of_strict() {
    type Damage = Box<dyn Fn(&mut Value)>;
    let damaged: Vec<(&str, Damage)> = vec![
        ("seq zero", Box::new(|line| line["seq"] = json!(0))),
        (
            "no time",
            Box::new(|line| drop(line.as_object_mut().unwrap().remove("time"))),
        ),
        (
            "no kind",
            Box::new(|line| drop(line.as_object_mut().unwrap().remove("kind"))),
        ),
        (
            "no data",
            Box::new(|line| drop(line.as_object_mut().unwrap().remove("data"))),
        ),
        ("kind not text", Box::new(|line| line["kind"] = json!(7))),
        (
            "missing field",
            Box::new(|line| drop(line["data"].as_object_mut().unwrap().remove("trial"))),
        ),
        (
            "wrong type",
            Box::new(|line| line["data"]["trial"] = json!(7)),
        ),
        (
            "outside the type",
            Box::new(|line| line["data"]["duration_s"] = json!(-1)),
        ),
        (
            "unknown word",
            Box::new(|line| line["data"]["outcome"] = json!("sideways")),
        ),
        ("not an object", Box::new(|line| line["data"] = json!([1]))),
    ];
    for (why, damage) in damaged {
        for extra in [false, true] {
            let mut line = line_of(sample(EventKind::TrialEnd));
            damage(&mut line);
            if extra {
                line["extra"] = json!(1);
                if let Some(data) = line["data"].as_object_mut() {
                    data.insert("also".into(), json!(1));
                }
            }
            let text = line.to_string();
            assert!(
                Event::decode(&text).is_err(),
                "{why} (extra {extra}) strict"
            );
            assert!(lenient(&text).is_err(), "{why} (extra {extra}) lenient");
        }
    }
    for text in ["", "nope", "{", "[]", "null", r#"{"seq":1}"#] {
        assert!(Event::decode(text).is_err(), "{text:?} strict");
        assert!(lenient(text).is_err(), "{text:?} lenient");
    }
}

#[test]
fn lenient_does_not_hide_a_duplicate_key_behind_an_unknown_field() {
    let text = line_of(sample(EventKind::TrialEnd)).to_string();
    let doubled = text.replacen('{', r#"{"extra":1,"seq":9,"#, 1);
    assert!(doubled.contains(r#""extra":1,"seq":9,"#));
    assert!(Event::decode(&doubled).is_err());
    let error = lenient(&doubled).err().unwrap();
    assert!(error.to_string().contains("duplicate field"), "{error}");
}

#[test]
fn every_known_kind_has_the_same_strict_and_lenient_decode() {
    for payload in samples() {
        let kind = payload.kind();
        let text = line_of(payload).to_string();
        let strict = Event::decode(&text).unwrap();
        let (event, ignored) = lenient_event(&text);
        assert_eq!(event, strict, "{kind}");
        assert!(ignored.is_empty(), "{kind}");
    }
}

/// The objects of `value` that name their fields (not a core-keyed map), as a JSON pointer and a field path.
fn struct_objects(value: &Value, pointer: &str, path: &str, found: &mut Vec<(String, String)>) {
    match value {
        Value::Object(members) => {
            let keyed = members
                .keys()
                .all(|key| key.bytes().all(|byte| byte.is_ascii_digit()));
            if !members.is_empty() && !keyed {
                found.push((pointer.to_owned(), path.to_owned()));
            }
            for (key, child) in members {
                struct_objects(child, &format!("{pointer}/{key}"), &join(path, key), found);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                struct_objects(
                    child,
                    &format!("{pointer}/{index}"),
                    &format!("{path}[{index}]"),
                    found,
                );
            }
        }
        _ => {}
    }
}

proptest! {
    #[test]
    fn strict_and_lenient_decode_a_known_event_the_same_and_lenient_reports_nothing(
        event in arbitrary_event(),
    ) {
        let text = event.encode().unwrap();
        let strict = Event::decode(&text).unwrap();
        let (lenient, ignored) = lenient_event(&text);
        prop_assert_eq!(&strict, &event);
        prop_assert_eq!(lenient, event);
        prop_assert!(ignored.is_empty());
    }

    #[test]
    fn an_unknown_field_in_any_object_is_named_by_strict_and_ignored_by_lenient(
        event in arbitrary_event(),
        pick in any::<prop::sample::Index>(),
        name in "unknown_[0-9]{1,4}",
    ) {
        let positions = positions(&event);
        let (pointer, path) = &positions[pick.index(positions.len())];
        check_unknown_field(&event, pointer, path, &name).map_err(TestCaseError::fail)?;
    }
}

#[test]
fn an_unknown_field_in_every_object_of_every_kind_is_named_and_ignored() {
    let mut checked = 0;
    for payload in samples() {
        let event = at(2, "boot-a", 0, &[1], payload);
        for (pointer, path) in positions(&event) {
            check_unknown_field(&event, &pointer, &path, "unknown_1")
                .unwrap_or_else(|why| panic!("{why}"));
            checked += 1;
        }
    }
    assert!(checked > 100, "{checked}");
}

/// The objects of `event`'s line that take an unknown field: those that name their fields, and not a core-keyed map
/// or a one-key externally tagged enum.
fn positions(event: &Event) -> Vec<(String, String)> {
    let line: Value = serde_json::from_str(&event.encode().unwrap()).unwrap();
    let mut found = Vec::new();
    struct_objects(&line, "", "", &mut found);
    if event.kind() == EventKind::CommandReset {
        found.retain(|(_, path)| path != "data.target");
    }
    found
}

fn check_unknown_field(event: &Event, pointer: &str, path: &str, name: &str) -> Result<(), String> {
    let mut line: Value = serde_json::from_str(&event.encode().unwrap()).unwrap();
    line.pointer_mut(pointer)
        .and_then(Value::as_object_mut)
        .unwrap()
        .insert(name.to_owned(), json!(1));
    let text = line.to_string();
    let field = join(path, name);
    let kind = event.kind();
    match Event::decode(&text) {
        Err(DecodeError::UnknownField {
            kind: named,
            field: found,
        }) if named == kind && found == field => {}
        other => return Err(format!("{kind} {pointer}: strict gave {other:?}")),
    }
    match lenient(&text) {
        Ok(Read {
            decoded: Decoded::Event(decoded),
            ignored,
        }) if decoded == *event && ignored == [field] => Ok(()),
        other => Err(format!(
            "{kind} {pointer}: lenient gave {:?}",
            other.map(|read| read.ignored)
        )),
    }
}
