use test_case::test_case;

use crate::{Build, Dimension, Operation, Refusal, Verdict};

const BINARY: (u32, u32, u32) = (2, 5, 3);

fn stamp((schema, ruleset, evidence): (u32, u32, u32)) -> Build {
    Build {
        version: "0.1.0".into(),
        rev: None,
        ruleset,
        schema,
        fixes: 0,
        evidence,
    }
}

fn verdict(recorded: (u32, u32, u32), operation: Operation) -> Verdict {
    stamp(BINARY).verdict(&stamp(recorded), operation)
}

fn refuse(dimension: Dimension, recorded: u32, current: u32) -> Verdict {
    Verdict::Refuse(Refusal {
        dimension,
        recorded,
        current,
    })
}

const SAME: (u32, u32, u32) = BINARY;
const OLDER_SCHEMA: (u32, u32, u32) = (1, 5, 3);
const OLDER_RULESET: (u32, u32, u32) = (2, 4, 3);
const OLDER_EVIDENCE: (u32, u32, u32) = (2, 5, 2);
const NEWER_SCHEMA: (u32, u32, u32) = (3, 5, 3);
const NEWER_RULESET: (u32, u32, u32) = (2, 6, 3);
const NEWER_EVIDENCE: (u32, u32, u32) = (2, 5, 4);
const MIXED_NEWER_SCHEMA: (u32, u32, u32) = (3, 4, 3);
const MIXED_NEWER_EVIDENCE: (u32, u32, u32) = (1, 5, 4);

#[test_case(OLDER_SCHEMA; "older schema")]
#[test_case(OLDER_RULESET; "older ruleset")]
#[test_case(OLDER_EVIDENCE; "older evidence")]
fn resume_archives_an_older_dimension(older: (u32, u32, u32)) {
    assert_eq!(verdict(older, Operation::Resume), Verdict::Archive);
}

#[test]
fn resume_uses_current_archives_older_and_refuses_any_newer_dimension() {
    assert_eq!(verdict(SAME, Operation::Resume), Verdict::Use);
    assert_eq!(
        verdict(NEWER_SCHEMA, Operation::Resume),
        refuse(Dimension::Schema, 3, 2)
    );
    assert_eq!(
        verdict(NEWER_RULESET, Operation::Resume),
        refuse(Dimension::Ruleset, 6, 5)
    );
    assert_eq!(
        verdict(NEWER_EVIDENCE, Operation::Resume),
        refuse(Dimension::Evidence, 4, 3)
    );
    assert_eq!(
        verdict(MIXED_NEWER_SCHEMA, Operation::Resume),
        refuse(Dimension::Schema, 3, 2)
    );
    assert_eq!(
        verdict(MIXED_NEWER_EVIDENCE, Operation::Resume),
        refuse(Dimension::Evidence, 4, 3)
    );
}

#[test]
fn append_needs_every_dimension_current() {
    assert_eq!(verdict(SAME, Operation::Append), Verdict::Use);
    assert_eq!(
        verdict(OLDER_SCHEMA, Operation::Append),
        refuse(Dimension::Schema, 1, 2)
    );
    assert_eq!(
        verdict(OLDER_RULESET, Operation::Append),
        refuse(Dimension::Ruleset, 4, 5)
    );
    assert_eq!(
        verdict(OLDER_EVIDENCE, Operation::Append),
        refuse(Dimension::Evidence, 2, 3)
    );
    assert_eq!(
        verdict(NEWER_SCHEMA, Operation::Append),
        refuse(Dimension::Schema, 3, 2)
    );
    assert_eq!(
        verdict(NEWER_RULESET, Operation::Append),
        refuse(Dimension::Ruleset, 6, 5)
    );
    assert_eq!(
        verdict(NEWER_EVIDENCE, Operation::Append),
        refuse(Dimension::Evidence, 4, 3)
    );
}

#[test_case(SAME; "same")]
#[test_case(OLDER_RULESET; "older ruleset")]
#[test_case(NEWER_RULESET; "newer ruleset")]
#[test_case(OLDER_EVIDENCE; "older evidence")]
#[test_case(NEWER_EVIDENCE; "newer evidence")]
fn reset_all_uses_the_archive_unchanged_when_the_schema_matches(same_schema: (u32, u32, u32)) {
    assert_eq!(verdict(same_schema, Operation::ResetAll), Verdict::Use);
}

