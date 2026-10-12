#![allow(dead_code, reason = "each test crate uses part of this shared module")]

use std::fs;
use std::num::{NonZeroU32, NonZeroUsize};
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::time::Duration;

use togi_sim::MachineDefinition;
use togi_tool_bench::{Options, Selection, Tree};

use crate::session;

pub(crate) struct Bench {
    pub(crate) dir: tempfile::TempDir,
}

impl Bench {
    pub(crate) fn new() -> Self {
        let bench = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        let journal = session::session(&MachineDefinition::default(), 1, None, 2).events;
        let lines: Vec<String> = journal
            .iter()
            .map(|event| serde_json::to_string(event).unwrap())
            .collect();
        let lines = lines.join("\n") + "\n";
        let mut last = serde_json::to_value(journal.last().unwrap()).unwrap();
        last["cause"] = serde_json::json!([last["seq"].clone()]);
        last["seq"] = (last["seq"].as_u64().unwrap() + 1).into();
        last["kind"] = "shutdown".into();
        last["msg"] = "stopped by signal".into();
        last["data"] = serde_json::json!({"reason": "signal"});
        fs::write(bench.path("signalled.jsonl"), format!("{lines}{last}\n")).unwrap();
        fs::write(bench.path("journal.jsonl"), lines).unwrap();
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

    /// A simulator that records its arguments, copies the journal into its state directory, prints `log` and
    /// exits with `exit`; `before` runs first.
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
echo '{log}'
exit {exit}
"#,
            args = self.path("args.txt").display(),
            journal = self.path("journal.jsonl").display(),
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
