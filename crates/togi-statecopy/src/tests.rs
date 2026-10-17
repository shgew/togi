use std::path::Path;

use test_case::test_case;
use togi_journal::MemFs;

use super::*;

const SESSION_BEARING: [&str; 4] = [
    "20260101T000000Z.jsonl",
    "20260101T000000Z-reset-all",
    "20260102T000000Z-carry-pending",
    "20260102T000000Z-compat-pending",
];

fn state(fs: &MemFs) {
    fs.put("/state/events.jsonl", "live\n");
    fs.put("/state/state.json", "{}");
    fs.put("/state/lock", "");
    fs.put("/state/trials/t/samples.jsonl.gz", "x");
    for name in SESSION_BEARING {
        fs.put(format!("/state/archive/{name}"), name);
    }
    fs.put("/state/archive/notes", "x");
    fs.put(
        "/state/archive/20260101T000000Z-trials/0001/sample.csv",
        "x",
    );
}

fn files_under(fs: &MemFs, root: &str) -> Vec<(String, Vec<u8>)> {
    fs.entries()
        .into_iter()
        .filter(|(_, kind)| *kind == FileKind::File)
        .filter_map(|(path, _)| {
            let relative = path.strip_prefix(root).ok()?.to_str()?.to_owned();
            Some((relative, fs.contents(&path)?))
        })
        .collect()
}

#[test]
fn the_live_journal_state_and_session_bearing_archive_entries_are_copied_and_nothing_else() {
    let fs = MemFs::new();
    state(&fs);
    let before = files_under(&fs, "/state");
    copy_in(&fs, Path::new("/state"), Path::new("/copy/nested")).unwrap();
    let mut expected = vec![
        ("events.jsonl".to_owned(), b"live\n".to_vec()),
        ("state.json".to_owned(), b"{}".to_vec()),
    ];
    expected
        .extend(SESSION_BEARING.map(|name| (format!("archive/{name}"), name.as_bytes().to_vec())));
    expected.sort();
    assert_eq!(files_under(&fs, "/copy/nested"), expected);
    assert_eq!(files_under(&fs, "/state"), before);
}

#[test]
fn a_directory_without_state_or_archive_copies_the_live_journal_alone() {
    let fs = MemFs::new();
    fs.put("/state/events.jsonl", "live\n");
    copy_in(&fs, Path::new("/state"), Path::new("/copy")).unwrap();
    assert_eq!(
        files_under(&fs, "/copy"),
        [("events.jsonl".to_owned(), b"live\n".to_vec())]
    );
    assert_eq!(fs.kind(Path::new("/copy/archive")).unwrap(), None);
}

#[test]
fn a_missing_live_journal_fails_naming_it() {
    let fs = MemFs::new();
    fs.put("/state/state.json", "{}");
    let error = copy_in(&fs, Path::new("/state"), Path::new("/copy")).unwrap_err();
    assert_eq!(
        togi_machine::error_chain(&error),
        "read /state/events.jsonl: entity not found"
    );
}

#[test]
fn a_session_bearing_entry_that_is_a_directory_fails_naming_it_before_anything_is_written() {
    let fs = MemFs::new();
    fs.put("/state/events.jsonl", "live\n");
    fs.put("/state/archive/20260101T000000Z-reset-all/inside", "");
    let error = copy_in(&fs, Path::new("/state"), Path::new("/copy")).unwrap_err();
    assert_eq!(
        togi_machine::error_chain(&error),
        "list the archive of /state: /state/archive/20260101T000000Z-reset-all is not a regular file"
    );
    assert_eq!(fs.kind(Path::new("/copy")).unwrap(), None);
}

#[test_case("20260101T000000Z.jsonl"; "archived journal")]
#[test_case("20260101T000000Z-reset-all"; "reset marker")]
#[test_case("20260101T000000Z-carry-pending"; "carry marker")]
#[test_case("20260101T000000Z-compat-pending"; "compatibility marker")]
fn real_fs_a_symlinked_session_bearing_entry_fails_naming_it(name: &str) {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("state");
    std::fs::create_dir_all(src.join("archive")).unwrap();
    std::fs::write(src.join("events.jsonl"), "live\n").unwrap();
    std::fs::write(root.path().join("elsewhere"), "archived\n").unwrap();
    std::os::unix::fs::symlink(
        root.path().join("elsewhere"),
        src.join("archive").join(name),
    )
    .unwrap();
    let error = copy(&src, &root.path().join("copy")).unwrap_err();
    assert!(togi_machine::error_chain(&error).contains(name), "{error}");
    assert!(!root.path().join("copy").exists());
}

#[test]
fn real_fs_a_copy_on_disk_holds_the_same_bytes() {
    let root = tempfile::tempdir().unwrap();
    let src = root.path().join("state");
    std::fs::create_dir_all(src.join("archive")).unwrap();
    std::fs::write(src.join("events.jsonl"), "live\n").unwrap();
    std::fs::write(src.join("archive/20260101T000000Z.jsonl"), "archived\n").unwrap();
    let dst = root.path().join("copy");
    copy(&src, &dst).unwrap();
    assert_eq!(std::fs::read(dst.join("events.jsonl")).unwrap(), b"live\n");
    assert_eq!(
        std::fs::read(dst.join("archive/20260101T000000Z.jsonl")).unwrap(),
        b"archived\n"
    );
    assert!(!dst.join("state.json").exists());
}