#[test_case(OLDER_SCHEMA; "older schema")]
#[test_case(NEWER_SCHEMA; "newer schema")]
#[test_case(MIXED_NEWER_SCHEMA; "mixed newer schema")]
fn reset_all_archives_when_the_schema_differs(other: (u32, u32, u32)) {
    assert_eq!(verdict(other, Operation::ResetAll), Verdict::Archive);
}

#[test_case(SAME; "same")]
#[test_case(OLDER_RULESET; "older ruleset")]
#[test_case(NEWER_RULESET; "newer ruleset")]
#[test_case(NEWER_EVIDENCE; "newer evidence")]
#[test_case(OLDER_EVIDENCE; "older evidence")]
fn inspect_uses_the_same_schema(same_schema: (u32, u32, u32)) {
    assert_eq!(verdict(same_schema, Operation::Inspect), Verdict::Use);
}

#[test]
fn inspect_refuses_another_schema() {
    assert_eq!(
        verdict(OLDER_SCHEMA, Operation::Inspect),
        refuse(Dimension::Schema, 1, 2)
    );
    assert_eq!(
        verdict(NEWER_SCHEMA, Operation::Inspect),
        refuse(Dimension::Schema, 3, 2)
    );
}

#[test_case(SAME; "same")]
#[test_case(OLDER_SCHEMA; "older schema")]
#[test_case(OLDER_EVIDENCE; "older evidence")]
#[test_case(NEWER_EVIDENCE; "newer evidence")]
fn replay_uses_a_known_schema_and_ignores_the_epoch(usable: (u32, u32, u32)) {
    assert_eq!(verdict(usable, Operation::Replay), Verdict::Use);
}

#[test]
fn replay_refuses_a_newer_schema_and_a_different_ruleset() {
    assert_eq!(
        verdict(NEWER_SCHEMA, Operation::Replay),
        refuse(Dimension::Schema, 3, 2)
    );
    assert_eq!(
        verdict(OLDER_RULESET, Operation::Replay),
        refuse(Dimension::Ruleset, 4, 5)
    );
    assert_eq!(
        verdict(NEWER_RULESET, Operation::Replay),
        refuse(Dimension::Ruleset, 6, 5)
    );
}

#[test_case(SAME; "same")]
#[test_case(OLDER_SCHEMA; "older schema")]
#[test_case(OLDER_RULESET; "older ruleset")]
#[test_case(NEWER_RULESET; "newer ruleset")]
#[test_case(NEWER_EVIDENCE; "newer evidence")]
fn history_reads_any_shipped_schema_not_newer(usable: (u32, u32, u32)) {
    assert_eq!(verdict(usable, Operation::History), Verdict::Use);
}

#[test]
fn history_refuses_a_newer_or_unshipped_schema_and_reads_older_evidence() {
    assert_eq!(
        verdict(NEWER_SCHEMA, Operation::History),
        refuse(Dimension::Schema, 3, 2)
    );
    assert_eq!(verdict(OLDER_EVIDENCE, Operation::History), Verdict::Use);
    assert_eq!(
        verdict((0, 5, 3), Operation::History),
        refuse(Dimension::Schema, 0, 2)
    );
}

const OPERATIONS: [Operation; 6] = [
    Operation::Resume,
    Operation::Append,
    Operation::ResetAll,
    Operation::Inspect,
    Operation::Replay,
    Operation::History,
];

#[test_case(SAME; "same")]
#[test_case(OLDER_SCHEMA; "older schema")]
#[test_case(OLDER_RULESET; "older ruleset")]
#[test_case(OLDER_EVIDENCE; "older evidence")]
#[test_case(NEWER_SCHEMA; "newer schema")]
#[test_case(NEWER_RULESET; "newer ruleset")]
#[test_case(NEWER_EVIDENCE; "newer evidence")]
fn changing_only_version_rev_or_fixes_leaves_every_verdict_unchanged(recorded: (u32, u32, u32)) {
    let mut upgraded = stamp(recorded);
    upgraded.version = "9.9.9".into();
    upgraded.rev = Some("def5678".into());
    upgraded.fixes = 7;
    for operation in OPERATIONS {
        assert_eq!(
            stamp(BINARY).verdict(&upgraded, operation),
            verdict(recorded, operation),
            "{operation:?}"
        );
    }
}

#[test]
fn an_upgraded_binary_uses_a_journal_of_its_previous_build() {
    let mut upgraded_binary = stamp(BINARY);
    upgraded_binary.version = "9.9.9".into();
    upgraded_binary.rev = Some("def5678".into());
    upgraded_binary.fixes = 7;
    for operation in OPERATIONS {
        assert_eq!(
            upgraded_binary.verdict(&stamp(SAME), operation),
            Verdict::Use,
            "{operation:?}"
        );
    }
}

