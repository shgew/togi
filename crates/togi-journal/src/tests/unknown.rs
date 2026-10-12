//! Finding an unknown field at any depth, among them inside an internally tagged enum, where serde's error path stops
//! at the enum's object, and cutting it out of the line.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::event::{Decoded, Decoding, Read, decode};
use crate::unknown::{Step, Unknown, is_unknown_field, locate};
use crate::*;

use super::fixtures::{at, sample};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(dead_code, reason = "decoded to be refused or accepted, never read")]
struct Leaf {
    id: u32,
    label: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[expect(dead_code, reason = "decoded to be refused or accepted, never read")]
enum Shape {
    Dot {
        leaf: Leaf,
        #[serde(default)]
        leaves: Vec<Leaf>,
    },
    Nest {
        shape: Box<Shape>,
        label: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[expect(dead_code, reason = "decoded to be refused or accepted, never read")]
struct Holder {
    shape: Shape,
    label: String,
}

/// What the decode of a `Holder` line does about its unknown fields: each one's path as the lenient decode loop
/// reports it, and the line that is left once they are all cut out.
fn strip(line: &str) -> (Vec<String>, String) {
    let mut text = line.to_owned();
    let mut found = Vec::new();
    loop {
        let tracked = serde_path_to_error::deserialize::<_, Holder>(
            &mut serde_json::Deserializer::from_str(&text),
        );
        let Err(error) = tracked else {
            return (found, text);
        };
        assert!(
            is_unknown_field(error.inner()),
            "{text}: not an unknown field: {error}"
        );
        let unknown: Unknown = locate(&text, &[], &error, |text| {
            serde_json::from_str::<Holder>(text).map(drop)
        })
        .unwrap_or_else(|| panic!("{text}: not located: {error}"));
        found.push(unknown.path());
        text = unknown
            .remove_from(&text)
            .unwrap_or_else(|| panic!("{text}: not cut: {}", unknown.path()));
    }
}

fn compact(value: &Value) -> String {
    value.to_string()
}

fn dot(leaf: &Value) -> Value {
    json!({"kind": "dot", "leaf": leaf, "leaves": []})
}

#[test]
fn an_unknown_field_in_a_struct_inside_a_tagged_variant_is_located() {
    let leaf = json!({"id": 1, "label": "a"});
    let marked = json!({"id": 1, "label": "a", "extra": 2});
    let holder = |shape: Value| json!({"shape": shape, "label": "x"});

    let (found, left) = strip(&compact(&holder(dot(&marked))));
    assert_eq!(found, ["shape.leaf.extra"]);
    assert_eq!(left, compact(&holder(dot(&leaf))));

    let mut shape = dot(&leaf);
    shape["leaves"] = json!([leaf, marked]);
    let (found, left) = strip(&compact(&holder(shape.clone())));
    assert_eq!(found, ["shape.leaves[1].extra"]);
    shape["leaves"][1] = leaf.clone();
    assert_eq!(left, compact(&holder(shape)));
}

#[test]
fn an_unknown_field_two_tagged_variants_deep_is_located() {
    let nest = |shape: Value| json!({"kind": "nest", "shape": shape, "label": "n"});
    let marked = json!({"id": 1, "label": "a", "extra": 2});
    let leaf = json!({"id": 1, "label": "a"});
    let line = json!({"shape": nest(nest(dot(&marked))), "label": "x"});
    let (found, left) = strip(&compact(&line));
    assert_eq!(found, ["shape.shape.shape.leaf.extra"]);
    assert_eq!(
        left,
        compact(&json!({"shape": nest(nest(dot(&leaf))), "label": "x"}))
    );
}

#[test]
fn an_unknown_field_is_told_from_a_known_field_of_the_same_name_before_it() {
    // `label` is known to the outer `nest`, to the leaf met before the refused field, and to the holder after it.
    let line = r#"{"shape":{"kind":"nest","label":"v","shape":{"kind":"dot","leaf":{"id":1,"label":"a"},"leaves":[],"label":"bad"}},"label":"x"}"#;
    let (found, left) = strip(line);
    assert_eq!(found, ["shape.shape.label"]);
    assert_eq!(
        left,
        r#"{"shape":{"kind":"nest","label":"v","shape":{"kind":"dot","leaf":{"id":1,"label":"a"},"leaves":[]}},"label":"x"}"#
    );
}

#[test]
fn an_unknown_field_is_told_from_a_known_field_of_the_same_name_after_it() {
    let line = r#"{"shape":{"kind":"nest","label":"v","shape":{"kind":"dot","label":"bad","leaf":{"id":1,"label":"a"},"leaves":[{"id":2,"label":"b"}]}},"label":"x"}"#;
    let (found, left) = strip(line);
    assert_eq!(found, ["shape.shape.label"]);
    assert_eq!(
        left,
        r#"{"shape":{"kind":"nest","label":"v","shape":{"kind":"dot","leaf":{"id":1,"label":"a"},"leaves":[{"id":2,"label":"b"}]}},"label":"x"}"#
    );
}

#[test]
fn an_unknown_field_named_like_the_tagged_enums_own_field_is_the_one_cut() {
    let line = r#"{"shape":{"kind":"dot","leaf":{"id":1,"label":"a"},"shape":0},"label":"x"}"#;
    let (found, left) = strip(line);
    assert_eq!(found, ["shape.shape"]);
    assert_eq!(
        left,
        r#"{"shape":{"kind":"dot","leaf":{"id":1,"label":"a"}},"label":"x"}"#
    );
}

#[test]
fn every_unknown_field_of_a_line_is_located_in_the_order_met() {
    let line = r#"{"shape":{"kind":"nest","one":1,"shape":{"kind":"dot","leaf":{"id":1,"two":2,"label":"a"},"leaves":[{"label":"b","id":2,"three":3}],"four":4}},"five":5,"label":"x"}"#;
    let (found, left) = strip(line);
    assert_eq!(
        found,
        [
            "shape.one",
            "shape.shape.leaf.two",
            "shape.shape.leaves[0].three",
            "shape.shape.four",
            "five"
        ]
    );
    assert_eq!(
        left,
        r#"{"shape":{"kind":"nest","shape":{"kind":"dot","leaf":{"id":1,"label":"a"},"leaves":[{"label":"b","id":2}]}},"label":"x"}"#
    );
}

#[test]
fn an_escaped_field_name_is_located_and_cut_at_any_depth() {
    let line = r#"{"shape":{"kind":"dot","leaf":{"id":1,"w\"i\\r\nd\u00e9 `, x":1,"label":"a"}},"label":"x"}"#;
    let (found, left) = strip(line);
    assert_eq!(found, ["shape.leaf.w\"i\\r\nd\u{e9} `, x"]);
    assert_eq!(
        left,
        r#"{"shape":{"kind":"dot","leaf":{"id":1,"label":"a"}},"label":"x"}"#
    );
}

#[test]
fn a_name_that_is_the_start_of_another_is_not_confused() {
    // `lab` is refused; `label` and `labels` are known or unknown by their own name.
    let line = r#"{"shape":{"kind":"dot","leaf":{"id":1,"lab":1,"label":"a"}},"label":"x"}"#;
    let (found, left) = strip(line);
    assert_eq!(found, ["shape.leaf.lab"]);
    assert_eq!(
        left,
        r#"{"shape":{"kind":"dot","leaf":{"id":1,"label":"a"}},"label":"x"}"#
    );
}

#[test]
fn a_repeated_unknown_field_is_cut_everywhere_it_repeats_and_other_repeats_are_kept() {
    let line =
        r#"{"shape":{"kind":"dot","leaf":{"id":1,"extra":1,"label":"a","extra":2}},"label":"x"}"#;
    let (found, left) = strip(line);
    assert_eq!(found, ["shape.leaf.extra"]);
    assert_eq!(
        left,
        r#"{"shape":{"kind":"dot","leaf":{"id":1,"label":"a"}},"label":"x"}"#
    );

    // A repeated known field is not an unknown field, so it is not cut and still fails the decode.
    let doubled =
        r#"{"shape":{"kind":"dot","leaf":{"id":1,"extra":1,"label":"a","id":2}},"label":"x"}"#;
    let error = serde_json::from_str::<Holder>(doubled).unwrap_err();
    assert!(is_unknown_field(&error), "{error}");
    let tracked = serde_path_to_error::deserialize::<_, Holder>(
        &mut serde_json::Deserializer::from_str(doubled),
    )
    .unwrap_err();
    let unknown = locate(doubled, &[], &tracked, |text| {
        serde_json::from_str::<Holder>(text).map(drop)
    })
    .unwrap();
    let left = unknown.remove_from(doubled).unwrap();
    assert_eq!(
        left,
        r#"{"shape":{"kind":"dot","leaf":{"id":1,"label":"a","id":2}},"label":"x"}"#
    );
    let error = serde_json::from_str::<Holder>(&left).unwrap_err();
    assert!(error.to_string().contains("duplicate field"), "{error}");
}

#[test]
fn a_prefix_places_the_value_inside_the_line() {
    let line = r#"{"wrapper":[0,{"shape":{"kind":"dot","leaf":{"id":1,"label":"a","extra":1}},"label":"x"}]}"#;
    let value = r#"{"shape":{"kind":"dot","leaf":{"id":1,"label":"a","extra":1}},"label":"x"}"#;
    let tracked = serde_path_to_error::deserialize::<_, Holder>(
        &mut serde_json::Deserializer::from_str(value),
    )
    .unwrap_err();
    let prefix = [Step::Key("wrapper".to_owned()), Step::Index(1)];
    let unknown = locate(line, &prefix, &tracked, |text| {
        serde_json::from_str::<Holder>(text).map(drop)
    })
    .unwrap();
    assert_eq!(unknown.path(), "wrapper[1].shape.leaf.extra");
}

/// A lenient decode of `line`, which is the line of a `trial.signal` event whose `schedule` object is `schedule`.
fn signal_line(schedule: Value) -> String {
    let event = at(2, "boot-a", 0, &[1], sample(EventKind::TrialSignal));
    let mut line: Value = serde_json::from_str(&event.encode().unwrap()).unwrap();
    line["data"]["schedule"] = schedule;
    line.to_string()
}

fn signal_event() -> Event {
    at(2, "boot-a", 0, &[1], sample(EventKind::TrialSignal))
}

#[test]
fn an_unknown_field_named_like_a_known_one_of_the_enclosing_objects_is_named_and_ignored() {
    // Each name is a field of `trial.signal`'s data, of its envelope, or of the other schedule variant.
    for name in [
        "schedule", "trial", "stops", "conts", "pct", "seed2", "data", "msg", "boot", "time",
    ] {
        let schedule = json!({"kind": "random", "seed": 99, name: 1});
        let text = signal_line(schedule);
        let path = format!("data.schedule.{name}");

        let error = Event::decode(&text).unwrap_err();
        assert!(
            matches!(&error, DecodeError::UnknownField { kind: EventKind::TrialSignal, field } if *field == path),
            "{name}: {error}"
        );
        let Read {
            decoded: Decoded::Event(event),
            ignored,
        } = decode(&text, Decoding::Lenient).unwrap()
        else {
            panic!("{name}: skipped");
        };
        assert_eq!(event, signal_event(), "{name}");
        assert_eq!(ignored, [path], "{name}");
    }
}

#[test]
fn an_unknown_field_before_the_known_ones_of_a_tagged_object_is_named_and_ignored() {
    // The unknown field is met before `seed`, and its name is the schedule's own field in the enclosing data.
    let text = signal_line(json!({"kind": "random", "seed": 99}))
        .replace(r#"{"kind":"random""#, r#"{"schedule":1,"kind":"random""#);
    assert!(text.contains(r#"{"schedule":1,"kind":"random","seed":99}"#));
    let error = Event::decode(&text).unwrap_err();
    assert!(
        matches!(&error, DecodeError::UnknownField { field, .. } if field == "data.schedule.schedule"),
        "{error}"
    );
    let Read {
        decoded: Decoded::Event(event),
        ignored,
    } = decode(&text, Decoding::Lenient).unwrap()
    else {
        panic!("skipped");
    };
    assert_eq!(event, signal_event());
    assert_eq!(ignored, ["data.schedule.schedule"]);
}

#[test]
fn an_escaped_unknown_field_in_a_tagged_object_is_named_and_ignored() {
    let name = "w\"i\\r\nd\u{e9}";
    let text = signal_line(json!({"kind": "random", "seed": 99, name: 1}));
    let path = format!("data.schedule.{name}");
    let error = Event::decode(&text).unwrap_err();
    assert!(
        matches!(&error, DecodeError::UnknownField { field, .. } if *field == path),
        "{error}"
    );
    let Read {
        decoded: Decoded::Event(event),
        ignored,
    } = decode(&text, Decoding::Lenient).unwrap()
    else {
        panic!("skipped");
    };
    assert_eq!(event, signal_event());
    assert_eq!(ignored, [path]);
}

#[test]
fn a_repeated_field_in_a_tagged_object_is_still_refused_by_both() {
    for text in [
        signal_line(json!({"kind": "random", "seed": 99}))
            .replace(r#""seed":99"#, r#""extra":1,"seed":99,"seed":98"#),
        signal_line(json!({"kind": "random", "seed": 99}))
            .replace(r#""seed":99"#, r#""seed":99,"seed":98,"extra":1"#),
    ] {
        assert!(Event::decode(&text).is_err(), "{text}");
        let error = decode(&text, Decoding::Lenient).err().unwrap();
        assert!(error.to_string().contains("duplicate field"), "{error}");
    }
}

#[test]
fn an_unknown_field_in_a_smu_command_is_named_and_ignored_whatever_its_name() {
    let payload = sample(EventKind::SmuIntent);
    let event = at(2, "boot-a", 0, &[1], payload);
    for name in ["extra", "data", "kind", "msg", "seq"] {
        let mut line: Value = serde_json::from_str(&event.encode().unwrap()).unwrap();
        let data = line["data"].as_object_mut().unwrap();
        assert!(!data.contains_key(name), "{name} is known to the command");
        data.insert(name.to_owned(), json!(1));
        let text = line.to_string();
        let path = format!("data.{name}");
        let error = Event::decode(&text).unwrap_err();
        assert!(
            matches!(&error, DecodeError::UnknownField { field, .. } if *field == path),
            "{name}: {error}"
        );
        let Read {
            decoded: Decoded::Event(decoded),
            ignored,
        } = decode(&text, Decoding::Lenient).unwrap()
        else {
            panic!("{name}: skipped");
        };
        assert_eq!(decoded, event, "{name}");
        assert_eq!(ignored, [path], "{name}");
    }
}
