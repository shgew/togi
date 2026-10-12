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

pub(crate) fn merged(mut base: Value, extra: Value) -> Value {
    base.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    base
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

pub(crate) fn phase(seq: u64, core: u8, failure_point: Option<i64>) -> Event {
    let mut data = json!({"core": core, "to": "has_room", "offset": 0, "reason": "r"});
    if let Some(point) = failure_point {
        data["failure_point"] = json!(point);
    }
    event(seq, "core.phase", data)
}

pub(crate) fn search(seq: u64, core: u8) -> Event {
    event(
        seq,
        "core.phase",
        json!({"core": core, "to": "search", "offset": 0, "reason": "r"}),
    )
}

pub(crate) fn applied(seq: u64, pair: [i64; 2]) -> Event {
    event(
        seq,
        "profile.applied",
        json!({"offsets": offsets(pair), "condition": "alone"}),
    )
}

pub(crate) fn restored(seq: u64, pair: [i64; 2]) -> Event {
    event(seq, "profile.restored", json!({"offsets": offsets(pair)}))
}

pub(crate) fn combination(seq: u64, id: u32, members: &[(u8, i64)]) -> Event {
    let members: Vec<Value> = members
        .iter()
        .map(|&(core, offset)| json!({"core": core, "offset": offset}))
        .collect();
    event(
        seq,
        "combination",
        json!({"combination": id, "members": members, "hunt": 1, "reason": "r"}),
    )
}

pub(crate) fn failure(seq: u64, core: u8, offset: i64) -> Event {
    event(
        seq,
        "failure",
        json!({"signal": "computation_error", "attribution": "attributed", "core": core, "offset": offset}),
    )
}

pub(crate) fn failure_with(seq: u64, extra: Value) -> Event {
    let base = json!({"signal": "computation_error", "attribution": "attributed"});
    event(seq, "failure", merged(base, extra))
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

pub(crate) fn decision(seq: u64, core: u8, kind: &str, failure_point: Option<i64>) -> Event {
    decision_in(seq, "checking", core, kind, failure_point)
}

pub(crate) fn decision_in(
    seq: u64,
    activity: &str,
    core: u8,
    kind: &str,
    failure_point: Option<i64>,
) -> Event {
    let mut data = json!({
        "core": core,
        "activity": activity,
        "decision": kind,
        "from_offset": -10,
        "to_offset": -10,
        "reason": "r",
    });
    if let Some(point) = failure_point {
        data["failure_point"] = json!(point);
    }
    event(seq, "tuner.decision", data)
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

pub(crate) fn cycle_end(seq: u64, passed: bool, full: bool) -> Event {
    event(
        seq,
        "checking.cycle",
        json!({"cycle": 1, "event": "end", "passed": passed, "full": full}),
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

pub(crate) fn run(cases: Vec<Case>) {
    for case in cases {
        assert_eq!(
            found(&case.events, case.completion),
            case.want,
            "{}",
            case.name
        );
    }
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
    let document = json!({
        "facts": togi_journal::State::replay(events).unwrap(),
        "tuner": togi_tuner::replay(events).unwrap().project(),
    });
    std::fs::write(
        dir.join("state.json"),
        serde_json::to_vec(&document).unwrap(),
    )
    .unwrap();
}
