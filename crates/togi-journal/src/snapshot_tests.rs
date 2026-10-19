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
