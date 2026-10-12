use std::io::Write as _;

use jiff::Timestamp;
use togi_watchtest::Scenario;

use crate::support::{first_trial, journal, lines, rewrite_first_line, session, text, togi};

#[test]
fn json_prints_the_journal_lines_unchanged() {
    let dir = session(Scenario::Search);
    let ran = togi(dir.path(), &["events", "--json"]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(text(&ran.stderr), "");
    let printed: Vec<&str> = text(&ran.stdout).lines().collect();
    assert_eq!(printed, lines(dir.path()));
}

#[test]
fn filters_narrow_the_readable_lines() {
    let dir = session(Scenario::Search);
    let trial = first_trial(dir.path());
    let cases: [(&str, Vec<&str>); 4] = [
        ("trial", vec!["--trial", &trial]),
        (
            "core",
            vec!["--core", "3", "--kind", "tuner.decision,core.phase"],
        ),
        ("kinds", vec!["--kind", "session.start, session.baseline,"]),
        (
            "group",
            vec!["--kind", "session", "--until", "2026-01-01T00:00:00.5Z"],
        ),
    ];
    for (name, extra) in cases {
        let mut args = vec!["events"];
        args.extend(extra);
        let ran = togi(dir.path(), &args);
        assert_eq!(ran.status.code(), Some(0), "{name}");
        assert_eq!(text(&ran.stderr), "", "{name}");
        insta::assert_snapshot!(format!("events_{name}"), text(&ran.stdout));
    }
}

#[test]
fn a_time_window_is_inclusive_since_and_exclusive_until() {
    let dir = session(Scenario::Search);
    let all = togi(dir.path(), &["events", "--json"]);
    let times: Vec<Timestamp> = text(&all.stdout)
        .lines()
        .map(|line| {
            let event: serde_json::Value = serde_json::from_str(line).unwrap();
            event["time"].as_str().unwrap().parse().unwrap()
        })
        .collect();
    let boundary = times[times.len() / 2];
    let text_of = boundary.to_string();
    let since = togi(dir.path(), &["events", "--json", "--since", &text_of]);
    let until = togi(dir.path(), &["events", "--json", "--until", &text_of]);
    assert_eq!(
        text(&since.stdout).lines().count(),
        times.iter().filter(|time| **time >= boundary).count()
    );
    assert_eq!(
        text(&until.stdout).lines().count(),
        times.iter().filter(|time| **time < boundary).count()
    );
}

#[test]
fn a_valid_filter_with_no_match_prints_nothing_and_exits_zero() {
    let dir = session(Scenario::Search);
    let ran = togi(dir.path(), &["events", "--trial", "no-such-trial"]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(text(&ran.stdout), "");
    assert_eq!(text(&ran.stderr), "");
}

#[test]
fn kind_errors_are_usage_errors_before_the_journal_is_read() {
    let empty = tempfile::tempdir().unwrap();
    for kind in ["no.such", "nosuchgroup", ",", " ", ""] {
        let ran = togi(empty.path(), &["events", "--kind", kind]);
        assert_eq!(ran.status.code(), Some(2), "{kind:?}");
        assert_eq!(text(&ran.stdout), "", "{kind:?}");
        assert!(
            text(&ran.stderr).contains("valid names:"),
            "{kind:?}: {}",
            text(&ran.stderr)
        );
        assert!(text(&ran.stderr).contains("trial.intent"), "{kind:?}");
    }
    let ran = togi(empty.path(), &["events", "--kind", "trial", "--kind", ","]);
    assert_eq!(ran.status.code(), Some(2));
}

#[test]
fn flag_value_errors_are_usage_errors() {
    let empty = tempfile::tempdir().unwrap();
    for args in [
        ["events", "--since", "yesterday"],
        ["events", "--until", "2026-01-01"],
        ["events", "--core", "x"],
        ["events", "--core", "999"],
    ] {
        let ran = togi(empty.path(), &args);
        assert_eq!(ran.status.code(), Some(2), "{args:?}");
        assert_eq!(text(&ran.stdout), "");
    }
}

#[test]
fn a_missing_journal_is_an_error_naming_the_file() {
    let empty = tempfile::tempdir().unwrap();
    let ran = togi(empty.path(), &["events"]);
    assert_eq!(ran.status.code(), Some(1));
    assert_eq!(text(&ran.stdout), "");
    assert!(
        text(&ran.stderr).contains("no journal at"),
        "{}",
        text(&ran.stderr)
    );
    assert!(text(&ran.stderr).contains("events.jsonl"));
}

#[test]
fn a_file_that_is_not_a_journal_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(journal(dir.path()), "not json\n").unwrap();
    let ran = togi(dir.path(), &["events"]);
    assert_eq!(ran.status.code(), Some(1));
    assert_eq!(text(&ran.stdout), "");
    assert!(
        text(&ran.stderr).starts_with("togi events:"),
        "{}",
        text(&ran.stderr)
    );
}

#[test]
fn a_newer_schema_is_refused_with_advice() {
    let dir = session(Scenario::Search);
    rewrite_first_line(dir.path(), |event| {
        event["data"]["build"]["schema"] = 99.into();
    });
    for command in ["events", "status"] {
        let ran = togi(dir.path(), &[command]);
        assert_eq!(ran.status.code(), Some(1), "{command}");
        assert_eq!(text(&ran.stdout), "", "{command}");
        let err = text(&ran.stderr);
        assert!(err.contains("schema 99"), "{err}");
        assert!(err.contains("togi reset --all"), "{err}");
    }
}

#[test]
fn an_older_schema_is_refused_too() {
    let dir = session(Scenario::Search);
    rewrite_first_line(dir.path(), |event| {
        event["data"]["build"]["schema"] = 0.into();
    });
    let ran = togi(dir.path(), &["events"]);
    assert_eq!(ran.status.code(), Some(1));
    assert!(
        text(&ran.stderr).contains("uses schema 1"),
        "{}",
        text(&ran.stderr)
    );
}

#[test]
fn a_different_ruleset_warns_and_still_renders() {
    let dir = session(Scenario::Search);
    rewrite_first_line(dir.path(), |event| {
        event["data"]["build"]["ruleset"] = 7.into();
    });
    for args in [
        ["events", "--json"],
        ["events", "--kind=session"],
        ["status", ""],
    ] {
        let args: Vec<&str> = args.into_iter().filter(|a| !a.is_empty()).collect();
        let ran = togi(dir.path(), &args);
        assert_eq!(ran.status.code(), Some(0), "{args:?}");
        assert!(
            text(&ran.stderr).contains("ruleset is 7"),
            "{}",
            text(&ran.stderr)
        );
        assert!(!text(&ran.stdout).is_empty());
    }
}

#[test]
fn an_event_of_an_unknown_kind_is_a_read_error() {
    let dir = session(Scenario::Search);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(journal(dir.path()))
        .unwrap();
    let last: serde_json::Value = serde_json::from_str(lines(dir.path()).last().unwrap()).unwrap();
    let seq = last["seq"].as_u64().unwrap() + 1;
    writeln!(
        file,
        r#"{{"seq":{seq},"time":"2026-01-02T00:00:00Z","boot":"{}","kind":"future.thing","msg":"x","data":{{}}}}"#,
        last["boot"].as_str().unwrap()
    )
    .unwrap();
    let ran = togi(dir.path(), &["events"]);
    assert_eq!(ran.status.code(), Some(1));
    assert_eq!(text(&ran.stdout), "");
    assert!(
        text(&ran.stderr).contains("future.thing"),
        "{}",
        text(&ran.stderr)
    );
}

#[test]
fn a_torn_tail_is_reported_after_the_events() {
    let dir = session(Scenario::Search);
    let whole = lines(dir.path()).len();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(journal(dir.path()))
        .unwrap();
    file.write_all(b"{\"seq\":").unwrap();
    for command in ["events", "status"] {
        let ran = togi(dir.path(), &[command]);
        assert_eq!(ran.status.code(), Some(0), "{command}");
        assert!(
            text(&ran.stderr).contains("journal ends with 7 torn bytes"),
            "{}",
            text(&ran.stderr)
        );
        if command == "events" {
            assert_eq!(text(&ran.stdout).lines().count(), whole);
        }
    }
    assert_eq!(
        lines(dir.path()).len(),
        whole + 1,
        "reading never repairs the tail"
    );
}

#[test]
fn state_dir_works_before_and_after_the_command() {
    let dir = session(Scenario::Search);
    let path = dir.path().to_str().unwrap();
    let after = std::process::Command::new(env!("CARGO_BIN_EXE_togi"))
        .args(["events", "--json", "--state-dir", path])
        .output()
        .unwrap();
    let before = togi(dir.path(), &["events", "--json"]);
    assert_eq!(after.status.code(), Some(0));
    assert_eq!(after.stdout, before.stdout);
}

#[test]
fn config_is_rejected() {
    let dir = session(Scenario::Search);
    let ran = togi(dir.path(), &["events", "--config", "x"]);
    assert_eq!(ran.status.code(), Some(2));
}

#[test]
fn events_help() {
    let ran = togi(std::path::Path::new("/nonexistent"), &["events", "--help"]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(text(&ran.stderr), "");
    insta::assert_snapshot!(text(&ran.stdout));
}
