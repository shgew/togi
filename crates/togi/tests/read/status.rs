use togi_watchtest::Scenario;

use crate::support::{session, text, togi};

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
