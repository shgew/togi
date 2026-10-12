use togi_watchtest::Scenario;

use crate::support::{append_event, session, text, togi};

#[test]
fn scenarios_render_the_status() {
    for scenario in Scenario::ALL {
        let dir = session(scenario);
        let ran = togi(dir.path(), &["status"]);
        assert_eq!(ran.status.code(), Some(0), "{}", scenario.name());
        assert_eq!(text(&ran.stderr), "", "{}", scenario.name());
        insta::assert_snapshot!(format!("status_{}", scenario.name()), text(&ran.stdout));
    }
}

#[test]
fn a_journal_without_a_session_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(crate::support::journal(dir.path()), "").unwrap();
    let ran = togi(dir.path(), &["status"]);
    assert_eq!(ran.status.code(), Some(1));
    assert_eq!(text(&ran.stdout), "");
    assert!(
        text(&ran.stderr).contains("no session in"),
        "{}",
        text(&ran.stderr)
    );
}

#[test]
fn a_missing_journal_is_an_error_naming_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let ran = togi(dir.path(), &["status"]);
    assert_eq!(ran.status.code(), Some(1));
    assert!(
        text(&ran.stderr).contains("no journal at"),
        "{}",
        text(&ran.stderr)
    );
}

#[test]
fn status_takes_no_arguments() {
    let dir = session(Scenario::Search);
    let ran = togi(dir.path(), &["status", "extra"]);
    assert_eq!(ran.status.code(), Some(2));
    assert_eq!(text(&ran.stdout), "");
}

#[test]
fn status_fits_eighty_columns() {
    for scenario in Scenario::ALL {
        let dir = session(scenario);
        let ran = togi(dir.path(), &["status"]);
        for line in text(&ran.stdout).lines() {
            assert!(line.chars().count() <= 80, "{}: {line}", scenario.name());
        }
    }
}

#[test]
fn status_help() {
    let ran = togi(std::path::Path::new("/nonexistent"), &["status", "--help"]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(text(&ran.stderr), "");
    insta::assert_snapshot!(text(&ran.stdout));
}

#[test]
fn a_too_cautious_defect_lists_reset_commands_until_the_core_is_reset() {
    let dir = session(Scenario::Search);
    append_event(
        dir.path(),
        "defect.found",
        "defect 1",
        &serde_json::json!({
            "id": 1, "title": "step\u{1b}[31m too cautious", "detail": "why", "pr": 7,
            "direction": "too_cautious", "cores": [3, 5], "decisions": [4, 6],
        }),
    );
    let before = togi(dir.path(), &["status"]);
    let shown = text(&before.stdout);
    assert!(
        shown.contains("defect 1: step\\x1b[31m too cautious (fixed by pull request #7)"),
        "{shown}"
    );
    assert!(shown.contains("togi reset --core 3\n"));
    assert!(shown.contains("togi reset --core 5\n"));
    append_event(
        dir.path(),
        "command.reset",
        "reset core 3",
        &serde_json::json!({"target": {"core": 3}}),
    );
    let after = togi(dir.path(), &["status"]);
    assert_eq!(after.status.code(), Some(0), "{}", text(&after.stderr));
    assert!(!text(&after.stdout).contains("togi reset --core 3"));
    assert!(text(&after.stdout).contains("togi reset --core 5\n"));
    append_event(
        dir.path(),
        "command.reset",
        "reset all",
        &serde_json::json!({"target": "all"}),
    );
    let all = togi(dir.path(), &["status"]);
    assert!(!text(&all.stdout).contains("defect 1"));
}

#[test]
fn a_between_trial_mce_is_listed_as_evidence_and_escaped() {
    let dir = session(Scenario::Search);
    append_event(
        dir.path(),
        "mce",
        "mce on cpu 3 \u{1b}[2J between trials (recorded only)",
        &serde_json::json!({
            "cpu": 3, "bank": 1, "bank_type": "load_store", "corrected": true,
            "between_trials": true, "lines": [],
        }),
    );
    let ran = togi(dir.path(), &["status"]);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    let shown = text(&ran.stdout);
    assert!(shown.contains("between-trial evidence [#"), "{shown}");
    assert!(shown.contains("\\x1b[2J"), "{shown}");
    assert!(!shown.contains('\u{1b}'));
}

#[test]
fn a_hunt_names_the_decisive_failure_signal() {
    let dir = session(Scenario::Hunt);
    let ran = togi(dir.path(), &["status"]);
    let shown = text(&ran.stdout);
    let hunt = shown
        .lines()
        .find(|line| line.starts_with("hunt 1 [#"))
        .unwrap();
    assert!(
        hunt.contains("unattributed corrected_mce in R2 trial"),
        "{hunt}"
    );
}

#[test]
fn wide_bios_fields_wrap_and_stay_whole() {
    let dir = session(Scenario::Search);
    let board = "very-wide-board-name-".repeat(5);
    append_event(
        dir.path(),
        "session.context",
        "context",
        &serde_json::json!({
            "bios_version": "v".repeat(30), "board": board, "cpu_model": "cpu ".repeat(15),
            "microcode": "0x1", "boost_limit_mhz": 5000,
        }),
    );
    let ran = togi(dir.path(), &["status"]);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    let shown = text(&ran.stdout);
    assert!(shown.contains(board.trim_end()), "{shown}");
    for line in shown.lines().filter(|line| line.chars().count() > 80) {
        assert!(!line.trim().contains(' '), "{line}");
    }
}

#[test]
fn carried_events_leave_the_core_table_unchanged() {
    let dir = session(Scenario::Search);
    let before = togi(dir.path(), &["status"]);
    append_event(
        dir.path(),
        "session.carried",
        "carried 0 candidate solo limits",
        &serde_json::json!({"sources": [], "failure_points": false}),
    );
    let after = togi(dir.path(), &["status"]);
    assert_eq!(after.status.code(), Some(0), "{}", text(&after.stderr));
    let table = |output: &[u8]| {
        text(output)
            .lines()
            .skip_while(|line| !line.starts_with("CORE"))
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    assert_eq!(table(&before.stdout), table(&after.stdout));
    assert!(text(&after.stdout).contains("carried: [#"));
}
