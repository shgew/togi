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
fn a_missing_shell_error_names_the_shells() {
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
    if nu.is_err() {
        assert!(
            std::env::var_os("TOGI_DEV_SHELL").is_none(),
            "nu is not on PATH in the dev shell"
        );
        return;
    }
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
