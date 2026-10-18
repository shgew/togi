use test_case::test_case;

use std::path::Path;
use std::process::ExitCode;

use clap::Parser;
use togi_tool_facts::{GenerateError, generate_recordings};
use togi_tool_trialfacts::decode;

use crate::fixture::{SESSION, recordings, unknown_recording};
use crate::{Cli, run};

fn cli() -> Cli {
    Cli::try_parse_from(["togi-tool-facts", "state", "facts.jsonl.gz"]).unwrap()
}

#[test]
fn the_command_prints_the_count_and_writes_the_extract() {
    let mut output = Vec::new();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let status = run(
        &cli(),
        |state, path| {
            assert_eq!(state, Path::new("state"));
            assert_eq!(path, Path::new("facts.jsonl.gz"));
            generate_recordings(recordings(true), &mut output)
        },
        &mut stdout,
        &mut stderr,
    );
    assert_eq!(status, ExitCode::SUCCESS);
    assert_eq!(stdout, b"extracted 1 facts\n");
    assert!(stderr.is_empty());
    assert_eq!(decode(output.as_slice()).unwrap().len(), 1);
}

#[test]
fn an_event_of_an_unknown_kind_is_skipped_named_on_stderr_and_does_not_fail_the_command() {
    let mut output = Vec::new();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let status = run(
        &cli(),
        |_, _| generate_recordings([unknown_recording()], &mut output),
        &mut stdout,
        &mut stderr,
    );
    assert_eq!(status, ExitCode::SUCCESS);
    assert_eq!(stdout, b"extracted 1 facts\n");
    assert_eq!(
        String::from_utf8(stderr).unwrap(),
        format!(
            "togi-tool-facts: session {SESSION}: skipped 1 event of a kind this build does not know (future.thing at line 3)\n"
        )
    );
    assert_eq!(decode(output.as_slice()).unwrap()[0].seq.get(), 5);
}

#[test]
fn a_failed_extraction_exits_one_names_the_tool_on_stderr_and_keeps_the_output() {
    let mut output = b"committed extract".to_vec();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let status = run(
        &cli(),
        |_, _| generate_recordings(recordings(false), &mut output),
        &mut stdout,
        &mut stderr,
    );
    assert_eq!(status, ExitCode::FAILURE);
    assert!(stdout.is_empty());
    assert!(
        String::from_utf8(stderr)
            .unwrap()
            .starts_with("togi-tool-facts: ")
    );
    assert_eq!(output, b"committed extract");
}

#[test]
fn an_unreadable_state_directory_prints_the_operation_and_its_io_cause_on_one_line() {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let status = run(
        &cli(),
        |_, _| {
            Err(GenerateError::StateDir {
                path: "state".into(),
                source: std::io::ErrorKind::NotFound.into(),
            })
        },
        &mut stdout,
        &mut stderr,
    );
    assert_eq!(status, ExitCode::FAILURE);
    assert!(stdout.is_empty());
    assert_eq!(
        String::from_utf8(stderr).unwrap(),
        "togi-tool-facts: read the state directory state: entity not found\n"
    );
}

#[test]
fn a_count_that_cannot_be_written_exits_one_and_names_the_write() {
    struct Full;
    impl std::io::Write for Full {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::StorageFull.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Vec::new();
    let mut stderr = Vec::new();
    let status = run(
        &cli(),
        |_, _| generate_recordings(recordings(true), &mut output),
        &mut Full,
        &mut stderr,
    );
    assert_eq!(status, ExitCode::FAILURE);
    assert!(
        String::from_utf8(stderr)
            .unwrap()
            .starts_with("togi-tool-facts: write the count")
    );
    assert_eq!(decode(output.as_slice()).unwrap().len(), 1);
}

#[test]
fn a_bare_output_name_is_passed_to_the_publication_adapter_unchanged() {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    assert_eq!(
        run(
            &cli(),
            |_, output| {
                assert_eq!(output, Path::new("facts.jsonl.gz"));
                generate_recordings(recordings(true), Vec::new())
            },
            &mut stdout,
            &mut stderr
        ),
        ExitCode::SUCCESS
    );
}

#[test_case(&["togi-tool-facts"]; "no arguments")]
#[test_case(&["togi-tool-facts", "one"]; "one argument")]
#[test_case(&["togi-tool-facts", "a", "b", "c"]; "three arguments")]
fn a_wrong_argument_count_is_a_usage_error(args: &[&str]) {
    let error = Cli::try_parse_from(args).unwrap_err();
    assert_eq!(error.exit_code(), 2);
    assert!(error.to_string().contains("Usage"));
}
