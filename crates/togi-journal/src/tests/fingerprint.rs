//! What the source fingerprint of a snapshot covers: a build that differs only in test-only files has the same
//! fingerprint, so it accepts the snapshot; a production change has another one, so the build rejects the snapshot
//! and replays the journal.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use test_case::test_case;

#[path = "../../build/fingerprint.rs"]
mod inputs;

fn write(root: &Path, name: &str, contents: &str) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// Two crates and the files around them, with test-only files of every kind the rule names.
fn workspace() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    for (name, contents) in [
        ("Cargo.toml", "[workspace]\n"),
        ("Cargo.lock", "version = 4\n"),
        ("rust-toolchain.toml", "[toolchain]\n"),
        ("version.txt", "1.0.0\n"),
        (
            "crates/test-only.txt",
            "# Compiled only under cfg(test).\na/src/support.rs\na/src/fakes\n",
        ),
        ("crates/a/Cargo.toml", "[package]\n"),
        ("crates/a/src/lib.rs", "mod model;\n"),
        ("crates/a/src/model/mod.rs", "pub struct Model;\n"),
        ("crates/b/Cargo.toml", "[package]\n"),
        ("crates/b/src/lib.rs", "pub struct Other;\n"),
        ("crates/a/src/tests.rs", "#[test] fn one() {}\n"),
        ("crates/a/src/model_tests.rs", "#[test] fn two() {}\n"),
        ("crates/a/src/tests/case.rs", "#[test] fn three() {}\n"),
        (
            "crates/a/src/model/model_tests/case.rs",
            "#[test] fn four() {}\n",
        ),
        ("crates/a/src/support.rs", "pub fn listed() {}\n"),
        (
            "crates/a/src/fakes/clock.rs",
            "pub fn listed_directory() {}\n",
        ),
        ("crates/a/tests/cli.rs", "#[test] fn five() {}\n"),
    ] {
        write(root, name, contents);
    }
    dir
}

fn fingerprint(root: &Path, revision: &str) -> String {
    let files = inputs::files(
        root,
        &["a", "b"],
        &[
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            "version.txt",
        ],
    )
    .unwrap();
    inputs::digest(root, &files, revision).unwrap()
}

#[test_case("crates/a/src/tests.rs"; "tests.rs")]
#[test_case("crates/a/src/model_tests.rs"; "a _tests.rs file")]
#[test_case("crates/a/src/tests/case.rs"; "a tests directory")]
#[test_case("crates/a/src/tests/added.rs"; "a file added to a tests directory")]
#[test_case("crates/a/src/model/model_tests/case.rs"; "a nested _tests directory")]
#[test_case("crates/a/src/support.rs"; "a listed file")]
#[test_case("crates/a/src/fakes/clock.rs"; "a file in a listed directory")]
#[test_case("crates/a/tests/cli.rs"; "an integration test")]
fn real_fs_test_only_edits_keep_the_fingerprint(edited: &str) {
    let dir = workspace();
    let before = fingerprint(dir.path(), "dev");
    write(dir.path(), edited, "// edited\n");
    assert_eq!(fingerprint(dir.path(), "dev"), before);
}

#[test_case("crates/a/src/lib.rs"; "a production file")]
#[test_case("crates/a/src/model/mod.rs"; "a nested production file")]
#[test_case("crates/a/src/added.rs"; "a production file added")]
#[test_case("crates/b/src/lib.rs"; "another crate")]
#[test_case("crates/a/Cargo.toml"; "a manifest")]
#[test_case("crates/test-only.txt"; "the test-only list")]
#[test_case("Cargo.lock"; "the lock file")]
#[test_case("version.txt"; "the version")]
fn real_fs_production_edits_change_the_fingerprint(edited: &str) {
    let dir = workspace();
    let before = fingerprint(dir.path(), "dev");
    write(dir.path(), edited, "// edited\n");
    assert_ne!(fingerprint(dir.path(), "dev"), before);
}

#[test]
fn real_fs_the_revision_changes_the_fingerprint() {
    let dir = workspace();
    assert_ne!(
        fingerprint(dir.path(), "dev"),
        fingerprint(dir.path(), "abc1234")
    );
}

#[test]
fn real_fs_a_missing_test_only_list_is_an_error() {
    let dir = workspace();
    fs::remove_file(dir.path().join(inputs::TEST_ONLY_LIST)).unwrap();
    assert!(inputs::files(dir.path(), &["a"], &[]).is_err());
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn real_fs_the_build_fingerprint_is_this_modules_digest_of_the_build_source() {
    let root = repository();
    // The sandbox includes every reducer's production source for build.rs, but not every crate's tests.
    let files = inputs::reducer_files(&root).unwrap();
    assert_eq!(
        inputs::digest(&root, &files, option_env!("TOGI_REV").unwrap_or("dev")).unwrap(),
        env!("TOGI_SNAPSHOT_BUILD")
    );
}

#[test_case("crates/togi-journal/src/lib.rs", true; "production source")]
#[test_case("crates/togi-journal/build/fingerprint.rs", true; "the fingerprint module")]
#[test_case("crates/togi-journal/src/tests/fingerprint.rs", false; "a tests directory")]
#[test_case("crates/togi-journal/src/snapshot_tests.rs", false; "a _tests.rs file")]
#[test_case("crates/togi-journal/src/testing.rs", false; "a listed test module")]
fn real_fs_the_build_source_fingerprint_covers_production_source_only(file: &str, covered: bool) {
    let root = repository();
    assert!(
        root.join(file).exists(),
        "{file} must be present to test its coverage"
    );
    let files = inputs::reducer_files(&root).unwrap();
    assert_eq!(files.contains(&root.join(file)), covered);
}
