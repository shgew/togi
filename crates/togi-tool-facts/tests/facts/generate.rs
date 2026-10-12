use std::fs;
use std::path::Path;

use tempfile::tempdir;
use togi_tool_facts::generate;
use togi_tool_trialfacts::read;

use crate::fixture::{SESSION, corrupt_state_dir, state_dir};

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
fn success_replaces_the_output_with_the_extracted_facts() {
    let source = tempdir().unwrap();
    let state = state_dir(source.path(), true);
    let out = tempdir().unwrap();
    let output = out.path().join("facts.jsonl.gz");
    fs::write(&output, COMMITTED).unwrap();

    assert_eq!(generate(&state, &output).unwrap(), 1);

    let facts = read(&output).unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(facts[0].session.to_string(), SESSION);
    assert_eq!(facts[0].trial.as_ref().unwrap().to_string(), "1");
    assert_eq!(facts[0].profile.as_slice().len(), 2);
    assert_eq!(names(out.path()), ["facts.jsonl.gz"]);
}

#[test]
fn success_creates_an_output_that_does_not_exist_yet() {
    let source = tempdir().unwrap();
    let state = state_dir(source.path(), true);
    let out = tempdir().unwrap();
    let output = out.path().join("facts.jsonl.gz");

    assert_eq!(generate(&state, &output).unwrap(), 1);
    assert_eq!(read(&output).unwrap().len(), 1);
}

#[test]
fn a_source_that_yields_no_facts_leaves_the_output_untouched() {
    let source = tempdir().unwrap();
    let no_facts = state_dir(source.path(), false);
    let empty = source.path().join("empty");
    fs::create_dir(&empty).unwrap();
    let corrupt = corrupt_state_dir(source.path());
    let missing = source.path().join("missing");

    for (name, state) in [
        ("session without trials", no_facts),
        ("empty directory", empty),
        ("invalid journal", corrupt),
        ("missing directory", missing),
    ] {
        let out = tempdir().unwrap();
        let output = out.path().join("facts.jsonl.gz");
        fs::write(&output, COMMITTED).unwrap();

        assert!(generate(&state, &output).is_err(), "{name} was accepted");
        assert_eq!(fs::read(&output).unwrap(), COMMITTED, "{name}");
        assert_eq!(names(out.path()), ["facts.jsonl.gz"], "{name}");
    }
}

#[test]
fn a_missing_state_directory_is_refused_without_creating_it_or_its_parents() {
    let source = tempdir().unwrap();
    let missing = source.path().join("typo").join("state");
    let out = tempdir().unwrap();
    let output = out.path().join("facts.jsonl.gz");

    let error = generate(&missing, &output).unwrap_err();

    assert!(
        error.to_string().starts_with("read the state directory "),
        "{error}"
    );
    assert!(!source.path().join("typo").exists());
    assert_eq!(names(out.path()), Vec::<String>::new());
}

#[test]
fn a_regular_file_in_place_of_the_state_directory_is_refused() {
    let source = tempdir().unwrap();
    let file = source.path().join("state");
    fs::write(&file, COMMITTED).unwrap();
    let out = tempdir().unwrap();
    let output = out.path().join("facts.jsonl.gz");

    let error = generate(&file, &output).unwrap_err();

    assert!(
        error.to_string().starts_with("read the state directory "),
        "{error}"
    );
    assert!(file.is_file());
    assert_eq!(names(source.path()), ["state"]);
    assert_eq!(names(out.path()), Vec::<String>::new());
}

#[test]
fn a_missing_output_directory_fails_without_creating_anything() {
    let source = tempdir().unwrap();
    let state = state_dir(source.path(), true);
    let out = tempdir().unwrap();
    let output = out.path().join("missing").join("facts.jsonl.gz");

    let error = generate(&state, &output).unwrap_err();

    assert!(error.to_string().contains("write extract"), "{error}");
    assert_eq!(names(out.path()), Vec::<String>::new());
}

#[test]
fn a_directory_in_place_of_the_output_is_not_replaced_and_leaves_no_temporary_file() {
    let source = tempdir().unwrap();
    let state = state_dir(source.path(), true);
    let out = tempdir().unwrap();
    let output = out.path().join("facts.jsonl.gz");
    fs::create_dir(&output).unwrap();
    fs::write(output.join("keep"), COMMITTED).unwrap();

    let error = generate(&state, &output).unwrap_err();

    assert!(error.to_string().contains("replace"), "{error}");
    assert!(output.is_dir());
    assert_eq!(fs::read(output.join("keep")).unwrap(), COMMITTED);
    assert_eq!(names(out.path()), ["facts.jsonl.gz"]);
}
