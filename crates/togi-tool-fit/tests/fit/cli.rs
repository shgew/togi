use std::path::Path;
use std::process::{Command, Output};

use togi_machine::Regime;

use crate::support::{fact, write_extract};

#[expect(
    clippy::disallowed_methods,
    reason = "real_process tests run the fitted-machine command"
)]
fn fit(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_togi-tool-fit"))
        .args(args)
        .output()
        .unwrap()
}

fn two_sessions(dir: &Path) -> std::path::PathBuf {
    let facts = vec![
        fact(
            "20260101T000000Z",
            1,
            &[-10, 0],
            Regime::R1,
            &[0],
            60,
            false,
        ),
        fact("20260102T000000Z", 1, &[-10, 0], Regime::R1, &[0], 60, true),
    ];
    write_extract(dir, &facts)
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[test]
fn real_process_forward_only_prints_the_check_and_writes_no_machine_files() {
    let dir = tempfile::tempdir().unwrap();
    let extract = two_sessions(dir.path());
    let out = dir.path().join("machines");
    let ran = fit(&[
        "--facts",
        extract.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--forward-only",
    ]);
    let stdout = text(&ran.stdout);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    assert!(
        stdout.contains("20260102T000000Z ruleset=1 training_sessions=1: trials=1 failures=1"),
        "{stdout}"
    );
    assert!(stdout.contains("Pooled: "), "{stdout}");
    assert!(stdout.contains("Forward-chained elapsed: "), "{stdout}");
    assert!(
        !out.exists(),
        "a forward-only run created the output directory"
    );
}

#[test]
fn real_process_sealing_every_held_out_session_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let extract = two_sessions(dir.path());
    let ran = fit(&[
        "--facts",
        extract.to_str().unwrap(),
        "--forward-only",
        "--seal",
        "1",
    ]);
    assert_eq!(ran.status.code(), Some(1));
    assert!(
        text(&ran.stderr).contains("--seal 1 leaves no held-out session to score (1 held out)"),
        "{}",
        text(&ran.stderr)
    );
}

#[test]
fn real_process_an_unreadable_extract_exits_1_with_the_error_on_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.gz");
    for extra in [
        &[][..],
        &["--forward-only"],
        &["--shared-voltage-in-sample"],
    ] {
        let mut args = vec!["--facts", missing.to_str().unwrap()];
        args.extend_from_slice(extra);
        let ran = fit(&args);
        assert_eq!(ran.status.code(), Some(1), "{extra:?}");
        assert!(
            text(&ran.stderr).contains("open extract"),
            "{extra:?}: {}",
            text(&ran.stderr)
        );
        assert!(ran.stdout.is_empty(), "{extra:?}");
    }
}

#[test]
fn real_process_flags_that_do_not_go_together_are_usage_errors() {
    for args in [
        &["--shared-voltage-in-sample", "--forward-only"][..],
        &["--shared-voltage-in-sample", "--bootstrap", "0"],
        &["--shared-voltage-in-sample", "--seed", "263"],
        &["--shared-voltage-in-sample", "--seal", "0"],
        &["--seal", "1"],
        &["--forward-only", "--seal", "-1"],
        &["--jobs", "0"],
        &["--bootstrap", "-1"],
        &["extra"],
    ] {
        let ran = fit(args);
        assert_eq!(
            ran.status.code(),
            Some(2),
            "{args:?}: {}",
            text(&ran.stderr)
        );
        assert!(ran.stdout.is_empty(), "{args:?}");
    }
}

#[test]
fn real_process_the_help_lists_the_modes() {
    let ran = fit(&["--help"]);
    assert_eq!(ran.status.code(), Some(0));
    let stdout = text(&ran.stdout);
    for flag in [
        "--forward-only",
        "--shared-voltage-in-sample",
        "--jobs",
        "--seal",
    ] {
        assert!(stdout.contains(flag), "{flag}: {stdout}");
    }
}

#[test]
fn real_process_an_ensemble_run_writes_the_machine_files_and_prints_the_report() {
    let dir = tempfile::tempdir().unwrap();
    let extract = two_sessions(dir.path());
    let out = dir.path().join("machines");
    let ran = fit(&[
        "--facts",
        extract.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--bootstrap",
        "1",
        "--jobs",
        "2",
    ]);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    for n in 0..2 {
        assert!(out.join(format!("target-fit-{n}.json")).is_file());
    }
    let stdout = text(&ran.stdout);
    assert!(stdout.contains("negative log likelihood"), "{stdout}");
    assert!(stdout.contains("Forward-chained check"), "{stdout}");
    assert!(stdout.contains("Fit elapsed: "), "{stdout}");
}

#[test]
fn real_process_the_shared_voltage_anchor_is_fitted_checked_and_written() {
    let dir = tempfile::tempdir().unwrap();
    let mut facts = Vec::new();
    for i in 0..149_u64 {
        let offset = if i >= 29 { -30 } else { 0 };
        let loaded: Vec<i64> = (0..16).collect();
        facts.push(fact(
            "20260101T000000Z",
            i + 1,
            &[offset; 16],
            Regime::R7,
            &loaded,
            600,
            i == 0,
        ));
    }
    let extract = write_extract(dir.path(), &facts);
    let out = dir.path().join("machines");
    let ran = fit(&[
        "--facts",
        extract.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
        "--shared-voltage-in-sample",
    ]);
    let stdout = text(&ran.stdout);
    assert_eq!(ran.status.code(), Some(0), "{stdout}{}", text(&ran.stderr));
    assert!(out.join("target-shared-voltage.json").is_file());
    assert!(stdout.contains("Wrote IN-SAMPLE anchor: "), "{stdout}");
    assert!(stdout.contains("Fit elapsed: "), "{stdout}");
    assert!(!stdout.contains("Forward-chained"), "{stdout}");
    assert!(!out.join("target-fit-0.json").exists());
}
