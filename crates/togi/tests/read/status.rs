use togi_watchtest::Scenario;

use crate::support::{append_between_trial_mce, append_event, session, text, togi};

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
    append_between_trial_mce(dir.path());
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

#[test]
fn a_queued_reset_shows_in_the_queued_column_until_the_search_restarts() {
    let dir = session(Scenario::Stopped);
    let queued = |dir: &std::path::Path| {
        let ran = togi(dir, &["status"]);
        assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
        text(&ran.stdout)
            .lines()
            .find(|line| line.starts_with("03 "))
            .unwrap()
            .to_owned()
    };
    assert!(queued(dir.path()).ends_with(" -"));
    append_event(
        dir.path(),
        "command.reset",
        "reset core 3",
        &serde_json::json!({"target": {"core": 3}}),
    );
    assert!(
        queued(dir.path()).ends_with(" reset"),
        "{}",
        queued(dir.path())
    );
}

#[test]
fn the_core_table_leads_with_core_ccd_slot_and_the_resident_offset() {
    let dir = session(Scenario::Checking);
    let ran = togi(dir.path(), &["status"]);
    let shown = text(&ran.stdout);
    let words = |line: &str| {
        line.split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let header = shown.lines().find(|line| line.starts_with("CORE")).unwrap();
    assert_eq!(
        words(header),
        [
            "CORE", "CCD", "SLOT", "OFFSET", "BIOS", "PHASE", "FAILURE", "QUEUED"
        ]
    );
    let row = |core: &str| {
        words(
            shown
                .lines()
                .find(|line| line.starts_with(&format!("{core} ")))
                .unwrap(),
        )
    };
    let first = row("00");
    assert_eq!(first[..4], ["00", "0", "0", "-26"]);
    assert_eq!(
        first[first.len() - 2],
        "-28",
        "the resident offset is not the failure point: {first:?}"
    );
    let ninth = row("08");
    assert_eq!(ninth[..4], ["08", "1", "0", "-5"]);
    assert_eq!(ninth[ninth.len() - 2], "-7");
    let last = row("15");
    assert_eq!(last[..3], ["15", "1", "7"]);
}

#[test]
fn the_phase_column_uses_the_dashboards_state_words() {
    use togi_journal::CorePhase;
    use togi_watch::words::phase_word;

    let all = [CorePhase::Search, CorePhase::HasRoom, CorePhase::AtLimit].map(phase_word);
    let mut seen = std::collections::BTreeSet::new();
    for scenario in Scenario::ALL {
        let dir = session(scenario);
        let ran = togi(dir.path(), &["status"]);
        let shown = text(&ran.stdout);
        let header = shown.lines().find(|line| line.starts_with("CORE")).unwrap();
        let (start, end) = (
            header.find("PHASE").unwrap(),
            header.find("FAILURE").unwrap(),
        );
        for line in shown
            .lines()
            .skip_while(|line| !line.starts_with("CORE"))
            .skip(1)
            .take_while(|line| !line.is_empty())
            .filter(|line| {
                line.get(..2)
                    .is_some_and(|id| id.bytes().all(|b| b.is_ascii_digit()))
            })
        {
            let cell = line.get(start..end).unwrap().trim();
            if cell != "-" {
                assert!(all.contains(&cell), "{} shows {cell:?}", scenario.name());
                seen.insert(cell.to_owned());
            }
        }
    }
    assert_eq!(seen.len(), all.len(), "{seen:?} of {all:?}");
}

#[test]
fn the_regime_table_keeps_its_regime_column() {
    let dir = session(Scenario::Between);
    let ran = togi(dir.path(), &["status"]);
    let shown = text(&ran.stdout);
    let at = shown
        .lines()
        .position(|line| line.starts_with("REGIME"))
        .unwrap();
    let rows: Vec<&str> = shown.lines().skip(at).collect();
    assert_eq!(
        rows[0].split_whitespace().collect::<Vec<_>>(),
        ["REGIME", "WORKLOAD", "TRIALS"]
    );
    assert_eq!(
        rows[1].split_whitespace().collect::<Vec<_>>(),
        ["R7", "mprime-avx2-36k-248k-allcore", "6"]
    );
}
