//! The audit command over state directories held in memory.
#![expect(
    clippy::unwrap_used,
    clippy::needless_pass_by_value,
    reason = "the tests panic on a broken fixture and share fixtures that take JSON literals by value"
)]

#[expect(dead_code, reason = "each test crate uses part of the shared fixtures")]
#[path = "audit/fixtures.rs"]
mod fixtures;

#[expect(
    dead_code,
    reason = "the durable binary main is not the injected test entry"
)]
#[path = "../src/main.rs"]
mod audit_command;
mod support;

use std::path::Path;

use fixtures::{applied, header, phase, stop};
use serde_json::{Value, json};
use support::Memory;
use togi_journal::Event;

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn dirty() -> Vec<Event> {
    vec![header(), phase(2, 0, Some(-30)), applied(3, [-30, 0])]
}

/// Compares a compiled snapshot's body without a runtime snapshot filesystem adapter.
fn assert_snapshot(actual: &str, expected: &str) {
    let (_, body) = expected.split_once("\n---\n").unwrap();
    assert_eq!(actual.trim_end(), body.trim_end());
}

/// What a finding says without the directory path: the member values that identify it.
fn brief(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(|line| {
            let finding: serde_json::Value = serde_json::from_str(line).unwrap();
            let member = |name: &str| finding.get(name).map_or("-".into(), ToString::to_string);
            format!(
                "journal={} seq={} {}: {}",
                member("journal"),
                member("seq"),
                member("check"),
                member("reason")
            )
        })
        .collect()
}

#[test]
fn help_goes_to_stdout() {
    let help = Memory::default().run(["--help"]);
    assert!(help.status.success());
    assert!(text(&help.stdout).contains("STATE-DIR"));
}

#[test]
fn no_directory_exits_two() {
    let none: [&str; 0] = [];
    assert_eq!(Memory::default().run(none).status.code(), Some(2));
}

#[test]
fn a_removed_sweep_flag_exits_two() {
    assert_eq!(
        Memory::default().run(["--seeds", "2"]).status.code(),
        Some(2)
    );
}

#[test]
fn an_unknown_flag_exits_two() {
    assert_eq!(Memory::default().run(["--bogus"]).status.code(), Some(2));
}

#[test]
fn given_directories_are_audited() {
    let mut memory = Memory::default();
    memory.state(Path::new("/fixture/ok"), &[header(), stop(2)]);
    memory.state(Path::new("/fixture/bad"), &dirty());
    let clean = memory.run(["/fixture/ok"]);
    assert_eq!(clean.status.code(), Some(0));
    assert!(text(&clean.stderr).contains("directories=1 journals=1 violations=0"));
    let both = memory.run(["/fixture/ok", "/fixture/bad"]);
    assert_eq!(both.status.code(), Some(1));
    assert!(text(&both.stderr).contains("directories=2 journals=2 violations=1"));
    assert_snapshot(
        &format!("{:#?}", brief(&text(&both.stdout))),
        include_str!("snapshots/command__given_directories_are_audited.snap"),
    );
}

#[test]
fn a_given_directory_may_be_in_progress() {
    let mut memory = Memory::default();
    memory.state(Path::new("/fixture/open"), &[header(), phase(2, 0, None)]);
    let output = memory.run(["/fixture/open"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stdout));
}

/// The line `event` is written as, to edit into one this build does not wholly know.
fn line(event: &Event) -> Value {
    serde_json::from_str(&event.encode().unwrap()).unwrap()
}

/// The session's `session.start` with a field this build does not know, nested in its first core.
fn header_with_unknown_field() -> Value {
    let mut header = line(&header());
    header["data"]["cores"][0]["wild"] = json!(2);
    header
}

fn future_event(seq: u64) -> Value {
    json!({"seq": seq, "time": "2026-01-01T00:00:00Z", "boot": "b", "kind": "future.thing", "msg": "m", "data": {}})
}

#[test]
fn a_given_directory_reports_what_its_lenient_read_left_out_and_is_still_audited() {
    let mut memory = Memory::default();
    let dir = Path::new("/fixture/bad");
    memory.journal(
        dir,
        &[
            header_with_unknown_field(),
            future_event(2),
            line(&phase(3, 0, Some(-30))),
            line(&applied(4, [-30, 0])),
        ],
    );
    let output = memory.run([dir]);
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert_eq!(
        brief(&text(&output.stdout)),
        [
            "journal=\"live\" seq=4 \"avoidance\": \"core 0 offset -30 reaches failure point -30 recorded at seq 3 \
             (tuner.md: P[c] <= fail[c])\""
        ]
    );
    let skipped = format!(
        "togi-tool-audit: {:?} live: skipped 1 event of a kind this build does not know (future.thing at line 2); \
         ignored unknown fields: session.start data.cores[0].wild (line 1)",
        dir.to_string_lossy()
    );
    assert!(stderr.lines().any(|line| line == skipped), "{stderr}");
    assert!(
        stderr.contains("directories=1 journals=1 violations=1"),
        "{stderr}"
    );
}
