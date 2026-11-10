use std::path::Path;

use serde_json::{Value, json};
use togi_journal::fs::MemFs;

use super::{Comparison, compare_with};

fn event(seq: u64, kind: &str, data: Value) -> Value {
    json!({"seq": seq, "kind": kind, "msg": kind, "data": data})
}

fn put(fs: &MemFs, path: &str, events: &[Value]) {
    let mut bytes = Vec::new();
    for event in events {
        serde_json::to_writer(&mut bytes, event).unwrap();
        bytes.push(b'\n');
    }
    fs.put(Path::new(path), bytes);
}

fn compare(base: &[Value], head: &[Value]) -> Result<Comparison, super::JournalError> {
    let fs = MemFs::new();
    put(&fs, "/base/events.jsonl", base);
    put(&fs, "/head/events.jsonl", head);
    compare_with(&fs, Path::new("/base"), Path::new("/head"))
}

fn histories() -> [Vec<Value>; 2] {
    let start = event(1, "session.start", json!({"session": "s"}));
    let cap = event(2, "ccd.cap", json!({"new": -5}));
    let base_edge = event(3, "unit.edge", json!({"ceiling": -10, "witnesses": [1]}));
    let head_edge = event(2, "unit.edge", json!({"ceiling": -10, "witnesses": [1]}));
    let mut base_target = event(
        4,
        "trial.start",
        json!({"target": {"0": -10}, "certificate": 3}),
    );
    let mut head_target = event(
        3,
        "trial.start",
        json!({"target": {"0": -10}, "certificate": 2}),
    );
    base_target["cause"] = json!([3]);
    head_target["cause"] = json!([2]);
    [
        vec![start.clone(), cap, base_edge, base_target],
        vec![start, head_edge, head_target],
    ]
}

#[test]
fn only_base_caps_and_sequence_identity_are_removed() {
    let [base, head] = histories();
    let comparison = compare(&base, &head).unwrap();
    assert_eq!(comparison.first, None);
    assert_eq!(comparison.summary.dropped_caps, 1);
    assert_eq!(comparison.summary.classes["trial.start"].identical, 1);
    assert_eq!(comparison.summary.classes["unit.edge"].identical, 1);
    assert_eq!(comparison.summary.classes.contains_key("ccd.cap"), false);
}

#[test]
fn a_changed_target_fails_semantic_equality() {
    let [base, mut head] = histories();
    head[2]["data"]["target"]["0"] = json!(-11);
    let comparison = compare(&base, &head).unwrap();
    assert!(comparison.first.is_some());
    assert_eq!(comparison.summary.classes["trial.start"].differing, 1);
    assert_eq!(
        comparison.summary.classes["trial.start"].evidence_differing,
        1
    );
}

#[test]
fn changed_permission_fails_semantic_equality() {
    let [base, mut head] = histories();
    head[1]["data"]["ceiling"] = json!(-9);
    let comparison = compare(&base, &head).unwrap();
    assert!(comparison.first.is_some());
    assert_eq!(comparison.summary.classes["unit.edge"].differing, 1);
    let mut report = Vec::new();
    comparison.summary.write(&mut report).unwrap();
    assert!(
        String::from_utf8(report)
            .unwrap()
            .contains("permission evidence different")
    );
}

#[test_case::test_case("cause"; "envelope cause")]
#[test_case::test_case("witnesses"; "payload witnesses")]
#[test_case::test_case("caps"; "payload cap evidence")]
fn a_dependency_on_a_removed_cap_is_an_error(field: &str) {
    let [mut base, head] = histories();
    if field == "cause" {
        base[3][field] = json!([2]);
    } else {
        base[3]["data"][field] = json!([2]);
    }
    let error = compare(&base, &head).err().unwrap().to_string();
    assert!(error.contains("references removed ccd.cap"), "{error}");
}

