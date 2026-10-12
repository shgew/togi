use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;
use togi_watchtest::Scenario;

/// The built command over `state`, with no colour preference, no journal stream and UTC.
pub(crate) fn command(state: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_togi"));
    command
        .arg("--state-dir")
        .arg(state)
        .args(args)
        .env_remove("NO_COLOR")
        .env_remove("JOURNAL_STREAM")
        .env("TZ", "UTC");
    command
}

pub(crate) fn togi(state: &Path, args: &[&str]) -> Output {
    command(state, args).output().unwrap()
}

/// Which of the command's streams `JOURNAL_STREAM` names.
#[derive(Clone, Copy)]
pub(crate) enum Journald {
    Stdout,
    Stderr,
}

/// Runs the command with stdout and stderr as files and `JOURNAL_STREAM` naming `stream`, as
/// systemd starts a service whose output goes to the journal.
pub(crate) fn togi_in_journal(state: &Path, args: &[&str], stream: Journald) -> Output {
    use std::os::unix::fs::MetadataExt as _;
    use std::process::Stdio;

    let capture = tempfile::tempdir().unwrap();
    let (out_path, err_path) = (capture.path().join("out"), capture.path().join("err"));
    let (out, err) = (
        std::fs::File::create(&out_path).unwrap(),
        std::fs::File::create(&err_path).unwrap(),
    );
    let named = match stream {
        Journald::Stdout => out.metadata().unwrap(),
        Journald::Stderr => err.metadata().unwrap(),
    };
    let status = command(state, args)
        .env("JOURNAL_STREAM", format!("{}:{}", named.dev(), named.ino()))
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .status()
        .unwrap();
    Output {
        status,
        stdout: std::fs::read(out_path).unwrap(),
        stderr: std::fs::read(err_path).unwrap(),
    }
}

pub(crate) fn text(bytes: &[u8]) -> &str {
    std::str::from_utf8(bytes).unwrap()
}

pub(crate) fn session(scenario: Scenario) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    togi_watchtest::install(dir.path(), scenario).unwrap();
    dir
}

pub(crate) fn journal(dir: &Path) -> std::path::PathBuf {
    dir.join("events.jsonl")
}

pub(crate) fn lines(dir: &Path) -> Vec<String> {
    std::fs::read_to_string(journal(dir))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

pub(crate) fn rewrite_first_line(dir: &Path, change: impl FnOnce(&mut serde_json::Value)) {
    let mut lines = lines(dir);
    let mut first: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    change(&mut first);
    lines[0] = first.to_string();
    std::fs::write(journal(dir), lines.join("\n") + "\n").unwrap();
}

pub(crate) fn first_trial(dir: &Path) -> String {
    let ran = togi(dir, &["events", "--kind", "trial.intent", "--json"]);
    let first = text(&ran.stdout).lines().next().unwrap();
    let event: serde_json::Value = serde_json::from_str(first).unwrap();
    event["data"]["trial"].as_str().unwrap().to_owned()
}

pub(crate) fn append_event(dir: &Path, kind: &str, msg: &str, data: &serde_json::Value) {
    use std::io::Write as _;
    let last: serde_json::Value = serde_json::from_str(lines(dir).last().unwrap()).unwrap();
    let event = serde_json::json!({
        "seq": last["seq"].as_u64().unwrap() + 1,
        "time": "2026-02-01T00:00:00Z",
        "boot": last["boot"],
        "kind": kind,
        "msg": msg,
        "data": data,
    });
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(journal(dir))
        .unwrap();
    writeln!(file, "{event}").unwrap();
}

pub(crate) const BETWEEN_TRIAL_MCE: &str = "mce on cpu 3 \u{1b}[2J between trials (recorded only)";

pub(crate) fn append_between_trial_mce(dir: &Path) {
    append_event(
        dir,
        "mce",
        BETWEEN_TRIAL_MCE,
        &serde_json::json!({
            "cpu": 3, "bank": 1, "bank_type": "load_store", "corrected": true,
            "between_trials": true, "lines": [],
        }),
    );
}

/// The kind of the events the lenient tests plant; no build knows it.
pub(crate) const UNKNOWN_KIND: &str = "future.thing";

/// Appends an event of `kind`, which no build should know; returns its line number.
pub(crate) fn append_unknown_kind(dir: &Path, kind: &str) -> usize {
    let line = lines(dir).len() + 1;
    append_event(dir, kind, "x", &serde_json::json!({}));
    line
}

/// Puts an event of `kind`, which no build should know, before the journal's last event, which takes the next
/// seq: a hole of one seq in what a build that knows every kind reads. Returns the unknown event's line number.
pub(crate) fn insert_unknown_kind_before_last(dir: &Path, kind: &str) -> usize {
    let mut kept = lines(dir);
    let last = kept.pop().unwrap();
    std::fs::write(
        journal(dir),
        kept.iter()
            .map(|line| line.clone() + "\n")
            .collect::<String>(),
    )
    .unwrap();
    let line = append_unknown_kind(dir, kind);
    let mut moved: serde_json::Value = serde_json::from_str(&last).unwrap();
    moved["seq"] = (line as u64 + 1).into();
    append_line(dir, &moved.to_string());
    line
}

/// Appends a `session.warning` carrying a field no build knows; returns its line number.
pub(crate) fn append_unknown_field(dir: &Path, field: &str) -> usize {
    let line = lines(dir).len() + 1;
    append_event(
        dir,
        "session.warning",
        "warned",
        &serde_json::json!({"operation": "write state projection", "error": "disk full", field: 1}),
    );
    line
}

fn append_line(dir: &Path, line: &str) {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(journal(dir))
        .unwrap();
    writeln!(file, "{line}").unwrap();
}
