use std::fs;
use std::num::{NonZeroU32, NonZeroUsize};
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::time::Duration;

use togi_journal::Event;
use togi_sim::MachineDefinition;
use togi_tool_bench::{Options, Selection, Tree};

use crate::session;

/// Explicit real-process fixture: executable scripts and their disposable on-disk inputs.
pub(crate) struct ProcessBench {
    pub(crate) dir: tempfile::TempDir,
}

impl ProcessBench {
    pub(crate) fn new() -> Self {
        let bench = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        bench.journal(&session::session(&MachineDefinition::default(), 1, None, 1000).events);
        fs::write(
            bench.path("suite.json"),
            r#"{"scenarios":[{"name":"default","dev":[1,2,3],"holdout":[101]},{"name":"late","dev":[1]}]}"#,
        )
        .unwrap();
        bench
    }

    pub(crate) fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Sets the journal every simulator leaves, with the `state.json` a run publishes at its last event.
    pub(crate) fn journal(&self, events: &[Event]) {
        let lines: String = events
            .iter()
            .map(|event| serde_json::to_string(event).unwrap() + "\n")
            .collect();
        let folded = togi_session::ReadState::replay(events).unwrap();
        let prefix = togi_journal::snapshot::Prefix::of(
            lines.as_bytes(),
            events.last().map(|event| event.seq),
        );
        let state = togi_journal::snapshot::seal(prefix, &folded.document()).unwrap();
        fs::write(self.path("journal.jsonl"), lines).unwrap();
        fs::write(self.path("state.json"), state.to_string()).unwrap();
    }

    /// A simulator that records its arguments, copies the journal and its `state.json` into its state directory,
    /// prints `log` and exits with `exit`; `before` runs first.
    pub(crate) fn simulator(&self, name: &str, exit: i32, log: &str, before: &str) -> PathBuf {
        let path = self.path(name);
        let script = format!(
            r#"#!/bin/sh
{before}
echo "$@" >> '{args}'
dir=
while [ $# -gt 0 ]; do
  if [ "$1" = --state-dir ]; then dir="$2"; fi
  shift
done
cp '{journal}' "$dir/events.jsonl"
cp '{state}' "$dir/state.json"
echo '{log}'
exit {exit}
"#,
            args = self.path("args.txt").display(),
            journal = self.path("journal.jsonl").display(),
            state = self.path("state.json").display(),
        );
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    pub(crate) fn options(&self, simulator: PathBuf) -> Options {
        Options {
            suite: self.path("suite.json"),
            selection: Selection::All,
            out: None,
            baseline_out: None,
            baseline: None,
            keep: None,
            jobs: NonZeroUsize::new(2).unwrap(),
            timeout: Duration::from_secs(60),
            max_boots: NonZeroU32::new(7).unwrap(),
            cache: Some(self.path("cache")),
            simulator: Some(simulator),
            tree: Tree {
                commit: "abc1234".to_owned(),
                dirty: false,
            },
        }
    }
}