#[test_case::test_case("night.plan", json!({}), true; "plan cursor")]
#[test_case::test_case("night.qualification", json!({"state":{"kind":"start"}}), true; "qualification cutoff")]
#[test_case::test_case("deadend", json!({"condition":"failure_at_zero"}), true; "stock failure cursor")]
#[test_case::test_case("night.qualification", json!({"state":{"kind":"complete"}}), false; "qualification evidence")]
#[test_case::test_case("deadend", json!({"condition":"smu"}), false; "other dead end")]
#[test_case::test_case("deadend", json!({"condition":"failure_at_zero","core":0}), false; "attributed dead end")]
#[test_case::test_case("deadend", json!({"condition":"failure_at_zero","action":"exit"}), false; "session dead end")]
#[test_case::test_case("trial.start", json!({}), false; "other commitment")]
fn last_event_cursor_producer_table_is_closed(kind: &str, data: Value, allowed: bool) {
    let start = event(1, "session.start", json!({"session":"s"}));
    let mut record = event(3, kind, data);
    record["cause"] = json!([2]);
    let mut head_record = record.clone();
    head_record["seq"] = json!(2);
    head_record["cause"] = json!([1]);
    let base = [start.clone(), event(2, "ccd.cap", json!({})), record];
    let head = [start, head_record];
    if allowed {
        let result = compare(&base, &head).unwrap();
        assert!(result.first.is_none());
        assert_eq!(result.summary.rebased_cursor_causes, 1);
    } else {
        assert!(
            compare(&base, &head)
                .err()
                .unwrap()
                .to_string()
                .contains("references removed ccd.cap")
        );
    }
}

#[test]
fn a_night_plan_latest_event_cursor_crosses_only_its_immediately_preceding_cap() {
    let start = event(1, "session.start", json!({"session": "s"}));
    let fact = event(2, "exposure.observed", json!({}));
    let cap = event(3, "ccd.cap", json!({}));
    let mut plan = event(4, "night.plan", json!({"night": 1, "tasks": []}));
    plan["cause"] = json!([1, 3]);
    let mut head_plan = plan.clone();
    head_plan["seq"] = json!(3);
    head_plan["cause"] = json!([1, 2]);
    let mut base = vec![start.clone(), fact.clone(), cap, plan];
    let head = vec![start, fact, head_plan];
    let result = compare(&base, &head).unwrap();
    assert!(result.first.is_none());
    assert_eq!(result.summary.rebased_cursor_causes, 1);
    assert_eq!(result.summary.classes["night.plan"].identical, 1);

    // Explicit payload evidence is never a latest-event cursor.
    base[3]["data"]["witnesses"] = json!([3]);
    assert!(
        compare(&base, &head)
            .err()
            .unwrap()
            .to_string()
            .contains("references removed ccd.cap")
    );
    base[3]["data"].as_object_mut().unwrap().remove("witnesses");
    // A nonadjacent cap cause is not the State::last dependency either.
    base.insert(3, event(4, "task.start", json!({})));
    base[4]["seq"] = json!(5);
    assert!(
        compare(&base, &head)
            .err()
            .unwrap()
            .to_string()
            .contains("references removed ccd.cap")
    );
}

#[test]
fn head_caps_are_never_dropped() {
    let [base, mut head] = histories();
    head.push(event(4, "ccd.cap", json!({"new": -5})));
    let comparison = compare(&base, &head).unwrap();
    assert!(comparison.first.is_some());
    assert_eq!(comparison.summary.dropped_caps, 1);
    assert_eq!(comparison.summary.classes["ccd.cap"].head, 1);
    assert_eq!(comparison.summary.classes["ccd.cap"].differing, 1);
}

#[test]
fn message_only_differences_are_counted_and_fail_equality() {
    let [base, mut head] = histories();
    head[1]["msg"] = json!("different permission explanation");
    head[2]["msg"] = json!("different target explanation");
    let comparison = compare(&base, &head).unwrap();
    assert!(comparison.first.is_some());
    assert_eq!(comparison.summary.classes["unit.edge"].messages, 1);
    assert_eq!(comparison.summary.classes["trial.start"].messages, 1);
    assert_eq!(
        comparison.summary.classes["unit.edge"].evidence_differing,
        0
    );
    let mut report = Vec::new();
    comparison.summary.write(&mut report).unwrap();
    let report = String::from_utf8(report).unwrap();
    assert!(report.contains("permission evidence identical"));
    assert!(report.contains("message_differences=1"));
}

