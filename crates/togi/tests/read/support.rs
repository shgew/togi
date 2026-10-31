use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Output};
use std::sync::Arc;

use togi_journal::Event;
use togi_journal::fs::{Fs, MemFs};
use togi_watchtest::Scenario;

use super::host::Host;

/// An isolated in-memory state directory and the filesystem the command reads.
pub(crate) struct Session {
    pub(crate) fs: Arc<MemFs>,
    path: PathBuf,
}

impl Session {
    pub(crate) fn empty() -> Self {
        let fs = Arc::new(MemFs::new());
        let path = PathBuf::from("/state");
        fs.create_dir(&path).unwrap();
        Self { fs, path }
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn host(&self) -> Host {
        Host::with_fs(self.fs.clone())
    }
}

pub(crate) fn output(ran: super::host::Ran) -> Output {
    Output {
        status: ExitStatus::from_raw(i32::from(ran.exit.code()) << 8),
        stdout: ran.out.into_bytes(),
        stderr: ran.err.into_bytes(),
    }
}

pub(crate) fn togi(state: &Session, args: &[&str]) -> Output {
    output(state.host().run(state.path(), args))
}

/// Which of the command's streams systemd directs to its journal.
#[derive(Clone, Copy)]
pub(crate) enum Journald {
    Stdout,
    Stderr,
}

/// Capture both streams in memory; journal identity is an injected host fact.
pub(crate) fn togi_in_journal(state: &Session, args: &[&str], stream: Journald) -> Output {
    let mut host = state.host();
    match stream {
        Journald::Stdout => {
            host.stdout_facts = Some(togi_render::OutputFacts {
                journald: true,
                ..togi_render::OutputFacts::default()
            });
        }
        Journald::Stderr => {
            host.stdout_facts = Some(togi_render::OutputFacts::default());
            host.facts.journald = true;
        }
    }
    output(host.run(state.path(), args))
}

pub(crate) fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap()
}

pub(crate) fn session(scenario: Scenario) -> Session {
    installed(&scenario.events().unwrap())
}

pub(crate) fn installed(events: &[Event]) -> Session {
    let state = Session::empty();
    togi_watchtest::install_events_with_fs(state.fs.clone(), state.path(), events).unwrap();
    state
}

pub(crate) fn journal(state: &Session) -> PathBuf {
    state.path().join("events.jsonl")
}

pub(crate) fn write_journal(state: &Session, bytes: impl AsRef<[u8]>) {
    state.fs.put(journal(state), bytes.as_ref().to_vec());
}

pub(crate) fn lines(state: &Session) -> Vec<String> {
    String::from_utf8(state.fs.read(&journal(state)).unwrap())
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

pub(crate) fn rewrite_first_line(state: &Session, change: impl FnOnce(&mut serde_json::Value)) {
    let mut lines = lines(state);
    let mut first: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    change(&mut first);
    lines[0] = first.to_string();
    write_journal(state, lines.join("\n") + "\n");
}

pub(crate) fn first_trial(state: &Session) -> String {
    let ran = togi(state, &["events", "--kind", "trial.intent", "--json"]);
    let first = text(&ran.stdout).lines().next().unwrap();
    let event: serde_json::Value = serde_json::from_str(first).unwrap();
    event["data"]["trial"].as_str().unwrap().to_owned()
}

pub(crate) fn append_event(state: &Session, kind: &str, msg: &str, data: &serde_json::Value) {
    let last: serde_json::Value = serde_json::from_str(lines(state).last().unwrap()).unwrap();
    let event = serde_json::json!({
        "seq": last["seq"].as_u64().unwrap() + 1,
        "time": "2026-02-01T00:00:00Z",
        "boot": last["boot"],
        "kind": kind,
        "msg": msg,
        "data": data,
    });
    append_bytes(state, format!("{event}\n").as_bytes());
}

pub(crate) fn append_bytes(state: &Session, bytes: &[u8]) {
    state
        .fs
        .open_append(&journal(state))
        .unwrap()
        .write_all(bytes)
        .unwrap();
}

pub(crate) const BETWEEN_TRIAL_MCE: &str = "mce on cpu 3 \u{1b}[2J between trials (recorded only)";

pub(crate) fn append_between_trial_mce(state: &Session) {
    append_event(
        state,
        "mce",
        BETWEEN_TRIAL_MCE,
        &serde_json::json!({
            "cpu": 3, "bank": 1, "bank_type": "load_store", "corrected": true,
            "between_trials": true, "lines": [],
        }),
    );
}

pub(crate) const UNKNOWN_KIND: &str = "future.thing";

pub(crate) fn append_unknown_kind(state: &Session, kind: &str) -> usize {
    let line = lines(state).len() + 1;
    append_event(state, kind, "x", &serde_json::json!({}));
    line
}

pub(crate) fn insert_unknown_kind_before_last(state: &Session, kind: &str) -> usize {
    let mut kept = lines(state);
    let last = kept.pop().unwrap();
    write_journal(
        state,
        kept.iter()
            .map(|line| line.clone() + "\n")
            .collect::<String>(),
    );
    let line = append_unknown_kind(state, kind);
    let mut moved: serde_json::Value = serde_json::from_str(&last).unwrap();
    moved["seq"] = (line as u64 + 1).into();
    append_bytes(state, format!("{moved}\n").as_bytes());
    line
}

pub(crate) fn append_unknown_field(state: &Session, field: &str) -> usize {
    let line = lines(state).len() + 1;
    append_event(
        state,
        "session.warning",
        "warned",
        &serde_json::json!({"operation": "write state projection", "error": "disk full", field: 1}),
    );
    line
}

/// Compare against compile-time snapshot content, without insta's runtime file reads.
pub(crate) fn snapshot(name: &str, actual: &str) {
    macro_rules! snapshots {
        ($($name:literal),+ $(,)?) => {
            match name {
                $($name => include_str!(concat!("snapshots/read__", $name, ".snap")),)+
                _ => panic!("unknown compiled snapshot {name}"),
            }
        };
    }
    let expected = snapshots!(
        "events__events_help",
        "events__events_group",
        "events__events_kinds",
        "events__events_trial",
        "events__events_core",
        "status__status_help",
        "status__status_between",
        "status__status_nonzero",
        "status__status_crashed",
        "status__status_day-step",
        "status__status_filler",
        "status__status_mapping",
        "status__status_qualification",
        "status__status_reproduction",
        "status__status_deadend",
        "status__status_idle",
        "status__status_recovering",
        "status__status_stopped",
        "status__evidence_nonzero",
        "status__evidence_mapping",
        "status__evidence_day-step",
        "status__status_morning",
        "status__evidence_morning",
        "status__paused_ruleset14",
        "watch__paused_ruleset14",
        "watch__watch_help",
    );
    let (_, body) = expected
        .strip_prefix("---\n")
        .unwrap()
        .split_once("\n---\n")
        .unwrap();
    assert_eq!(
        actual.trim_end_matches('\n'),
        body.trim_end_matches('\n'),
        "{name}"
    );
}
