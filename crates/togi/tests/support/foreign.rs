//! A state directory as the Go build left it, and a byte-exact record of a directory to prove a refusal changed
//! nothing in it.
//!
//! `tests/testdata/foreign-go` is the Go build's state directory:
//!
//! - `events.jsonl` is `internal/journal/testdata/20260927T221954Z-config.jsonl`, the Go journal of a session that
//!   only started and loaded its configuration, line for line (flat events, no `data` or `build` object). The
//!   only change is that its eleven boot ids, which identify the machine, are replaced by the synthetic
//!   `00000000-0000-4000-8000-00000000000N`, the same way each time.
//! - `archive/20260926T151414Z.jsonl` is `internal/journal/testdata/20260926T151414Z-config.jsonl` with its boot
//!   ids replaced the same way.
//! - `state.json` is shaped by hand from `internal/journal/state.go` (`State`, `SessionInfo`, `CoreState`,
//!   `BIOSContext`) with synthetic values. A Rust build never reads it.
//! - `.events.jsonl.tmp`, `trials/20260927T221954Z-0001.log`, `archive/20260926T151414Z-trials/artifact` and
//!   `archive/20260926T151414Z-compat-pending` are hand-made, empty or synthetic stand-ins for the files the Go
//!   journal keeps next to its journal. Their names and places follow `eventsTmpFile`, `trialsDir`, `archiveDir`,
//!   `trialsSuffix` and the `-compat-pending` archive marker in `internal/journal/journal.go`; their contents are
//!   not Go output. There is no `lock` file: a refusal must not create one.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use togi_journal::fs::{FileKind, Fs};
/// Where the refusal sends the operator.
pub(crate) const CUTOVER: &str = "docs/howto.md#go-to-rust-cutover";

const FILES: &[(&str, &[u8])] = &[
    (
        "events.jsonl",
        include_bytes!("../testdata/foreign-go/events.jsonl"),
    ),
    (
        "state.json",
        include_bytes!("../testdata/foreign-go/state.json"),
    ),
    (
        ".events.jsonl.tmp",
        include_bytes!("../testdata/foreign-go/.events.jsonl.tmp"),
    ),
    (
        "archive/20260926T151414Z.jsonl",
        include_bytes!("../testdata/foreign-go/archive/20260926T151414Z.jsonl"),
    ),
    (
        "archive/20260926T151414Z-compat-pending",
        include_bytes!("../testdata/foreign-go/archive/20260926T151414Z-compat-pending"),
    ),
    (
        "archive/20260926T151414Z-trials/artifact",
        include_bytes!("../testdata/foreign-go/archive/20260926T151414Z-trials/artifact"),
    ),
    (
        "trials/20260927T221954Z-0001.log",
        include_bytes!("../testdata/foreign-go/trials/20260927T221954Z-0001.log"),
    ),
];

fn create_dirs(fs: &dyn Fs, path: &Path) {
    if fs.kind(path).unwrap().is_some() {
        return;
    }
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        create_dirs(fs, parent);
    }
    fs.create_dir(path).unwrap();
}

/// Installs the compiled Go fixture in the caller's in-memory filesystem.
pub(crate) fn install(fs: &dyn Fs, state: &Path) {
    create_dirs(fs, state);
    for (name, bytes) in FILES {
        let target = state.join(name);
        create_dirs(fs, target.parent().unwrap());
        fs.create_truncate(&target)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }
}

/// Disk fixture only for a genuine interactive subprocess test; never syncs.
pub(crate) fn install_real_fs(state: &Path) {
    for (name, bytes) in FILES {
        let target = state.join(name);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, bytes).unwrap();
    }
}

/// What a snapshot keeps of one entry: a directory's modification time, which moves when an entry in it is added,
/// removed or renamed, and a file's bytes and modification time.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    Dir {
        modified: Option<SystemTime>,
    },
    File {
        bytes: Vec<u8>,
        modified: Option<SystemTime>,
    },
}

/// Every file and directory under `root`, and `root` itself, by path relative to it.
pub(crate) type Snapshot = BTreeMap<String, Entry>;

