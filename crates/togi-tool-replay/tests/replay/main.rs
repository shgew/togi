#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "the tests install a recording, run the built command and panic on a broken fixture"
)]

mod live;
#[path = "../../src/tests/plant.rs"]
mod plant;

use std::process::{Command, Output};

use tempfile::tempdir;
use test_case::test_case;
use togi_watchtest::{Scenario, install};

fn replay(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_togi-tool-replay"))
        .args(args)
        .output()
        .unwrap()
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap()
}

#[test]
fn real_process_at_prints_a_frame_of_a_recording_and_nothing_else() {
    let dir = tempdir().unwrap();
    install(dir.path(), Scenario::Checking).unwrap();
    let state = dir.path().to_str().unwrap();
    let ran = replay(&[
        "--state-dir",
        state,
        "--at",
        "20",
        "--width",
        "100",
        "--height",
        "30",
    ]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(text(ran.stderr), "");
    let frame = text(ran.stdout);
    assert_eq!(frame.lines().count(), 29);
    assert!(!frame.contains('\x1b'));
    assert!(frame.contains("togi"), "{frame}");

    let coloured = replay(&["--state-dir", state, "--at", "20", "--color"]);
    assert!(text(coloured.stdout).contains('\x1b'));
}

#[test]
fn real_process_at_says_once_on_stderr_what_it_skipped_and_still_prints_the_frame() {
    let dir = tempdir().unwrap();
    install(dir.path(), Scenario::Checking).unwrap();
    let mut recording = plant::recording(Scenario::Checking);
    let planted = plant::plant(&mut recording);
    std::fs::write(dir.path().join("events.jsonl"), recording).unwrap();
    let path = dir.path().join("events.jsonl");
    let bytes = std::fs::read(&path).unwrap();
    let ran = replay(&["--state-dir", dir.path().to_str().unwrap(), "--at", "99999"]);
    assert_eq!(ran.status.code(), Some(0));
    let stderr = text(ran.stderr);
    assert_eq!(stderr.lines().count(), 1, "{stderr}");
    let report = stderr.strip_prefix("togi-tool-replay: ").unwrap();
    assert!(report.starts_with(&planted.skipped_kind()), "{stderr}");
    for ignored in planted.ignored_fields() {
        assert!(report.contains(&ignored), "{stderr}");
    }
    let frame = text(ran.stdout);
    assert!(frame.contains("togi"), "{frame}");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test_case("--speed=NaN"; "nan")]
#[test_case("--speed=inf"; "inf")]
#[test_case("--speed=0"; "zero")]
#[test_case("--speed=-0"; "negative zero")]
#[test_case("--speed=-1"; "negative")]
fn real_process_an_invalid_speed_is_a_usage_error_before_the_journal_is_read(speed: &str) {
    let dir = tempdir().unwrap();
    let missing = dir.path().join("missing");
    let ran = replay(&["--state-dir", missing.to_str().unwrap(), speed]);
    assert_eq!(ran.status.code(), Some(2));
    let stderr = text(ran.stderr);
    assert!(
        stderr.contains("--speed must be finite and positive"),
        "{stderr}"
    );
    assert!(!stderr.contains("read journal"), "{stderr}");
}

#[test]
fn real_process_playing_without_a_terminal_fails_and_points_at_at() {
    let dir = tempdir().unwrap();
    install(dir.path(), Scenario::Checking).unwrap();
    let ran = replay(&["--state-dir", dir.path().to_str().unwrap()]);
    assert_eq!(ran.status.code(), Some(1));
    assert_eq!(
        text(ran.stderr),
        "togi-tool-replay: playing needs a terminal; use --at for one frame\n"
    );
    assert_eq!(text(ran.stdout), "");
}

#[test]
fn real_process_a_missing_journal_exits_1_naming_the_file() {
    let dir = tempdir().unwrap();
    let ran = replay(&["--state-dir", dir.path().to_str().unwrap(), "--at", "1"]);
    assert_eq!(ran.status.code(), Some(1));
    assert!(text(ran.stderr).starts_with("togi-tool-replay: read journal: "));
}
