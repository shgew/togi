use std::fs;
use std::process::{Command, Output};

use tempfile::tempdir;
use togi_tool_trialfacts::read;

use crate::fixture::{corrupt_state_dir, state_dir};

fn facts(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_togi-tool-facts"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn the_command_prints_the_count_and_writes_the_extract() {
    let source = tempdir().unwrap();
    let state = state_dir(source.path(), true);
    let out = tempdir().unwrap();
    let output = out.path().join("facts.jsonl.gz");

    let ran = facts(&[state.as_os_str(), output.as_os_str()]);

    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(ran.stdout).unwrap(),
        "extracted 1 facts\n"
    );
    assert_eq!(String::from_utf8(ran.stderr).unwrap(), "");
    assert_eq!(read(&output).unwrap().len(), 1);
}

#[test]
fn a_failed_extraction_exits_one_names_the_tool_on_stderr_and_keeps_the_output() {
    let source = tempdir().unwrap();
    let state = corrupt_state_dir(source.path());
    let out = tempdir().unwrap();
    let output = out.path().join("facts.jsonl.gz");
    fs::write(&output, b"committed extract").unwrap();

    let ran = facts(&[state.as_os_str(), output.as_os_str()]);

    assert_eq!(ran.status.code(), Some(1));
    assert_eq!(String::from_utf8(ran.stdout).unwrap(), "");
    let stderr = String::from_utf8(ran.stderr).unwrap();
    assert!(stderr.starts_with("togi-tool-facts: "), "{stderr}");
    assert_eq!(fs::read(&output).unwrap(), b"committed extract");
}

#[cfg(target_os = "linux")]
#[test]
fn a_count_that_cannot_be_written_exits_one_and_names_the_write() {
    let source = tempdir().unwrap();
    let state = state_dir(source.path(), true);
    let out = tempdir().unwrap();
    let output = out.path().join("facts.jsonl.gz");
    let full = fs::File::options().write(true).open("/dev/full").unwrap();

    let ran = Command::new(env!("CARGO_BIN_EXE_togi-tool-facts"))
        .args([state.as_os_str(), output.as_os_str()])
        .stdout(std::process::Stdio::from(full))
        .output()
        .unwrap();

    assert_eq!(ran.status.code(), Some(1));
    let stderr = String::from_utf8(ran.stderr).unwrap();
    assert!(
        stderr.starts_with("togi-tool-facts: write the count"),
        "{stderr}"
    );
}

#[test]
fn a_bare_output_name_is_written_in_the_working_directory() {
    let source = tempdir().unwrap();
    let state = state_dir(source.path(), true);
    let out = tempdir().unwrap();

    let ran = Command::new(env!("CARGO_BIN_EXE_togi-tool-facts"))
        .current_dir(out.path())
        .args([state.as_os_str(), "facts.jsonl.gz".as_ref()])
        .output()
        .unwrap();

    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(read(out.path().join("facts.jsonl.gz")).unwrap().len(), 1);
}

#[test]
fn a_wrong_argument_count_is_a_usage_error() {
    for args in [&[][..], &["only-one"][..], &["a", "b", "c"][..]] {
        let args: Vec<&std::ffi::OsStr> = args.iter().map(std::ffi::OsStr::new).collect();
        let ran = facts(&args);
        assert_eq!(ran.status.code(), Some(2), "{args:?}");
        assert!(
            String::from_utf8(ran.stderr).unwrap().contains("Usage"),
            "{args:?}"
        );
    }
}