#[test]
fn unrelated_classes_are_compared_not_filtered() {
    let [mut base, mut head] = histories();
    base.push(event(5, "shutdown", json!({"reason": "command"})));
    head.push(event(4, "shutdown", json!({"reason": "different"})));
    let comparison = compare(&base, &head).unwrap();
    assert!(comparison.first.is_some());
    assert_eq!(comparison.summary.classes["shutdown"].differing, 1);
}

#[test]
fn canonicalization_applies_to_both_sides_and_preserves_ordinals() {
    let base = [
        event(10, "session.start", json!({"session": "s"})),
        event(20, "ccd.cap", json!({})),
        event(
            30,
            "climb.decision",
            json!({"decision": 17, "application": 23, "night": 10, "hold_s": 30}),
        ),
    ];
    let head = [
        event(7, "session.start", json!({"session": "s"})),
        event(
            50,
            "climb.decision",
            json!({"decision": 17, "application": 23, "night": 7, "hold_s": 30}),
        ),
    ];
    assert_eq!(compare(&base, &head).unwrap().first, None);
}

#[test]
fn archived_session_provenance_uses_its_own_identity_map() {
    let fs = MemFs::new();
    put(
        &fs,
        "/base/archive/old.jsonl",
        &[
            event(1, "session.start", json!({"session": "old"})),
            event(2, "ccd.cap", json!({})),
            event(3, "exposure.observed", json!({})),
        ],
    );
    put(
        &fs,
        "/head/archive/old.jsonl",
        &[
            event(1, "session.start", json!({"session": "old"})),
            event(2, "exposure.observed", json!({})),
        ],
    );
    put(
        &fs,
        "/base/events.jsonl",
        &[
            event(1, "session.start", json!({"session": "new"})),
            event(
                2,
                "exposure.observed",
                json!({"source": {"session": "old", "seq": 3}}),
            ),
        ],
    );
    put(
        &fs,
        "/head/events.jsonl",
        &[
            event(1, "session.start", json!({"session": "new"})),
            event(
                2,
                "exposure.observed",
                json!({"source": {"session": "old", "seq": 2}}),
            ),
        ],
    );
    assert_eq!(
        compare_with(&fs, Path::new("/base"), Path::new("/head"))
            .unwrap()
            .first,
        None
    );
}

#[test]
fn external_provenance_is_not_mistaken_for_a_local_reference() {
    let start = event(1, "session.start", json!({"session": "s"}));
    let observed = event(
        2,
        "exposure.observed",
        json!({"source": {"session": "external", "seq": 800}}),
    );
    assert_eq!(
        compare(&[start.clone(), observed.clone()], &[start, observed])
            .unwrap()
            .first,
        None
    );
}

#[test]
fn build_normalization_retains_ruleset_and_evidence_epoch() {
    let build = |version, evidence| {
        let mut start = event(
            1,
            "session.start",
            json!({"session": "s", "build": {"version": version, "rev": version, "ruleset": 14, "schema": 1, "evidence": evidence}}),
        );
        start["msg"] = json!(format!(
            "session s started by togi {version}+{version} (schema 1, ruleset 14)"
        ));
        start
    };
    assert_eq!(
        compare(&[build("old", 5)], &[build("new", 5)])
            .unwrap()
            .first,
        None
    );
    assert!(
        compare(&[build("old", 5)], &[build("new", 6)])
            .unwrap()
            .first
            .is_some()
    );
}

#[test]
fn missing_archives_are_differences_even_when_empty() {
    let fs = MemFs::new();
    let start = event(1, "session.start", json!({"session": "s"}));
    put(&fs, "/base/events.jsonl", &[start.clone()]);
    put(&fs, "/head/events.jsonl", &[start]);
    put(&fs, "/base/archive/empty.jsonl", &[]);
    let compared = compare_with(&fs, Path::new("/base"), Path::new("/head")).unwrap();
    assert_eq!(compared.first.unwrap().head, "<missing file>");
}

