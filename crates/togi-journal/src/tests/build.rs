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
    fn a_first_event_that_is_not_session_start_is_not_a_journal() {
        let other = at(1, "boot-a", 0, &[], sample(EventKind::SmuIntent));
        let line = serde_json::to_string(&other).unwrap();
        assert!(matches!(
            build_of(&format!("{line}\n")),
            Err(DirError::NotAJournal { .. })
        ));
    }

    #[test]
    fn a_malformed_first_line_is_not_a_journal() {
        assert!(matches!(
            build_of("garbage\n"),
            Err(DirError::NotAJournal { .. })
        ));
    }
}
