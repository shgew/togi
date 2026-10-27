//! The source fingerprint a snapshot is sealed with: which files it covers and how they are hashed.
//!
//! `build.rs` computes it into `TOGI_SNAPSHOT_BUILD`, and the crate's tests include this file to pin both. It
//! covers production source only. A module that compiles only under `cfg(test)` is left out, by name (`tests.rs`,
//! `*_tests.rs`, a `tests` or `*_tests` directory under a `src`) or by an entry in `crates/test-only.txt`, so a test
//! edit leaves the fingerprint and the build alone. `nix/sources.nix` applies the same rule to the sources it hands
//! to each derivation.

use std::error::Error;
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

/// The crates whose source the reducers and their dependencies span.
const REDUCER_CRATES: [&str; 9] = [
    "togi-machine",
    "togi-journal",
    "togi-config",
    "togi-requests",
    "togi-tuner",
    "togi-defect",
    "togi-render",
    "togi-watch",
    "togi-session",
];

/// The files outside those crates' `src` the fingerprint covers.
const BUILD_FILES: [&str; 6] = [
    "Cargo.toml",
    "Cargo.lock",
    "rust-toolchain.toml",
    "crates/togi-journal/build.rs",
    "crates/togi-journal/build/fingerprint.rs",
    "version.txt",
];

/// The files this build's fingerprint hashes.
pub(crate) fn reducer_files(root: &Path) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    files(root, &REDUCER_CRATES, &BUILD_FILES)
}

/// The list of test-only modules no name marks, relative to the repository root.
pub(crate) const TEST_ONLY_LIST: &str = "crates/test-only.txt";

fn text(component: Component<'_>) -> &str {
    component.as_os_str().to_str().unwrap_or_default()
}

/// Whether `relative`, a path under `crates/`, is a test-only file or directory.
fn test_only(listed: &[PathBuf], relative: &Path, is_directory: bool) -> bool {
    if listed.iter().any(|entry| relative.starts_with(entry)) {
        return true;
    }
    let Some(name) = relative.components().next_back().map(text) else {
        return false;
    };
    if is_directory {
        name == "tests" || name.ends_with("_tests")
    } else {
        name == "tests.rs" || name.ends_with("_tests.rs")
    }
}

fn listed(root: &Path) -> Result<Vec<PathBuf>, std::io::Error> {
    Ok(fs::read_to_string(root.join(TEST_ONLY_LIST))?
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(PathBuf::from)
        .collect())
}

fn production_sources(
    crates: &Path,
    directory: &Path,
    listed: &[PathBuf],
    files: &mut Vec<PathBuf>,
) -> Result<(), std::io::Error> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        let is_directory = path.is_dir();
        if path
            .strip_prefix(crates)
            .is_ok_and(|relative| test_only(listed, relative, is_directory))
        {
            continue;
        }
        if is_directory {
            production_sources(crates, &path, listed, files)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    Ok(())
}

/// The files the fingerprint hashes, in hash order: the `src` tree of every crate in `crates` without its
/// test-only files, their manifests, the test-only list and `extra`, paths relative to `root`.
pub(crate) fn files(
    root: &Path,
    crates: &[&str],
    extra: &[&str],
) -> Result<Vec<PathBuf>, Box<dyn Error>> {
    let listed = listed(root)?;
    let crates_dir = root.join("crates");
    let mut files = Vec::new();
    for name in crates {
        let directory = crates_dir.join(name);
        production_sources(&crates_dir, &directory.join("src"), &listed, &mut files)?;
        files.push(directory.join("Cargo.toml"));
    }
    files.push(root.join(TEST_ONLY_LIST));
    files.extend(extra.iter().map(|name| root.join(name)));
    files.sort();
    Ok(files)
}

/// The hex SHA-256 of `files` and `rev`, each file under its name relative to `root`.
pub(crate) fn digest(
    root: &Path,
    files: &[PathBuf],
    rev: &str,
) -> Result<String, Box<dyn Error>> {
    let mut digest = Sha256::new();
    for path in files {
        let name = path.strip_prefix(root)?.to_string_lossy();
        let bytes = fs::read(path)?;
        digest.update((name.len() as u64).to_le_bytes());
        digest.update(name.as_bytes());
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    digest.update(rev.as_bytes());
    let mut identity = String::with_capacity(64);
    for byte in digest.finalize() {
        write!(identity, "{byte:02x}")?;
    }
    Ok(identity)
}
