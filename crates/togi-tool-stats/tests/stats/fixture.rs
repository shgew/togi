use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use togi_config::Config;
use togi_journal::{Event, FixStamp, SCHEMA};
use togi_tuner::{EVIDENCE_EPOCH, RULESET};

fn build() -> Value {
    json!({
        "version": "0.0.0",
        "ruleset": RULESET,
        "schema": SCHEMA,
        "fixes": FixStamp::none(),
        "evidence": EVIDENCE_EPOCH,
    })
}

fn config_loaded(seq: u64, boot: &str) -> Event {
    event(
        seq,
        boot,
        "config.loaded",
        json!({
            "build": build(),
            "config": serde_json::to_value(Config::default().snapshot()).unwrap(),
            "path": "/etc/togi/config.json",
            "file": false,
        }),
    )
}

fn event(seq: u64, boot: &str, kind: &str, data: Value) -> Event {
    let mut record = json!({
        "seq": seq,
        "time": format!("2026-01-01T00:{:02}:{:02}Z", seq / 60, seq % 60),
        "boot": boot,
        "kind": kind,
        "msg": kind,
    });
    record["data"] = data;
    Event::decode(&record.to_string()).unwrap()
}

fn intent(seq: u64, boot: &str, trial: &str, cores: &[u8], offset: i64) -> Event {
    let profile: Value = (0..4_u8)
        .map(|core| {
            let offset = if cores.contains(&core) { offset } else { 0 };
            (core.to_string(), json!(offset))
        })
        .collect::<serde_json::Map<_, _>>()
        .into();
    event(
        seq,
        boot,
        "trial.intent",
        json!({
            "trial": trial,
            "profile": profile,
            "cores": cores,
            "regime": "R2",
            "workload": "mprime-avx2-36k-248k",
            "duration_s": 90,
            "condition": "together",
        }),
    )
}

fn start(seq: u64, boot: &str, trial: &str) -> Event {
    event(
        seq,
        boot,
        "trial.start",
        json!({"trial": trial, "scope": "togi-trial", "pid": 100, "cpus": [0], "argv": ["mprime", "-t"]}),
    )
}

fn pass(seq: u64, boot: &str, trial: &str) -> Event {
    event(
        seq,
        boot,
        "trial.end",
        json!({
            "trial": trial,
            "outcome": "pass",
            "duration_s": 90,
            "tctl_max_c": 62,
            "voltage_requests_v": {"3": 1.1},
        }),
    )
}

/// A static journal of a four-core machine: one passing trial, a joint trial of cores 1 and 3 that ends in a
/// crash and a reboot, and a passing trial of the next boot.
pub(crate) fn crashed_session() -> Vec<Event> {
    vec![
        event(
            1,
            "a",
            "session.start",
            json!({
                "build": build(),
                "session": "20260101T000000Z",
                "cores": [
                    {"core": 0, "ccd": 0, "cpus": [0]},
                    {"core": 1, "ccd": 0, "cpus": [1]},
                    {"core": 2, "ccd": 1, "cpus": [2]},
                    {"core": 3, "ccd": 1, "cpus": [3]},
                ],
            }),
        ),
        config_loaded(2, "a"),
        intent(3, "a", "0001", &[0, 1, 2, 3], -20),
        start(4, "a", "0001"),
        pass(5, "a", "0001"),
        intent(6, "a", "0002", &[1, 3], -22),
        start(7, "a", "0002"),
        event(
            8,
            "b",
            "crash.detected",
            json!({
                "previous_boot": "a",
                "in_flight": 6,
                "stray": false,
                "condition": "together",
                "reset_reason": "watchdog",
                "reset_reason_raw": ["watchdog"],
                "inconclusive": false,
            }),
        ),
        config_loaded(9, "b"),
        intent(10, "b", "0003", &[3], -20),
        start(11, "b", "0003"),
        pass(12, "b", "0003"),
        event(13, "b", "shutdown", json!({"reason": "command"})),
    ]
}

/// `events.jsonl` of `events` in `dir`, one encoded event a line.
pub(crate) fn write_journal(dir: &Path, events: &[Event]) {
    let mut text = String::new();
    for event in events {
        text.push_str(&event.encode().unwrap());
        text.push('\n');
    }
    fs::write(dir.join("events.jsonl"), text).unwrap();
}
