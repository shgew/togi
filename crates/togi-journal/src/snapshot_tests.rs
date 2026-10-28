use std::path::Path;

use serde_json::{Value, json};
use test_case::test_case;

use super::{Prefix, Rejected, read_in, seal, unseal, verify_prefix};
use crate::{MemFs, Seq};

const JOURNAL: &str = "/state/events.jsonl";
const STATE: &str = "/state/state.json";
const PREFIX: &[u8] = b" {\"seq\":1}\n";

fn stamp() -> Prefix {
    Prefix::of(PREFIX, Some(Seq::FIRST))
}

#[test]
fn readable_snapshot_is_tied_to_exact_raw_prefix_and_all_stamp_fields() {
    let fs = MemFs::new();
    let document = json!({"facts": {"last_seq": 1}, "incremental": {"cores": [1, 2]}});
    let sealed = seal(stamp(), &document).unwrap();
    fs.put(STATE, serde_json::to_vec(&sealed).unwrap());
    fs.put(JOURNAL, [PREFIX, b"{\"seq\":2}\n"].concat());
    let before = fs.mutations();
    let (prefix, decoded): (Prefix, Value) = read_in(&fs, Path::new("/state")).unwrap().unwrap();
    assert_eq!(decoded, document);
    assert_eq!(prefix, stamp());
    assert_eq!(fs.mutations(), before);
}

#[test_case("/facts/last_seq", json!(2), "snapshot content checksum differs"; "payload")]
#[test_case("/snapshot/prefix/bytes", json!(1), "snapshot content checksum differs"; "prefix boundary")]
#[test_case("/snapshot/prefix/last_seq", json!(2), "snapshot content checksum differs"; "sequence")]
#[test_case("/snapshot/build", json!("another-build"), "snapshot belongs to another build"; "build")]
#[test_case("/snapshot/sha256", json!("tampered"), "snapshot content checksum differs"; "checksum")]
fn altered_snapshot_is_rejected(pointer: &str, change: Value, expected: &str) {
    let mut saved = seal(stamp(), &json!({"facts": {"last_seq": 1}})).unwrap();
    *saved.pointer_mut(pointer).unwrap() = change;
    let error = unseal::<Value>(saved).unwrap_err();
    assert_eq!(error.to_string(), expected);
}

#[test_case(b" {\"seq\":9}\n"; "changed bytes")]
#[test_case(b" {\"seq\":1}"; "truncation")]
#[test_case(b"\n"; "wrong line count")]
fn changed_or_truncated_prefix_is_rejected(bytes: &[u8]) {
    let fs = MemFs::new();
    fs.put(JOURNAL, bytes);
    assert!(matches!(
        verify_prefix(&fs, Path::new(JOURNAL), &stamp()),
        Err(Rejected::Prefix)
    ));
}

#[test]
fn boundary_must_end_on_complete_line_and_match_sequence() {
    let fs = MemFs::new();
    fs.put(JOURNAL, PREFIX);
    let boundary = Prefix::of(&PREFIX[..PREFIX.len() - 1], Some(Seq::FIRST));
    assert!(matches!(
        verify_prefix(&fs, Path::new(JOURNAL), &boundary),
        Err(Rejected::Prefix)
    ));
    let sequence = Prefix::of(PREFIX, None);
    assert!(matches!(
        verify_prefix(&fs, Path::new(JOURNAL), &sequence),
        Err(Rejected::Prefix)
    ));
}

#[test]
fn absence_is_not_a_read_failure_or_a_write() {
    let fs = MemFs::new();
    let before = fs.mutations();
    assert!(
        read_in::<Value>(&fs, Path::new("/state"))
            .unwrap()
            .is_none()
    );
    assert_eq!(fs.mutations(), before);
}

#[test_case(true; "stamp array")]
#[test_case(false; "prefix array")]
fn positional_verification_metadata_is_rejected(stamp_array: bool) {
    let mut saved = seal(stamp(), &json!({"facts": {"last_seq": 1}})).unwrap();
    let metadata = saved["snapshot"].clone();
    if stamp_array {
        saved["snapshot"] = json!([metadata["build"], metadata["prefix"], metadata["sha256"]]);
    } else {
        let prefix = &metadata["prefix"];
        saved["snapshot"]["prefix"] =
            json!([prefix["last_seq"], prefix["bytes"], prefix["sha256"]]);
    }
    assert!(matches!(unseal::<Value>(saved), Err(Rejected::Decode(_))));
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Reducers {
    nested: Reducer,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Reducer {
    count: u32,
}

#[test_case(&json!({"nested": [1]}); "sized nested array")]
#[test_case(&json!({"nested": {"count": 1, "future": true}}); "unknown nested key")]
fn resealed_noncanonical_reducer_shapes_are_rejected(document: &Value) {
    let saved = seal(stamp(), document).unwrap();
    assert!(matches!(
        unseal::<Reducers>(saved),
        Err(Rejected::ReducerShape)
    ));
}

#[test]
fn partial_reader_checks_the_shape_of_every_field_it_restores() {
    let saved = seal(
        stamp(),
        &json!({"nested": {"count": 1}, "other_reader": {"extra": true}}),
    )
    .unwrap();
    let (_, restored) = unseal::<Reducers>(saved).unwrap();
    assert_eq!(restored.nested.count, 1);
}

#[test_case(None, None, None, None; "intact")]
#[test_case(Some(("/nested/count", json!(2))), None, None, Some("snapshot content checksum differs"); "changed content")]
#[test_case(Some(("/snapshot/build", json!("another-build"))), None, None, Some("snapshot belongs to another build"); "another build")]
#[test_case(None, Some(&b" {\"seq\":9}\n"[..]), None, Some("snapshot journal prefix differs"); "changed prefix")]
#[test_case(None, None, Some(json!({"nested": [1]})), Some("snapshot reducer representation differs from this build's shape"); "resealed reducer shape")]
fn an_already_read_document_gets_every_check_of_a_snapshot_read(
    edit: Option<(&str, Value)>,
    journal: Option<&[u8]>,
    resealed: Option<Value>,
    rejected: Option<&str>,
) {
    let fs = MemFs::new();
    let mut saved = seal(
        stamp(),
        &resealed.unwrap_or_else(|| json!({"nested": {"count": 1}})),
    )
    .unwrap();
    if let Some((pointer, change)) = edit {
        *saved.pointer_mut(pointer).unwrap() = change;
    }
    fs.put(STATE, serde_json::to_vec(&saved).unwrap());
    fs.put(JOURNAL, journal.unwrap_or(PREFIX));
    let dir = crate::testing::simulated(&fs);
    let before = fs.mutations();
    let read = dir.read_snapshot::<Reducers>().map(Option::unwrap);
    let checked = dir.verify_snapshot_document::<Reducers>(saved);
    assert_eq!(format!("{checked:?}"), format!("{read:?}"));
    assert_eq!(
        checked.err().map(|error| error.to_string()).as_deref(),
        rejected
    );
    assert_eq!(fs.mutations(), before);
}
