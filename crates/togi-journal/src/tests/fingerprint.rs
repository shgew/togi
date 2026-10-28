//! What the source fingerprint of a snapshot covers: a build that differs only in test-only files has the same
//! fingerprint, so it accepts the snapshot; a production change has another one, so the build rejects the snapshot
//! and replays the journal.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use test_case::test_case;

#[path = "../../build/fingerprint.rs"]
mod inputs;

const ROOT: &str = "/workspace";

/// A source tree in memory: file paths and contents, with the directories they imply.
#[derive(Clone, Default)]
struct Tree(BTreeMap<PathBuf, String>);

impl Tree {
    fn write(&mut self, name: &str, contents: &str) {
        self.0
            .insert(Path::new(ROOT).join(name), contents.to_owned());
    }

    fn remove(&mut self, name: &str) {
        self.0.remove(&Path::new(ROOT).join(name));
    }
}

impl inputs::Tree for Tree {
    fn entries(&self, directory: &Path) -> io::Result<Vec<(PathBuf, bool)>> {
        let mut entries = BTreeMap::new();
        for path in self.0.keys() {
            let Ok(inside) = path.strip_prefix(directory) else {
                continue;
            };
            let Some(first) = inside.components().next() else {
                continue;
            };
            entries.insert(directory.join(first), inside.components().nth(1).is_some());
        }
        if entries.is_empty() {
            return Err(io::ErrorKind::NotFound.into());
        }
        Ok(entries.into_iter().collect())
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.0
            .get(path)
            .map(|contents| contents.clone().into_bytes())
            .ok_or_else(|| io::ErrorKind::NotFound.into())
    }
}

/// Two crates and the files around them, with test-only files of every kind the rule names.
fn workspace() -> Tree {
    let mut tree = Tree::default();
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
        tree.write(name, contents);
    }
    tree
}

fn fingerprint(tree: &Tree, revision: &str) -> String {
    let root = Path::new(ROOT);
    let files = inputs::files(
        tree,
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
    inputs::digest(tree, root, &files, revision).unwrap()
}

#[test_case("crates/a/src/tests.rs"; "tests.rs")]
#[test_case("crates/a/src/model_tests.rs"; "a _tests.rs file")]
#[test_case("crates/a/src/tests/case.rs"; "a tests directory")]
#[test_case("crates/a/src/tests/added.rs"; "a file added to a tests directory")]
#[test_case("crates/a/src/model/model_tests/case.rs"; "a nested _tests directory")]
#[test_case("crates/a/src/support.rs"; "a listed file")]
#[test_case("crates/a/src/fakes/clock.rs"; "a file in a listed directory")]
#[test_case("crates/a/tests/cli.rs"; "an integration test")]
fn test_only_edits_keep_the_fingerprint(edited: &str) {
    let mut tree = workspace();
    let before = fingerprint(&tree, "dev");
    tree.write(edited, "// edited\n");
    assert_eq!(fingerprint(&tree, "dev"), before);
}

#[test_case("crates/a/src/lib.rs"; "a production file")]
#[test_case("crates/a/src/model/mod.rs"; "a nested production file")]
#[test_case("crates/a/src/added.rs"; "a production file added")]
#[test_case("crates/b/src/lib.rs"; "another crate")]
#[test_case("crates/a/Cargo.toml"; "a manifest")]
#[test_case("crates/test-only.txt"; "the test-only list")]
#[test_case("Cargo.lock"; "the lock file")]
#[test_case("version.txt"; "the version")]
fn production_edits_change_the_fingerprint(edited: &str) {
    let mut tree = workspace();
    let before = fingerprint(&tree, "dev");
    tree.write(edited, "// edited\n");
    assert_ne!(fingerprint(&tree, "dev"), before);
}

#[test]
fn the_revision_changes_the_fingerprint() {
    let tree = workspace();
    assert_ne!(fingerprint(&tree, "dev"), fingerprint(&tree, "abc1234"));
}

#[test]
fn a_missing_test_only_list_is_an_error() {
    let mut tree = workspace();
    tree.remove(inputs::TEST_ONLY_LIST);
    assert!(inputs::files(&tree, Path::new(ROOT), &["a"], &[]).is_err());
}

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn real_fs_the_build_fingerprint_is_this_modules_digest_of_the_build_source() {
    let root = repository();
    // The sandbox includes every reducer's production source for build.rs, but not every crate's tests.
    let files = inputs::reducer_files(&inputs::Disk, &root).unwrap();
    assert_eq!(
        inputs::digest(
            &inputs::Disk,
            &root,
            &files,
            option_env!("TOGI_REV").unwrap_or("dev")
        )
        .unwrap(),
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
    let files = inputs::reducer_files(&inputs::Disk, &root).unwrap();
    assert_eq!(files.contains(&root.join(file)), covered);
}
