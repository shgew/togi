//! The built command's exit status and standard error, which `just cover` relies on to stop.
#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "the tests run the built command and panic on a broken fixture"
)]

use std::process::{Command, Output};

fn cover(args: &[&str]) -> Output {
    let dir = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_togi-tool-cover"))
        .args(args)
        .current_dir(dir.path())
        .output()
        .unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn real_process_help_exits_zero_with_the_usage_on_standard_error() {
    let output = cover(&["-h"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stdout.is_empty());
    assert!(stderr(&output).starts_with("Usage: cover --lcov TRACE --build-dir DIR --base REV\n"));
}

#[test]
fn real_process_an_unknown_flag_exits_one_with_the_usage() {
    let output = cover(&["--profile", "p", "--base", "main"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = stderr(&output);
    assert!(stderr.starts_with("cover: flag provided but not defined: --profile\nUsage: cover "));
}

#[test]
fn real_process_a_refused_report_exits_one_and_prints_nothing() {
    let output = cover(&[
        "--lcov",
        "missing.info",
        "--build-dir",
        "d",
        "--base",
        "HEAD",
    ]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        stderr(&output),
        "cover: read the lcov trace missing.info: No such file or directory (os error 2)\n"
    );
}
