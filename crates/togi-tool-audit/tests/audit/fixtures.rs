use serde_json::{Value, json};
use togi_journal::{Event, Seq};
use togi_machine::BootId;
use togi_tool_audit::{Check, Completion, Violation, audit};

pub(crate) fn event(seq: u64, kind: &str, data: Value) -> Event {
    let record = json!({
        "seq": seq,
        "time": "2026-01-01T00:00:00Z",
        "boot": "b",
        "kind": kind,
        "msg": kind,
        "data": data,
    });
    Event::decode(&record.to_string()).unwrap()
}

pub(crate) fn cause(mut event: Event, seqs: &[u64]) -> Event {
    event.cause = seqs.iter().map(|&seq| Seq::new(seq).unwrap()).collect();
    event
}

pub(crate) fn boot(mut event: Event, name: &str) -> Event {
    event.boot = BootId::from(name.to_owned());
    event
}

pub(crate) fn build(fixes: u32) -> Value {
    json!({"version": "0.1.0", "ruleset": 1, "schema": 1, "fixes": fixes, "evidence": 1})
}

pub(crate) fn header() -> Event {
    event(
        1,
        "session.start",
        json!({
            "build": build(0),
            "session": "20260101T000000Z",
            "cores": [
                {"core": 0, "ccd": 0, "cpus": [0, 1]},
                {"core": 8, "ccd": 1, "cpus": [2, 3]},
            ],
        }),
    )
}

pub(crate) fn offsets(pair: [i64; 2]) -> Value {
    json!({"0": pair[0], "8": pair[1]})
}

pub(crate) fn phase(seq: u64, core: u8) -> Event {
    event(
        seq,
        "core.phase",
        json!({"core": core, "to": "has_room", "offset": 0, "reason": "r"}),
    )
}

pub(crate) fn restored(seq: u64, pair: [i64; 2]) -> Event {
    event(seq, "profile.restored", json!({"offsets": offsets(pair)}))
}

pub(crate) fn trial_intent(
    seq: u64,
    trial: &str,
    regime: &str,
    condition: &str,
    cores: &[u8],
) -> Event {
    let mut data = json!({
        "trial": trial,
        "profile": offsets([-30, 0]),
        "regime": regime,
        "workload": "mprime-avx2-36k-248k",
        "duration_s": 60,
        "condition": condition,
    });
    match cores {
        [core] => data["core"] = json!(core),
        _ => data["cores"] = json!(cores),
    }
    event(seq, "trial.intent", data)
}

pub(crate) fn trial_end(seq: u64, trial: &str) -> Event {
    event(
        seq,
        "trial.end",
        json!({"trial": trial, "outcome": "failure", "duration_s": 60}),
    )
}

pub(crate) fn intent(seq: u64, command: Value) -> Event {
    event(seq, "smu.intent", command)
}

pub(crate) fn write(seq: u64, command: Value, intent: u64) -> Event {
    cause(event(seq, "smu.write", command), &[intent])
}

pub(crate) fn set(core: u8, offset: i64) -> Value {
    json!({"op": "set", "core": core, "offset": offset})
}

pub(crate) fn set_all(offset: i64) -> Value {
    json!({"op": "set_all", "offset": offset})
}

pub(crate) fn readback(seq: u64, core: u8, offset: i64, write: Option<u64>) -> Event {
    let event = event(
        seq,
        "smu.readback",
        json!({"core": core, "offset": offset, "expected": offset}),
    );
    match write {
        Some(write) => cause(event, &[write]),
        None => event,
    }
}

pub(crate) fn stop(seq: u64) -> Event {
    event(seq, "shutdown", json!({"reason": "command"}))
}

pub(crate) fn shutdown(seq: u64, data: Value) -> Event {
    event(seq, "shutdown", data)
}

pub(crate) fn dead_end(seq: u64, detail: &str) -> Event {
    event(
        seq,
        "deadend",
        json!({"condition": "preflight", "detail": detail}),
    )
}

pub(crate) fn archived(seq: u64) -> Event {
    event(
        seq,
        "session.archived",
        json!({"session": "20260101T000000Z"}),
    )
}

pub(crate) fn warning(seq: u64) -> Event {
    event(
        seq,
        "session.warning",
        json!({"operation": "write state projection", "error": "disk full"}),
    )
}

pub(crate) fn reset_core(seq: u64, core: u8) -> Event {
    event(seq, "command.reset", json!({"target": {"core": core}}))
}

pub(crate) fn reset_all(seq: u64) -> Event {
    event(seq, "command.reset", json!({"target": "all"}))
}

const DIGEST: &str = "0707070707070707070707070707070707070707070707070707070707070707";

pub(crate) fn vector(pair: [i64; 2]) -> Value {
    json!({"ccd": {"0": pair[0], "1": pair[1]}, "cores": {"0": pair[0], "8": pair[1]}})
}

pub(crate) fn firmware(version: &str) -> Value {
    json!({
        "bios_version": version,
        "board": "X570",
        "cpu_model": "5950X",
        "microcode": "0xa201016",
        "boost_limit_mhz": 5000,
    })
}

pub(crate) fn context(seq: u64, version: &str) -> Event {
    event(seq, "session.context", firmware(version))
}

/// A `night.qualification` of `pair` under firmware `version`; `state` is `"complete"` or `"failed"`.
pub(crate) fn qualification(seq: u64, pair: [i64; 2], version: &str, state: &str) -> Event {
    let state = if state == "complete" {
        json!({"kind": "complete", "witnesses": []})
    } else {
        json!({"kind": "start", "outstanding": []})
    };
    event(
        seq,
        "night.qualification",
        json!({
            "profile": vector(pair),
            "firmware": firmware(version),
            "epoch": 1,
            "contents": [],
            "matrix": DIGEST,
            "purpose": "next_step",
            "state": state,
        }),
    )
}

