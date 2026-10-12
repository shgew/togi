use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;
use togi_watchtest::Scenario;

pub(crate) fn togi(state: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_togi"))
        .arg("--state-dir")
        .arg(state)
        .args(args)
        .env_remove("NO_COLOR")
        .env_remove("JOURNAL_STREAM")
        .env("TZ", "UTC")
        .output()
        .unwrap()
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
