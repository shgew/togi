use std::fs;
use std::path::Path;

use tempfile::tempdir;
use test_case::test_case;
use togi_journal::Recording;
use togi_tool_trialfacts::{ExtractError, decode, from_recordings, read, write_unsynced};

use crate::fixture::{SESSION, recordings, unknown_recording};
use crate::{finish, generate, generate_recordings};

const COMMITTED: &[u8] = b"committed extract";

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn the_extracted_facts_are_encoded_in_memory() {
    let mut output = Vec::new();
    assert_eq!(
        generate_recordings(recordings(true), &mut output)
            .unwrap()
            .count,
        1
    );
    let facts = decode(output.as_slice()).unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].session.to_string(), SESSION);
    assert_eq!(facts[0].trial.as_ref().unwrap().to_string(), "1");
    assert_eq!(facts[0].profile.as_slice().len(), 2);
}

#[test_case(recordings(false); "recordings without facts")]
#[test_case(Vec::new(); "no recordings")]
fn a_source_that_yields_no_facts_leaves_the_output_untouched(recordings: Vec<Recording>) {
    let mut output = COMMITTED.to_vec();
    assert!(generate_recordings(recordings, &mut output).is_err());
    assert_eq!(output, COMMITTED);
}

#[test]
fn no_facts_does_not_publish_the_output() {
    let mut called = false;
    assert!(
        finish(Err(ExtractError::NoFacts), |_| {
            called = true;
            Ok(())
        })
        .is_err()
    );
    assert!(!called);
}

#[test]
fn an_invalid_journal_leaves_the_output_untouched() {
    let (fs, dir) = crate::fixture::state(true);
    fs.put(dir.path().join("events.jsonl"), b"{invalid}\n".to_vec());
    let mut output = COMMITTED.to_vec();
    let error = finish(togi_tool_trialfacts::extract(&dir), |facts| {
        togi_tool_trialfacts::encode(&mut output, facts)
    })
    .unwrap_err();
    assert!(matches!(
        error,
        crate::GenerateError::Extract(ExtractError::Journals(_))
    ));
    assert_eq!(output, COMMITTED);
}

#[test]
fn unknown_events_are_reported_without_losing_the_trial() {
    let mut output = Vec::new();
    let generated = generate_recordings([unknown_recording()], &mut output).unwrap();
    assert_eq!(generated.count, 1);
    assert_eq!(
        generated.skipped,
        [format!(
            "session {SESSION}: skipped 1 event of a kind this build does not know (future.thing at line 3)"
        )]
    );
    assert_eq!(decode(output.as_slice()).unwrap()[0].seq.get(), 5);
}

#[test]
fn a_failed_write_is_reported() {
    struct Fails;
    impl std::io::Write for Fails {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::StorageFull.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let error = generate_recordings(recordings(true), Fails).unwrap_err();
    assert!(error.to_string().contains("write extract"), "{error}");
}

#[expect(
    clippy::disallowed_methods,
    reason = "real_fs publication tests use a disposable unsynced extract"
)]
fn publish(output: &Path) -> Result<crate::Generated, crate::GenerateError> {
    finish(from_recordings(recordings(true)), |facts| {
        write_unsynced(output, facts)
    })
}

#[test]
fn real_fs_success_replaces_the_output_without_leaving_a_temporary_file() {
    let out = tempdir().unwrap();
    let output = out.path().join("facts.jsonl.gz");
    fs::write(&output, COMMITTED).unwrap();
    assert_eq!(publish(&output).unwrap().count, 1);
    assert_eq!(read(&output).unwrap().len(), 1);
    assert_eq!(names(out.path()), ["facts.jsonl.gz"]);
}

#[test]
fn real_fs_success_creates_an_output_that_does_not_exist_yet() {
    let out = tempdir().unwrap();
    let output = out.path().join("facts.jsonl.gz");
    assert_eq!(publish(&output).unwrap().count, 1);
    assert_eq!(read(&output).unwrap().len(), 1);
}

#[test]
fn real_fs_a_missing_state_directory_is_refused_without_creating_it_or_its_parents() {
    let source = tempdir().unwrap();
    let missing = source.path().join("typo/state");
    let out = tempdir().unwrap();
    let error = generate(&missing, out.path().join("facts.jsonl.gz")).unwrap_err();
    assert!(
        error.to_string().starts_with("read the state directory "),
        "{error}"
    );
    assert!(!source.path().join("typo").exists());
    assert_eq!(names(out.path()), Vec::<String>::new());
}

#[test]
fn real_fs_a_regular_file_in_place_of_the_state_directory_is_refused() {
    let source = tempdir().unwrap();
    let file = source.path().join("state");
    fs::write(&file, COMMITTED).unwrap();
    let out = tempdir().unwrap();
    let error = generate(&file, out.path().join("facts.jsonl.gz")).unwrap_err();
    assert!(
        error.to_string().starts_with("read the state directory "),
        "{error}"
    );
    assert!(file.is_file());
    assert_eq!(names(source.path()), ["state"]);
    assert_eq!(names(out.path()), Vec::<String>::new());
}

#[test]
fn real_fs_a_missing_output_directory_fails_without_creating_anything() {
    let out = tempdir().unwrap();
    let error = publish(&out.path().join("missing/facts.jsonl.gz")).unwrap_err();
    assert!(error.to_string().contains("write extract"), "{error}");
    assert_eq!(names(out.path()), Vec::<String>::new());
}

#[test]
fn real_fs_a_directory_in_place_of_the_output_is_not_replaced_and_leaves_no_temporary_file() {
    let out = tempdir().unwrap();
    let output = out.path().join("facts.jsonl.gz");
    fs::create_dir(&output).unwrap();
    fs::write(output.join("keep"), COMMITTED).unwrap();
    let error = publish(&output).unwrap_err();
    assert!(error.to_string().contains("replace"), "{error}");
    assert!(output.is_dir());
    assert_eq!(fs::read(output.join("keep")).unwrap(), COMMITTED);
    assert_eq!(names(out.path()), ["facts.jsonl.gz"]);
}
