#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "the tests run the built command and panic on a broken fixture"
)]

use std::process::{Command, Output};

fn togi(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_togi"))
        .args(args)
        .output()
        .unwrap()
}

fn stdout(output: &Output) -> &str {
    std::str::from_utf8(&output.stdout).unwrap()
}

fn stderr(output: &Output) -> &str {
    std::str::from_utf8(&output.stderr).unwrap()
}

fn is_version_line(line: &str) -> bool {
    let Some(rest) = line
        .strip_prefix("togi ")
        .and_then(|r| r.strip_suffix('\n'))
    else {
        return false;
    };
    let Some((version, revision)) = rest.split_once('+') else {
        return false;
    };
    let numbers: Vec<&str> = version.split('.').collect();
    numbers.len() == 3
        && numbers
            .iter()
            .all(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        && !revision.is_empty()
        && revision
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[test]
fn version_prints_the_version_and_revision_on_stdout() {
    let ran = togi(&["--version"]);
    assert_eq!(ran.status.code(), Some(0));
    assert!(is_version_line(stdout(&ran)), "{:?}", stdout(&ran));
    assert_eq!(stderr(&ran), "");
}

#[test]
fn a_local_build_reports_the_dev_revision() {
    if option_env!("TOGI_REV").is_some_and(|rev| !rev.is_empty()) {
        return;
    }
    assert!(stdout(&togi(&["--version"])).ends_with("+dev\n"));
}

#[test]
fn version_after_a_command_is_a_usage_error() {
    let ran = togi(&["completion", "bash", "--version"]);
    assert_eq!(ran.status.code(), Some(2));
    assert_eq!(stdout(&ran), "");
    assert!(
        stderr(&ran).contains("unexpected argument '--version'"),
        "{}",
        stderr(&ran)
    );
}

#[test]
fn top_level_help() {
    let ran = togi(&["--help"]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(stderr(&ran), "");
    insta::assert_snapshot!(stdout(&ran));
}

#[test]
fn completion_help() {
    let ran = togi(&["completion", "--help"]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(stderr(&ran), "");
    insta::assert_snapshot!(stdout(&ran));
}

#[test]
fn no_command_prints_the_wordmark_and_usage_on_stderr() {
    let ran = togi(&[]);
    assert_eq!(ran.status.code(), Some(2));
    assert_eq!(stdout(&ran), "");
    insta::assert_snapshot!(stderr(&ran));
}

#[test]
fn state_dir_without_a_command_prints_the_wordmark_and_usage_on_stderr() {
    for args in [&["--state-dir", "/x"][..], &["--state-dir=/x"]] {
        let ran = togi(args);
        assert_eq!(ran.status.code(), Some(2), "{args:?}");
        assert_eq!(stdout(&ran), "", "{args:?}");
        assert!(
            stderr(&ran).starts_with("   __              _\n"),
            "{args:?}: {}",
            stderr(&ran)
        );
        assert!(stderr(&ran).contains("Usage: togi"), "{args:?}");
    }
}

/// `/dev/full` fails every write with `ENOSPC`. A stdout closed at startup is not tested: Rust's
/// runtime reopens it on `/dev/null` before `main`, so no write ever fails there.
#[cfg(target_os = "linux")]
#[test]
fn a_stdout_that_cannot_be_written_exits_1_with_the_error_on_stderr() {
    let full = |args: &[&str]| {
        let stdout = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/full")
            .unwrap();
        Command::new(env!("CARGO_BIN_EXE_togi"))
            .args(args)
            .stdout(stdout)
            .output()
            .unwrap()
    };
    for args in [
        &["completion", "bash"][..],
        &["completion", "nushell"],
        &["--help"],
        &["completion", "--help"],
        &["--version"],
    ] {
        let ran = full(args);
        assert_eq!(ran.status.code(), Some(1), "{args:?}: {}", stderr(&ran));
        assert!(
            stderr(&ran).starts_with("togi: ") && stderr(&ran).contains("No space left on device"),
            "{args:?}: {}",
            stderr(&ran)
        );
    }
    let ran = full(&["bogus"]);
    assert_eq!(ran.status.code(), Some(2), "a usage error stays exit 2");
    assert!(stderr(&ran).contains("unrecognized subcommand"));
}

const SCOPE: &str = "--config applies only to doctor, run and reset";

#[test]
fn usage_errors_exit_2_on_stderr_with_nothing_on_stdout() {
    for (args, text) in [
        (&["bogus"][..], "unrecognized subcommand 'bogus'"),
        (&["--bogus"], "unexpected argument '--bogus'"),
        (
            &["--state-dir"],
            "a value is required for '--state-dir <path>'",
        ),
        (&["completion"], "required arguments were not provided"),
        (
            &["completion", "tcsh"],
            "invalid value 'tcsh' for '<SHELL>'",
        ),
        (&["completion", "bash", "zsh"], "unexpected argument 'zsh'"),
        (&["--config", "c.json", "completion", "bash"], SCOPE),
        (&["--config=c.json", "completion", "bash"], SCOPE),
        (&["completion", "bash", "--config", "c.json"], SCOPE),
        (&["--config", "c.json"], SCOPE),
    ] {
        let ran = togi(args);
        assert_eq!(ran.status.code(), Some(2), "{args:?}");
        assert_eq!(stdout(&ran), "", "{args:?}");
        assert!(stderr(&ran).contains(text), "{args:?}: {}", stderr(&ran));
    }
}

#[test]
fn a_missing_shell_error_names_the_argument() {
    let ran = togi(&["completion"]);
    assert!(stderr(&ran).contains("<SHELL>"), "{}", stderr(&ran));
    insta::assert_snapshot!(stderr(&ran));
}

fn completion(shell: &str) -> String {
    let ran = togi(&["completion", shell]);
    assert_eq!(ran.status.code(), Some(0), "{shell}: {}", stderr(&ran));
    assert_eq!(stderr(&ran), "", "{shell}");
    stdout(&ran).to_owned()
}

#[test]
fn bash_completion() {
    insta::assert_snapshot!(completion("bash"));
}

#[test]
fn zsh_completion() {
    insta::assert_snapshot!(completion("zsh"));
}

#[test]
fn fish_completion() {
    insta::assert_snapshot!(completion("fish"));
}

#[test]
fn nushell_completion() {
    insta::assert_snapshot!(completion("nushell"));
}

#[test]
fn completion_reads_and_writes_nothing_in_the_state_directory() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_str().unwrap();
    for args in [
        ["--state-dir", state, "completion", "fish"],
        ["completion", "fish", "--state-dir", state],
    ] {
        let ran = togi(&args);
        assert_eq!(ran.status.code(), Some(0), "{args:?}: {}", stderr(&ran));
        assert!(!stdout(&ran).is_empty());
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
}

#[test]
fn nushell_loads_the_script_without_warnings() {
    let nu = Command::new("nu").arg("--version").output();
    assert!(
        nu.is_ok(),
        "nu is not on PATH: the dev shell and the rust-nextest check provide it, and without it this test would pass without loading the script"
    );
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("togi.nu");
    std::fs::write(&script, completion("nushell")).unwrap();
    let ran = Command::new("nu")
        .args(["--no-config-file", "--no-history", "-c"])
        .arg(format!(
            "source '{}'; 'togi completion ' | commandline complete | str join ' '",
            script.display()
        ))
        .current_dir(dir.path())
        .env("HOME", dir.path())
        .env("XDG_CONFIG_HOME", dir.path())
        .env("XDG_DATA_HOME", dir.path())
        .env("NO_COLOR", "1")
        .output()
        .unwrap();
    assert_eq!(ran.status.code(), Some(0), "{}", stderr(&ran));
    assert_eq!(stderr(&ran), "");
    let offered: Vec<&str> = stdout(&ran).split_whitespace().collect();
    for shell in ["bash", "zsh", "fish", "nushell"] {
        assert!(offered.contains(&shell), "{shell} not offered: {offered:?}");
    }
}

fn help(command: &str) -> String {
    let ran = togi(&[command, "--help"]);
    assert_eq!(ran.status.code(), Some(0), "{command}");
    assert_eq!(stderr(&ran), "", "{command}");
    stdout(&ran).to_owned()
}

#[test]
fn reset_help() {
    insta::assert_snapshot!(help("reset"));
}

#[test]
fn doctor_help() {
    insta::assert_snapshot!(help("doctor"));
}

#[test]
fn restart_limit_help() {
    insta::assert_snapshot!(help("restart-limit"));
}

#[test]
fn only_doctor_and_reset_list_config() {
    for command in ["doctor", "reset"] {
        assert!(help(command).contains("--config"), "{command}");
    }
    assert!(!help("restart-limit").contains("--config"));
}

#[test]
fn administration_usage_errors_exit_2_before_touching_anything() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state");
    let state = state.to_str().unwrap();
    for (args, text) in [
        (&["reset"][..], "required arguments were not provided"),
        (&["reset", "--core", "1", "--all"], "cannot be used with"),
        (&["reset", "--core", "x"], "invalid value 'x'"),
        (&["reset", "--core", "-1"], "unexpected argument"),
        (&["reset", "--all", "extra"], "unexpected argument 'extra'"),
        (&["restart-limit"], "required arguments were not provided"),
        (&["restart-limit", "--tuning-boot="], "a value is required"),
        (
            &["restart-limit", "--tuning-boot", "g", "extra"],
            "unexpected argument 'extra'",
        ),
        (
            &["restart-limit", "--config", "c.json", "--tuning-boot", "g"],
            SCOPE,
        ),
        (
            &["--config", "c.json", "restart-limit", "--tuning-boot", "g"],
            SCOPE,
        ),
        (&["doctor", "extra"], "unexpected argument 'extra'"),
        (&["doctor", "--version"], "unexpected argument '--version'"),
    ] {
        let mut all = vec!["--state-dir", state];
        all.extend_from_slice(args);
        let ran = togi(&all);
        assert_eq!(ran.status.code(), Some(2), "{args:?}: {}", stderr(&ran));
        assert_eq!(stdout(&ran), "", "{args:?}");
        assert!(stderr(&ran).contains(text), "{args:?}: {}", stderr(&ran));
    }
    assert!(
        !std::path::Path::new(state).exists(),
        "a usage error created the state directory"
    );
}

fn session_start(schema: u32) -> String {
    format!(
        "{{\"seq\":1,\"time\":\"2026-01-01T00:00:00Z\",\"boot\":\"b1a8cebe-21d1-14db-3741-5b5df946de05\",\"kind\":\"session.start\",\"msg\":\"x\",\"data\":{{\"build\":{{\"version\":\"9.9.9\",\"ruleset\":1,\"schema\":{schema},\"fixes\":0,\"evidence\":1}},\"session\":\"20260101T000000Z\",\"cores\":[]}}}}\n"
    )
}

#[test]
#[cfg(target_os = "linux")]
fn doctor_refuses_a_newer_journal_with_exit_16_before_any_check() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("events.jsonl"), session_start(99)).unwrap();
    let state = dir.path().to_str().unwrap();
    for args in [
        ["--state-dir", state, "doctor"],
        ["doctor", "--state-dir", state],
    ] {
        let ran = togi(&args);
        assert_eq!(ran.status.code(), Some(16), "{args:?}: {}", stderr(&ran));
        assert_eq!(stdout(&ran), "");
        assert!(
            stderr(&ran).starts_with("togi doctor: this journal was written by togi 9.9.9"),
            "{}",
            stderr(&ran)
        );
    }
}

#[test]
#[cfg(target_os = "linux")]
fn doctor_exits_1_for_an_unreadable_journal_and_2_for_a_bad_config() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().to_str().unwrap();
    let missing = dir.path().join("none.json");
    let ran = togi(&[
        "--state-dir",
        state,
        "doctor",
        "--config",
        missing.to_str().unwrap(),
    ]);
    assert_eq!(ran.status.code(), Some(2), "{}", stderr(&ran));
    assert!(stderr(&ran).contains("none.json"), "{}", stderr(&ran));
    let ran = togi(&[
        "--config",
        missing.to_str().unwrap(),
        "--state-dir",
        state,
        "doctor",
    ]);
    assert_eq!(ran.status.code(), Some(2), "{}", stderr(&ran));

    std::fs::write(dir.path().join("events.jsonl"), "not json\n").unwrap();
    let ran = togi(&["--state-dir", state, "doctor"]);
    assert_eq!(ran.status.code(), Some(1), "{}", stderr(&ran));
    assert_eq!(stdout(&ran), "");
}
