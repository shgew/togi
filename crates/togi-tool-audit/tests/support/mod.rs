use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Component, Path, PathBuf};
use std::process::{ExitCode, ExitStatus, Output};

use serde_json::Value;
use togi_journal::fs::MemFs;
use togi_journal::{Event, StateDir};
use togi_tool_audit::{Completion, audit_directory_with};

use crate::audit_command::{Io, entry};

fn code(code: ExitCode) -> i32 {
    if code == ExitCode::SUCCESS {
        0
    } else if code == ExitCode::from(2) {
        2
    } else {
        1
    }
}

fn lexical(path: &Path) -> PathBuf {
    assert!(
        path.is_absolute(),
        "memory command fixtures use absolute paths"
    );
    let mut resolved = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other @ (Component::Prefix(_) | Component::RootDir | Component::Normal(_)) => {
                resolved.push(other);
            }
        }
    }
    resolved
}

/// State directories in memory, audited through the command's entry as the binary audits them on disk.
#[derive(Default)]
pub(crate) struct Memory {
    dirs: BTreeMap<PathBuf, MemFs>,
}

impl Memory {
    pub(crate) fn state(&mut self, path: &Path, events: &[Event]) {
        let fs = self.dirs.entry(lexical(path)).or_default();
        let journal: String = events
            .iter()
            .map(|event| event.encode().unwrap() + "\n")
            .collect();
        fs.put("/state/events.jsonl", journal.into_bytes());
        fs.put(
            "/state/state.json",
            serde_json::to_vec(&serde_json::json!({
                "facts": togi_journal::State::replay(events).unwrap(),
                "tuner": togi_tuner::replay(events).unwrap().project(),
            }))
            .unwrap(),
        );
    }

    pub(crate) fn journal(&mut self, path: &Path, lines: &[Value]) {
        let fs = self.dirs.entry(lexical(path)).or_default();
        let mut journal = String::new();
        for line in lines {
            writeln!(journal, "{line}").unwrap();
        }
        fs.put("/state/events.jsonl", journal.into_bytes());
    }

    pub(crate) fn run(&self, args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> Output {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let inspect = |path: &Path, completion: Completion| {
            audit_directory_with(path, completion, |path| {
                let fs = self
                    .dirs
                    .get(&lexical(path))
                    .ok_or(togi_tool_audit::OpenError::NotADirectory)?;
                #[expect(
                    clippy::disallowed_methods,
                    reason = "ordinary command content fixtures open only the injected in-memory journal"
                )]
                let state = StateDir::simulated(fs.clone());
                Ok(state?)
            })
        };
        let args = std::iter::once(OsString::from("togi-tool-audit"))
            .chain(args.into_iter().map(|arg| arg.as_ref().to_owned()));
        let result = entry(
            args,
            &mut Io {
                inspect: &inspect,
                out: &mut out,
                err: &mut err,
            },
        );
        Output {
            status: ExitStatus::from_raw(code(result) << 8),
            stdout: out,
            stderr: err,
        }
    }
}