pub(crate) fn day_applied(seq: u64, pair: [i64; 2]) -> Event {
    event(
        seq,
        "day.applied",
        json!({
            "application": 1,
            "cause": {"kind": "reapplication"},
            "target": vector(pair),
            "readbacks": {"0": pair[0], "8": pair[1]},
            "transactions": [],
            "boot": "b",
            "mono_ns": 1_000_000_000_u64,
        }),
    )
}

/// A `day.step` from `old` to `proposed`: a promotion of CCD 0, or a hold when `promote` is false.
pub(crate) fn day_step(seq: u64, old: [i64; 2], proposed: [i64; 2], promote: bool) -> Event {
    let step = if promote {
        json!({"kind": "promote", "changed": 0})
    } else {
        json!({"kind": "hold", "blocked_by": []})
    };
    event(
        seq,
        "day.step",
        json!({
            "decision": 1,
            "cold_boot": "b",
            "old": vector(old),
            "proposed": vector(proposed),
            "step": step,
            "hours": [],
            "hold_s": 0,
            "causes": {},
            "reason": "r",
        }),
    )
}

/// A `day.veto` of the whole profile `pair` for the record at `source`; `state` is `"active"` or `"cleared"`.
pub(crate) fn veto(seq: u64, source: u64, pair: [i64; 2], state: &str) -> Event {
    event(
        seq,
        "day.veto",
        json!({
            "event": {"kind": "record", "session": "20260101T000000Z", "seq": source},
            "profile": {
                "kind": "ccd",
                "ccd": {"0": pair[0], "1": pair[1]},
                "cores": {"0": pair[0], "8": pair[1]},
            },
            "state": state,
            "evidence": [],
            "reason": "r",
        }),
    )
}

/// A live `exposure.observed` of trial `trial`, which names itself as its source.
pub(crate) fn exposure(seq: u64, trial: &str, origin: &str) -> Event {
    event(
        seq,
        "exposure.observed",
        json!({
            "source": {"session": "20260101T000000Z", "seq": 2, "boot": "b", "trial": trial},
            "origin": {"kind": origin},
            "firmware": firmware("F31"),
            "epoch": 1,
            "class": {
                "regime": "R8",
                "workloads": [],
                "cores": [0],
                "position": {"kind": "light", "core": 0},
                "duration_s": 120,
                "placement": "isolated",
            },
            "profile": vector([-20, 0]),
            "tested": {"kind": "isolated", "ccd": 0, "offset": -20},
            "activity": "mapping",
            "outcome": "pass",
            "eligible_s": 120,
            "uninterrupted_s": 120,
        }),
    )
}

pub(crate) fn found(events: &[Event], completion: Completion) -> Vec<(u64, Check)> {
    audit(events, completion)
        .iter()
        .map(|Violation { seq, check, .. }| (seq.get(), *check))
        .collect()
}

pub(crate) struct Case {
    pub(crate) name: &'static str,
    pub(crate) events: Vec<Event>,
    pub(crate) completion: Completion,
    pub(crate) want: Vec<(u64, Check)>,
}

pub(crate) fn case(
    name: &'static str,
    mut body: Vec<Event>,
    completion: Completion,
    want: &[(u64, Check)],
) -> Case {
    let mut events = vec![header()];
    events.append(&mut body);
    Case {
        name,
        events,
        completion,
        want: want.to_vec(),
    }
}

pub(crate) fn run(case: Case) {
    assert_eq!(
        found(&case.events, case.completion),
        case.want,
        "{}",
        case.name
    );
}

#[expect(
    clippy::disallowed_methods,
    reason = "kernel audit fixtures explicitly inject an unsynced journal adapter; production keeps durable locking"
)]
pub(crate) fn disk_inspect(
    path: &std::path::Path,
    completion: togi_tool_audit::Completion,
) -> togi_tool_audit::DirectoryAudit {
    togi_tool_audit::audit_directory_with(path, completion, |path| {
        if !path.is_dir() {
            return Err(togi_tool_audit::OpenError::NotADirectory);
        }
        Ok(togi_journal::StateDir::lock_with_fs(
            togi_journal::fs::unsynced_disk_fs(),
            path,
        )?)
    })
}

pub(crate) fn state_document(events: &[Event]) -> Value {
    let state = togi_session::ReadState::replay(events).unwrap();
    let bytes: Vec<u8> = events
        .iter()
        .flat_map(|event| {
            let mut line = event.encode().unwrap().into_bytes();
            line.push(b'\n');
            line
        })
        .collect();
    togi_journal::snapshot::seal(
        togi_journal::snapshot::Prefix::of(&bytes, events.last().map(|event| event.seq)),
        &state.document(),
    )
    .unwrap()
}

/// Writes `events` as the live journal of a state directory on disk, with the `state.json` a run publishes at its
/// last event.
pub(crate) fn write_state_dir(dir: &std::path::Path, events: &[Event]) {
    std::fs::create_dir_all(dir).unwrap();
    let journal: String = events
        .iter()
        .map(|event| event.encode().unwrap() + "\n")
        .collect();
    std::fs::write(dir.join("events.jsonl"), journal).unwrap();
    let document = state_document(events);
    std::fs::write(
        dir.join("state.json"),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();
}
