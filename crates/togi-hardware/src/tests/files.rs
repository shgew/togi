use tempfile::TempDir;

use crate::files::{read_attribute, read_attribute_from};

#[test]
fn reads_a_small_regular_file() {
    assert_eq!(
        read_attribute_from(Ok("active\n".as_bytes())).unwrap(),
        "active\n"
    );
}

#[test]
fn real_fs_refuses_a_directory() {
    let dir = TempDir::new().unwrap();
    let error = read_attribute(dir.path()).unwrap_err();
    assert!(error.to_string().contains("not a regular file"), "{error}");
}

#[test]
fn refuses_a_file_over_four_kib() {
    assert!(read_attribute_from(Ok(vec![b'a'; 4097].as_slice())).is_err());
    assert_eq!(
        read_attribute_from(Ok(vec![b'a'; 4096].as_slice()))
            .unwrap()
            .len(),
        4096
    );
}

#[test]
fn a_missing_file_keeps_its_not_found_kind() {
    let error =
        read_attribute_from(Err::<&[u8], _>(std::io::ErrorKind::NotFound.into())).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
}