#[test]
fn build_names_the_program_as_togi() {
    let mut build = stamp(SAME);
    assert_eq!(build.to_string(), "togi 0.1.0");
    build.rev = Some("abc".into());
    assert_eq!(build.to_string(), "togi 0.1.0+abc");
    build.version.clear();
    build.rev = None;
    assert_eq!(build.to_string(), "togi");
}

#[test]
fn build_wire_accepts_a_missing_fixes_field() {
    let build: Build =
        serde_json::from_str(r#"{"version":"1","ruleset":1,"schema":1,"evidence":1}"#).unwrap();
    assert_eq!(build.fixes, 0);
    assert!(
        serde_json::from_str::<Build>(
            r#"{"version":"1","ruleset":1,"schema":1,"evidence":1,"x":1}"#
        )
        .is_err()
    );
}

mod recorded {
    use std::path::Path;

    use test_case::test_case;

    use super::*;
    use crate::fs::MemFs;
    use crate::tests::fixtures::{at, sample};
    use crate::{DirError, Event, EventKind, SCHEMA, recorded_build_in};

    fn first_event() -> Event {
        at(1, "boot-a", 0, &[], sample(EventKind::SessionStart))
    }

    fn build_of(contents: &str) -> Result<Option<Build>, DirError> {
        let fs = MemFs::new();
        fs.put("/state/events.jsonl", contents);
        recorded_build_in(&fs, Path::new("/state"))
    }

    #[test]
    fn a_missing_journal_has_no_build() {
        let fs = MemFs::new();
        assert!(
            recorded_build_in(&fs, Path::new("/state"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn the_first_line_alone_gives_the_build() {
        let line = serde_json::to_string(&first_event()).unwrap();
        let build = build_of(&format!("{line}\n{{not json")).unwrap().unwrap();
        assert_eq!(build.schema, SCHEMA);
    }

    #[test]
    fn an_unterminated_first_line_has_no_build() {
        let line = serde_json::to_string(&first_event()).unwrap();
        assert!(build_of(&line).unwrap().is_none());
    }

    #[test]
    fn a_first_event_that_is_not_session_start_is_another_builds_journal() {
        let other = at(1, "boot-a", 0, &[], sample(EventKind::SmuIntent));
        let line = serde_json::to_string(&other).unwrap();
        assert!(matches!(
            build_of(&format!("{line}\n")),
            Err(DirError::ForeignJournal { .. })
        ));
    }

    #[test]
    fn a_malformed_first_line_is_another_builds_journal() {
        assert!(matches!(
            build_of("garbage\n"),
            Err(DirError::ForeignJournal { .. })
        ));
    }

    fn start_value() -> serde_json::Value {
        serde_json::to_value(first_event()).unwrap()
    }

    fn start_without_data() -> serde_json::Value {
        let mut start = start_value();
        start.as_object_mut().unwrap().remove("data");
        start
    }

    fn start_without_build() -> serde_json::Value {
        let mut start = start_value();
        start["data"].as_object_mut().unwrap().remove("build");
        start
    }

    fn start_without_session() -> serde_json::Value {
        let mut start = start_value();
        start["data"].as_object_mut().unwrap().remove("session");
        start
    }

    fn start_with_array_data() -> serde_json::Value {
        let mut start = start_value();
        start["data"] = serde_json::json!([]);
        start
    }

    fn start_with_a_bad_stamp() -> serde_json::Value {
        let mut start = start_value();
        start["data"]["build"]["schema"] = "one".into();
        start
    }

    #[test_case(&start_without_data(); "flat")]
    #[test_case(&start_without_build(); "no build")]
    #[test_case(&start_without_session(); "no session")]
    #[test_case(&start_with_array_data(); "array data")]
    #[test_case(&start_with_a_bad_stamp(); "bad stamp")]
    fn a_session_start_without_its_data_session_or_build_is_another_builds_journal(
        line: &serde_json::Value,
    ) {
        assert!(matches!(
            build_of(&format!("{line}\n")),
            Err(DirError::ForeignJournal { .. })
        ));
    }

    #[test]
    fn the_go_journal_header_is_another_builds_journal() {
        let go = r#"{"seq":1,"time":"2026-09-27T22:19:54.468855707Z","boot":"b","kind":"session.start","msg":"session started","version":"0.3.0","rev":"d1d77d5","ruleset":3,"schema":2,"fixes":1,"session":"20260927T221954Z","cores":[]}"#;
        let error = build_of(&format!("{go}\n")).unwrap_err();
        let text = error.to_string();
        assert!(matches!(error, DirError::ForeignJournal { .. }), "{text}");
        assert!(text.contains("/state/events.jsonl"), "{text}");
        assert!(text.contains("docs/howto.md#go-to-rust-cutover"), "{text}");
    }

    #[test]
    fn a_newer_builds_additions_to_the_header_are_not_another_builds_journal() {
        let mut start = serde_json::to_value(first_event()).unwrap();
        start["data"]["build"]["channel"] = "nightly".into();
        start["data"]["build"]["schema"] = (SCHEMA + 1).into();
        start["data"]["added"] = serde_json::json!({"later": true});
        start["added"] = 1.into();
        let build = build_of(&format!("{start}\n")).unwrap().unwrap();
        assert_eq!(build.schema, SCHEMA + 1);
        start["data"]["build"]["schema"] = SCHEMA.into();
        assert_eq!(
            build_of(&format!("{start}\n")).unwrap().unwrap().schema,
            SCHEMA
        );
    }

    #[test]
    fn the_header_is_judged_before_any_later_line_is_read() {
        let line = serde_json::to_string(&first_event()).unwrap();
        let fs = MemFs::new();
        fs.put("/state/events.jsonl", format!("{line}\n\u{0}garbage\n"));
        assert!(
            recorded_build_in(&fs, Path::new("/state"))
                .unwrap()
                .is_some()
        );
        fs.put("/state/events.jsonl", format!("garbage\n{line}\n"));
        assert!(matches!(
            recorded_build_in(&fs, Path::new("/state")),
            Err(DirError::ForeignJournal { .. })
        ));
    }

    #[test]
    fn a_foreign_first_line_is_refused_without_writing() {
        let fs = MemFs::new();
        fs.put("/state/events.jsonl", "{\"seq\":1}\nrest\n");
        let mutations = fs.mutations();
        assert!(recorded_build_in(&fs, Path::new("/state")).is_err());
        assert_eq!(fs.mutations(), mutations);
        assert_eq!(
            fs.contents("/state/events.jsonl").unwrap(),
            b"{\"seq\":1}\nrest\n"
        );
    }

    /// The Go build's journal of a session that only started, first line alone, without its newline.
    fn go_first_line_without_newline() -> &'static str {
        let go = include_str!("../../testdata/20260927T221954Z-config.jsonl");
        go.lines().next().unwrap()
    }

    #[test_case(go_first_line_without_newline(); "go first line")]
    #[test_case("garbage"; "garbage")]
    #[test_case("{\"seq\":1}"; "a complete object")]
    fn an_unterminated_first_line_that_is_not_a_journal_header_is_another_builds_journal(
        bytes: &str,
    ) {
        let fs = MemFs::new();
        fs.put("/state/events.jsonl", bytes);
        let mutations = fs.mutations();
        assert!(matches!(
            recorded_build_in(&fs, Path::new("/state")),
            Err(DirError::ForeignJournal { .. })
        ));
        assert_eq!(fs.mutations(), mutations);
        assert_eq!(
            fs.contents("/state/events.jsonl").unwrap(),
            bytes.as_bytes()
        );
    }

    #[test]
    fn a_zero_byte_journal_has_no_build() {
        let fs = MemFs::new();
        fs.put("/state/events.jsonl", "");
        assert!(
            recorded_build_in(&fs, Path::new("/state"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_first_line_cut_short_is_refused_like_a_foreign_one_and_the_message_pins_both_readings() {
        let fs = MemFs::new();
        fs.put("/state/events.jsonl", "{\"seq\":1,\"kind\":\"sess");
        let truncated = recorded_build_in(&fs, Path::new("/state")).unwrap_err();
        fs.put("/state/events.jsonl", go_first_line_without_newline());
        let go = recorded_build_in(&fs, Path::new("/state")).unwrap_err();
        let expected = "/state/events.jsonl: first line is not a complete header of this build; either another build's journal or a header torn by a crash; see docs/howto.md#go-to-rust-cutover";
        assert_eq!(truncated.to_string(), expected);
        assert_eq!(go.to_string(), expected);
    }
}

#[test]
fn real_fs_an_unreadable_journal_is_an_io_error() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");
    std::fs::write(&path, "x\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    if std::fs::File::open(&path).is_ok() {
        return;
    }
    assert!(matches!(
        crate::recorded_build(dir.path()),
        Err(crate::DirError::Io { .. })
    ));
}
