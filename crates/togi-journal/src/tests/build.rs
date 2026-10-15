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

#[test]
fn resume_uses_current_archives_older_and_refuses_any_newer_dimension() {
    assert_eq!(verdict(SAME, Operation::Resume), Verdict::Use);
    for older in [OLDER_SCHEMA, OLDER_RULESET, OLDER_EVIDENCE] {
        assert_eq!(
            verdict(older, Operation::Resume),
            Verdict::Archive,
            "{older:?}"
        );
    }
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

#[test]
fn reset_all_archives_unchanged_unless_the_schema_matches() {
    for same_schema in [
        SAME,
        OLDER_RULESET,
        NEWER_RULESET,
        OLDER_EVIDENCE,
        NEWER_EVIDENCE,
    ] {
        assert_eq!(
            verdict(same_schema, Operation::ResetAll),
            Verdict::Use,
            "{same_schema:?}"
        );
    }
    for other in [OLDER_SCHEMA, NEWER_SCHEMA, MIXED_NEWER_SCHEMA] {
        assert_eq!(
            verdict(other, Operation::ResetAll),
            Verdict::Archive,
            "{other:?}"
        );
    }
}

#[test]
fn inspect_needs_the_same_schema() {
    for same_schema in [SAME, OLDER_RULESET, NEWER_RULESET, NEWER_EVIDENCE] {
        assert_eq!(verdict(same_schema, Operation::Inspect), Verdict::Use);
    }
    assert_eq!(verdict(OLDER_EVIDENCE, Operation::Inspect), Verdict::Use);
    assert_eq!(
        verdict(OLDER_SCHEMA, Operation::Inspect),
        refuse(Dimension::Schema, 1, 2)
    );
    assert_eq!(
        verdict(NEWER_SCHEMA, Operation::Inspect),
        refuse(Dimension::Schema, 3, 2)
    );
}

#[test]
fn replay_needs_a_known_schema_and_the_same_ruleset_and_ignores_the_epoch() {
    for usable in [SAME, OLDER_SCHEMA, OLDER_EVIDENCE, NEWER_EVIDENCE] {
        assert_eq!(
            verdict(usable, Operation::Replay),
            Verdict::Use,
            "{usable:?}"
        );
    }
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

#[test]
fn history_reads_any_shipped_schema_not_newer() {
    for usable in [
        SAME,
        OLDER_SCHEMA,
        OLDER_RULESET,
        NEWER_RULESET,
        NEWER_EVIDENCE,
    ] {
        assert_eq!(
            verdict(usable, Operation::History),
            Verdict::Use,
            "{usable:?}"
        );
    }
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

#[test]
fn changing_only_version_rev_or_fixes_leaves_every_verdict_unchanged() {
    let operations = [
        Operation::Resume,
        Operation::Append,
        Operation::ResetAll,
        Operation::Inspect,
        Operation::Replay,
        Operation::History,
    ];
    let recorded_stamps = [
        SAME,
        OLDER_SCHEMA,
        OLDER_RULESET,
        OLDER_EVIDENCE,
        NEWER_SCHEMA,
        NEWER_RULESET,
        NEWER_EVIDENCE,
    ];
    for recorded in recorded_stamps {
        let mut upgraded = stamp(recorded);
        upgraded.version = "9.9.9".into();
        upgraded.rev = Some("def5678".into());
        upgraded.fixes = 7;
        for operation in operations {
            assert_eq!(
                stamp(BINARY).verdict(&upgraded, operation),
                verdict(recorded, operation),
                "{recorded:?} {operation:?}"
            );
        }
    }
    let mut upgraded_binary = stamp(BINARY);
    upgraded_binary.version = "9.9.9".into();
    upgraded_binary.rev = Some("def5678".into());
    upgraded_binary.fixes = 7;
    for operation in operations {
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

    #[test]
    fn a_session_start_without_its_data_session_or_build_is_another_builds_journal() {
        let start = serde_json::to_value(first_event()).unwrap();
        let mut flat = start.clone();
        flat.as_object_mut().unwrap().remove("data");
        let mut no_build = start.clone();
        no_build["data"].as_object_mut().unwrap().remove("build");
        let mut no_session = start.clone();
        no_session["data"]
            .as_object_mut()
            .unwrap()
            .remove("session");
        let mut listed = start.clone();
        listed["data"] = serde_json::json!([]);
        let mut bad_stamp = start;
        bad_stamp["data"]["build"]["schema"] = "one".into();
        for (name, line) in [
            ("flat", flat),
            ("no build", no_build),
            ("no session", no_session),
            ("array data", listed),
            ("bad stamp", bad_stamp),
        ] {
            assert!(
                matches!(
                    build_of(&format!("{line}\n")),
                    Err(DirError::ForeignJournal { .. })
                ),
                "{name}"
            );
        }
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
    fn go_first_line_without_newline() -> String {
        let go = include_str!("../../testdata/20260927T221954Z-config.jsonl");
        go.lines().next().unwrap().to_owned()
    }

    #[test]
    fn an_unterminated_first_line_that_is_not_a_journal_header_is_another_builds_journal() {
        for (name, bytes) in [
            ("go first line", go_first_line_without_newline()),
            ("garbage", "garbage".to_owned()),
            ("a complete object", "{\"seq\":1}".to_owned()),
        ] {
            let fs = MemFs::new();
            fs.put("/state/events.jsonl", bytes.clone());
            let mutations = fs.mutations();
            assert!(
                matches!(
                    recorded_build_in(&fs, Path::new("/state")),
                    Err(DirError::ForeignJournal { .. })
                ),
                "{name}"
            );
            assert_eq!(fs.mutations(), mutations, "{name}");
            assert_eq!(
                fs.contents("/state/events.jsonl").unwrap(),
                bytes.as_bytes(),
                "{name}"
            );
        }
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
