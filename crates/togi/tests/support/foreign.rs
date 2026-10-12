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
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Where the refusal sends the operator.
pub(crate) const CUTOVER: &str = "docs/howto.md#go-to-rust-cutover";

fn source() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testdata/foreign-go")
}

/// Copies the Go build's state directory into `state`, which exists or is created.
pub(crate) fn install(state: &Path) {
    fn copy(from: &Path, to: &Path) {
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                fs::create_dir(&target).unwrap();
                copy(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), &target).unwrap();
            }
        }
    }
    fs::create_dir_all(state).unwrap();
    copy(&source(), state);
}

/// What a snapshot keeps of one entry: a directory's modification time, which moves when an entry in it is added,
/// removed or renamed, and a file's bytes and modification time.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    Dir { modified: SystemTime },
    File { bytes: Vec<u8>, modified: SystemTime },
}

/// Every file and directory under `root`, and `root` itself, by path relative to it.
pub(crate) type Snapshot = BTreeMap<String, Entry>;

pub(crate) fn snapshot(root: &Path) -> Snapshot {
    fn walk(root: &Path, at: &Path, seen: &mut Snapshot) {
        let metadata = fs::symlink_metadata(at).unwrap();
        let name = at.strip_prefix(root).unwrap().display().to_string();
        let modified = metadata.modified().unwrap();
        if metadata.is_dir() {
            seen.insert(name, Entry::Dir { modified });
            for entry in fs::read_dir(at).unwrap() {
                walk(root, &entry.unwrap().path(), seen);
            }
        } else {
            assert!(metadata.is_file(), "{}: neither file nor directory", at.display());
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
        assert!(stderr.contains(wanted), "{command}: no {wanted:?} in {stderr}");
    }
    let message = refusal_message(state);
    assert!(
        stderr.contains(&message),
        "{command}: not {message:?} in {stderr}"
    );
}

/// Appends to the installed journal a line a Rust build would refuse for another reason, to show it is never read.
pub(crate) fn append_unreadable_line(state: &Path) {
    use std::io::Write as _;
    let mut journal = fs::OpenOptions::new()
        .append(true)
        .open(state.join("events.jsonl"))
        .unwrap();
    journal
        .write_all(b"{\"seq\":13,\"kind\":\"future.thing\",\"data\":{}}\nnot json\n")
        .unwrap();
}

/// Cuts the installed journal down to its first line without the final newline: a Go journal torn after its first
/// line, which a Rust build must still refuse as another build's journal, not treat as a torn tail or an empty journal.
pub(crate) fn keep_only_the_unterminated_first_line(state: &Path) {
    let path = state.join("events.jsonl");
    let text = fs::read_to_string(&path).unwrap();
    let (first, _) = text.split_once('\n').unwrap();
    assert!(first.starts_with("{\"seq\":1,"), "{first}");
    fs::write(&path, first).unwrap();
}

/// Replaces the installed journal with the first half of the first line of a journal this build wrote, as a crash
/// during the very first write leaves it, with the final newline when `newline` and without it otherwise. The
/// installed journal is made in a scratch directory, so the state directory gains no lock or state file.
pub(crate) fn keep_only_a_truncated_rust_header(state: &Path, newline: bool) {
    let scratch = tempfile::tempdir().unwrap();
    togi_watchtest::install(scratch.path(), togi_watchtest::Scenario::Search).unwrap();
    let text = fs::read_to_string(scratch.path().join("events.jsonl")).unwrap();
    let (first, _) = text.split_once('\n').unwrap();
    assert!(first.contains("\"session.start\""), "{first}");
    let mut kept = first.as_bytes()[..first.len() / 2].to_vec();
    if newline {
        kept.push(b'\n');
    }
    fs::write(state.join("events.jsonl"), kept).unwrap();
}
