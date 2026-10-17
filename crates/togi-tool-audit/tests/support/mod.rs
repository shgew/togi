use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::io::Write as _;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Output, Stdio};

use serde_json::Value;
use togi_journal::fs::MemFs;
use togi_journal::{Event, StateDir};
use togi_tool_audit::{RecordError, audit_directory_with, read_records, read_records_from};

use crate::audit_command::{Io, entry};
use crate::fixtures::disk_inspect;

const CHILD_ARGS: &str = "TOGI_AUDIT_TEST_ARGS";
const OUTPUT_START: &[u8] = b"\0audit-command-output\0";

fn code(code: ExitCode) -> i32 {
    if code == ExitCode::SUCCESS {
        0
    } else if code == ExitCode::from(2) {
        2
    } else {
        1
    }
}

/// Reenters the same selected process test with explicit unsynced dependencies, never a production override.
pub(crate) fn child() {
    let Some(args) = std::env::var_os(CHILD_ARGS) else {
        return;
    };
    let args: Vec<Vec<u8>> = serde_json::from_str(args.to_str().unwrap()).unwrap();
    let args: Vec<OsString> = args.into_iter().map(OsString::from_vec).collect();
    let mut out = std::io::stdout();
    out.write_all(OUTPUT_START).unwrap();
    out.flush().unwrap();
    let result = entry(
        args,
        &mut Io {
            inspect: &disk_inspect,
            records: &read_records,
            out: &mut out,
            err: &mut std::io::stderr(),
        },
    );
    std::process::exit(code(result));
}

/// The real command protocol, executed by a child arm of the current test rather than the durable binary.
pub(crate) struct AuditCommand {
    command: Command,
    args: Vec<OsString>,
}

impl AuditCommand {
    pub(crate) fn new(program: impl AsRef<OsStr>) -> Self {
        let mut command = Command::new(program);
        command.args([
            "--exact",
            std::thread::current().name().unwrap(),
            "--nocapture",
        ]);
        Self {
            command,
            args: vec!["togi-tool-audit".into()],
        }
    }

    pub(crate) fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.args.push(arg.as_ref().to_owned());
        self
    }

    pub(crate) fn args(&mut self, args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> &mut Self {
        for arg in args {
            self.arg(arg);
        }
        self
    }

    pub(crate) fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
        self.command.env(key, value);
        self
    }

    pub(crate) fn current_dir(&mut self, path: impl AsRef<Path>) -> &mut Self {
        self.command.current_dir(path);
        self
    }

    pub(crate) fn stdout(&mut self, stdio: Stdio) -> &mut Self {
        self.command.stdout(stdio);
        self
    }

    pub(crate) fn stderr(&mut self, stdio: Stdio) -> &mut Self {
        self.command.stderr(stdio);
        self
    }

    fn prepare(&mut self) {
        let args: Vec<&[u8]> = self.args.iter().map(|arg| arg.as_bytes()).collect();
        self.command
            .env(CHILD_ARGS, serde_json::to_string(&args).unwrap());
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "real_process fixtures launch the current test's unsynced command-entry child arm"
    )]
    pub(crate) fn spawn(&mut self) -> std::io::Result<Child> {
        self.prepare();
        self.command.spawn()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "real_process fixtures capture the current test's unsynced command-entry child arm"
    )]
    pub(crate) fn output(&mut self) -> std::io::Result<Output> {
        self.prepare();
        let mut output = self.command.output()?;
        if let Some(start) = output
            .stdout
            .windows(OUTPUT_START.len())
            .position(|bytes| bytes == OUTPUT_START)
        {
            output.stdout.drain(..start + OUTPUT_START.len());
        }
        Ok(output)
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

#[derive(Default)]
pub(crate) struct Memory {
    dirs: BTreeMap<PathBuf, MemFs>,
    pub(crate) records: BTreeMap<PathBuf, Vec<u8>>,
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
        let inspect = |path: &Path, simulated| {
            audit_directory_with(path, simulated, |path| {
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
        let records = |path: &Path| {
            let bytes = self
                .records
                .get(&lexical(path))
                .ok_or_else(|| RecordError::Read {
                    path: path.to_path_buf(),
                    source: std::io::Error::from(std::io::ErrorKind::NotFound),
                })?;
            read_records_from(path, bytes.as_slice())
        };
        let args = std::iter::once(OsString::from("togi-tool-audit"))
            .chain(args.into_iter().map(|arg| arg.as_ref().to_owned()));
        let result = entry(
            args,
            &mut Io {
                inspect: &inspect,
                records: &records,
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