#[test_case::test_case("night")]
#[test_case::test_case("last_checkpoint")]
#[test_case::test_case("witness")]
#[test_case::test_case("witnesses")]
#[test_case::test_case("exposure")]
#[test_case::test_case("failure")]
#[test_case::test_case("failures")]
#[test_case::test_case("retreat")]
#[test_case::test_case("evidence")]
#[test_case::test_case("caps")]
#[test_case::test_case("qualification")]
#[test_case::test_case("vetoes")]
#[test_case::test_case("events")]
#[test_case::test_case("hours")]
#[test_case::test_case("transactions")]
#[test_case::test_case("credited")]
#[test_case::test_case("checkpoints")]
#[test_case::test_case("edges")]
#[test_case::test_case("certificate")]
#[test_case::test_case("ceilings")]
#[test_case::test_case("decisions")]
#[test_case::test_case("known_failure")]
#[test_case::test_case("in_flight")]
#[test_case::test_case("source_seqs")]
#[test_case::test_case("parked_seq")]
#[test_case::test_case("base_seq")]
#[test_case::test_case("passes")]
#[test_case::test_case("causes")]
fn nested_payload_reference_fields_are_rebased(field: &str) {
    let [mut base, mut head] = histories();
    base[3]["data"]["nested"] = json!({(field): [3]});
    head[2]["data"]["nested"] = json!({(field): [2]});
    assert_eq!(compare(&base, &head).unwrap().first, None, "{field}");
}

#[test]
fn unresolved_local_references_fail_instead_of_becoming_null() {
    let [base, mut head] = histories();
    head[2]["cause"] = json!([99]);
    assert!(
        compare(&base, &head)
            .err()
            .unwrap()
            .to_string()
            .contains("unresolved cause reference")
    );
}

#[test]
fn imported_raw_payload_references_use_the_original_session() {
    let fs = MemFs::new();
    put(
        &fs,
        "/base/archive/old.jsonl",
        &[
            event(1, "session.start", json!({"session": "old"})),
            event(2, "ccd.cap", json!({})),
            event(3, "unit.edge", json!({})),
        ],
    );
    put(
        &fs,
        "/head/archive/old.jsonl",
        &[
            event(1, "session.start", json!({"session": "old"})),
            event(2, "unit.edge", json!({})),
        ],
    );
    put(
        &fs,
        "/base/events.jsonl",
        &[
            event(1, "session.start", json!({"session": "new"})),
            event(
                2,
                "day.imported",
                json!({"original": {"session": "old", "seq": 3}, "original_kind": "day.step", "raw": "{\"causes\":{\"edges\":[3]},\"decision\":10}"}),
            ),
        ],
    );
    put(
        &fs,
        "/head/events.jsonl",
        &[
            event(1, "session.start", json!({"session": "new"})),
            event(
                2,
                "day.imported",
                json!({"original": {"session": "old", "seq": 2}, "original_kind": "day.step", "raw": "{\"decision\":10,\"causes\":{\"edges\":[2]}}"}),
            ),
        ],
    );
    assert_eq!(
        compare_with(&fs, Path::new("/base"), Path::new("/head"))
            .unwrap()
            .first,
        None
    );
}

#[test]
fn record_order_is_compared_across_classes() {
    let start = event(1, "session.start", json!({"session": "s"}));
    let base = [
        start.clone(),
        event(2, "unit.edge", json!({})),
        event(3, "trial.start", json!({})),
    ];
    let head = [
        start,
        event(2, "trial.start", json!({})),
        event(3, "unit.edge", json!({})),
    ];
    assert!(compare(&base, &head).unwrap().first.is_some());
}

#[test]
fn empty_journals_are_not_semantic_equality() {
    assert!(matches!(compare(&[], &[]), Err(super::JournalError::Empty)));
}

#[test]
fn imported_raw_evidence_cannot_hide_a_removed_cap_dependency() {
    let [mut base, head] = histories();
    base[3]["data"]["original"] = json!({"session": "s", "seq": 3});
    base[3]["data"]["original_kind"] = json!("day.step");
    base[3]["data"]["raw"] = json!("{\"causes\":{\"caps\":[2]}}");
    let error = compare(&base, &head).err().unwrap().to_string();
    assert!(error.contains("references removed ccd.cap"), "{error}");
}
