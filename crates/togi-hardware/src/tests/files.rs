use std::fs;

use tempfile::TempDir;

use crate::files::read_attribute;

#[test]
fn reads_a_small_regular_file() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("state");
    fs::write(&path, "active\n").unwrap();
    assert_eq!(read_attribute(&path).unwrap(), "active\n");
}

#[test]
fn refuses_a_directory() {
    let dir = TempDir::new().unwrap();
    let error = read_attribute(dir.path()).unwrap_err();
    assert!(error.to_string().contains("not a regular file"), "{error}");
}

#[test]
fn refuses_a_file_over_four_kib() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("big");
    fs::write(&path, vec![b'a'; 4097]).unwrap();
    assert!(read_attribute(&path).is_err());
    fs::write(&path, vec![b'a'; 4096]).unwrap();
    assert_eq!(read_attribute(&path).unwrap().len(), 4096);
}

#[test]
fn a_missing_file_keeps_its_not_found_kind() {
    let dir = TempDir::new().unwrap();
    let error = read_attribute(&dir.path().join("absent")).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}
