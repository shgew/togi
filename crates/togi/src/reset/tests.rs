use super::remove_durably;

#[test]
fn a_removed_file_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");
    std::fs::write(&path, "").unwrap();
    remove_durably(&path).unwrap();
    assert!(!path.exists());
}

#[test]
fn a_failed_removal_names_the_step_and_the_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");
    let error = remove_durably(&path).unwrap_err();
    assert!(
        error.starts_with(&format!("remove {}: ", path.display())),
        "{error}"
    );
}