pub(crate) fn snapshot_real_fs(root: &Path) -> Snapshot {
    fn walk(root: &Path, at: &Path, seen: &mut Snapshot) {
        let metadata = fs::symlink_metadata(at).unwrap();
        let name = at.strip_prefix(root).unwrap().display().to_string();
        let modified = Some(metadata.modified().unwrap());
        if metadata.is_dir() {
            seen.insert(name, Entry::Dir { modified });
            for entry in fs::read_dir(at).unwrap() {
                walk(root, &entry.unwrap().path(), seen);
            }
        } else {
            assert!(
                metadata.is_file(),
                "{}: neither file nor directory",
                at.display()
            );
            let bytes = fs::read(at).unwrap();
            seen.insert(name, Entry::File { bytes, modified });
        }
    }
    let mut seen = Snapshot::new();
    walk(root, root, &mut seen);
    seen
}

/// What a refusal says of `state`'s journal: the first line is not a complete header of this build, which is either
/// another build's journal or a header torn by a crash, so it does not claim to know which.
pub(crate) fn refusal_message(state: &Path) -> String {
    format!(
        "{}: first line is not a complete header of this build; either another build's journal or a header torn by a crash; see {CUTOVER}",
        state.join("events.jsonl").display()
    )
}

/// `stderr` of `command` is `togi <command>: ` and exactly [`refusal_message`], which names `state`'s `events.jsonl`,
/// another build's journal and the cutover step in `docs/howto.md`.
pub(crate) fn assert_refusal(command: &str, stderr: &str, state: &Path) {
    let journal = state.join("events.jsonl").display().to_string();
    assert!(
        stderr.starts_with(&format!("togi {command}: ")),
        "{command}: {stderr}"
    );
    for wanted in [journal.as_str(), CUTOVER, "another build's journal"] {
        assert!(
            stderr.contains(wanted),
            "{command}: no {wanted:?} in {stderr}"
        );
    }
    let message = refusal_message(state);
    assert!(
        stderr.contains(&message),
        "{command}: not {message:?} in {stderr}"
    );
}

/// Every directory and regular file, without touching the host filesystem.
pub(crate) fn snapshot(fs: &dyn Fs, root: &Path) -> Snapshot {
    fn walk(fs: &dyn Fs, root: &Path, at: &Path, seen: &mut Snapshot) {
        let name = at.strip_prefix(root).unwrap().display().to_string();
        match fs.kind(at).unwrap().unwrap() {
            FileKind::Dir => {
                seen.insert(name, Entry::Dir { modified: None });
                for entry in fs.read_dir(at).unwrap() {
                    walk(fs, root, &at.join(entry.name), seen);
                }
            }
            FileKind::File => {
                seen.insert(
                    name,
                    Entry::File {
                        bytes: fs.read(at).unwrap(),
                        modified: None,
                    },
                );
            }
            kind @ (FileKind::Symlink | FileKind::Other) => {
                panic!("{}: unexpected {kind:?}", at.display())
            }
        }
    }
    let mut seen = Snapshot::new();
    walk(fs, root, root, &mut seen);
    seen
}

/// Plants unreadable later lines to prove refusal uses only the first header.
pub(crate) fn append_unreadable_line(fs: &dyn Fs, state: &Path) {
    fs.open_append(&state.join("events.jsonl"))
        .unwrap()
        .write_all(b"{\"seq\":13,\"kind\":\"future.thing\",\"data\":{}}\nnot json\n")
        .unwrap();
}

pub(crate) fn keep_only_the_unterminated_first_line(fs: &dyn Fs, state: &Path) {
    let path = state.join("events.jsonl");
    let text = String::from_utf8(fs.read(&path).unwrap()).unwrap();
    let (first, _) = text.split_once('\n').unwrap();
    assert!(first.starts_with("{\"seq\":1,"), "{first}");
    fs.create_truncate(&path)
        .unwrap()
        .write_all(first.as_bytes())
        .unwrap();
}

pub(crate) fn keep_only_a_truncated_journal_header(fs: &dyn Fs, state: &Path, newline: bool) {
    let scratch = std::sync::Arc::new(togi_journal::fs::MemFs::new());
    let path = Path::new("/header");
    togi_watchtest::install_with_fs(scratch.clone(), path, togi_watchtest::Scenario::Search)
        .unwrap();
    let bytes = scratch.read(&path.join("events.jsonl")).unwrap();
    let first = bytes.split(|byte| *byte == b'\n').next().unwrap();
    assert!(
        std::str::from_utf8(first)
            .unwrap()
            .contains("\"session.start\"")
    );
    let mut kept = first[..first.len() / 2].to_vec();
    if newline {
        kept.push(b'\n');
    }
    create_dirs(fs, state);
    fs.create_truncate(&state.join("events.jsonl"))
        .unwrap()
        .write_all(&kept)
        .unwrap();
}
