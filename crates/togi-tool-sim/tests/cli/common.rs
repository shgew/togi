use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use togi_journal::{Event, Recording};

pub(crate) const BIN: &str = env!("CARGO_BIN_EXE_togi-tool-sim");

pub(crate) struct Ran {
    pub code: Option<i32>,
    pub stderr: String,
    pub stdout: String,
}

pub(crate) fn command() -> Command {
    let mut command = Command::new(BIN);
    command.env_remove("NO_COLOR").env_remove("JOURNAL_STREAM");
    command
}

pub(crate) fn ran(output: Output) -> Ran {
    Ran {
        code: output.status.code(),
        stderr: String::from_utf8(output.stderr).unwrap(),
        stdout: String::from_utf8(output.stdout).unwrap(),
    }
}

pub(crate) fn sim<S: AsRef<OsStr>>(args: impl IntoIterator<Item = S>) -> Ran {
    ran(command().args(args).output().unwrap())
}

pub(crate) fn sim_in_tmp<S: AsRef<OsStr>>(tmp: &Path, args: impl IntoIterator<Item = S>) -> Ran {
    ran(command().env("TMPDIR", tmp).args(args).output().unwrap())
}

pub(crate) fn machines() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/bench/machines")
}

pub(crate) fn live_events(dir: &Path) -> Vec<Event> {
    let recording = Recording::read(&dir.join("events.jsonl")).unwrap();
    assert!(
        recording.torn().is_none(),
        "torn journal in {}",
        dir.display()
    );
    recording.into_events()
}

pub(crate) fn entries(dir: &Path) -> Vec<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    entries
}
