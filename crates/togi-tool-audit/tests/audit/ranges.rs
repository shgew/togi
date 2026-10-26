use serde_json::json;
use togi_journal::Event;

use crate::fixtures::*;

fn decodes(kind: &str, data: serde_json::Value) -> bool {
    let record = json!({"seq": 2, "time": "2026-01-01T00:00:00Z", "boot": "b", "kind": kind, "msg": kind, "data": data});
    Event::decode(&record.to_string()).is_ok()
}

#[test_case::test_case((-50, true); "50 true")]
#[test_case::test_case((0, true); "0 true")]
#[test_case::test_case((-51, false); "51 false")]
#[test_case::test_case((1, false); "1 false")]
fn an_offset_the_tuner_chooses_cannot_leave_the_range_in_a_journal(case: (i64, bool)) {
    let (offset, readable) = case;

    assert_eq!(
        decodes(
            "profile.applied",
            json!({"offsets": {"0": offset, "8": 0}, "condition": "alone"})
        ),
        readable,
        "applied profile at {offset}"
    );
    assert_eq!(
        decodes("smu.intent", set(0, offset)),
        readable,
        "SMU intent at {offset}"
    );
    assert_eq!(
        decodes("smu.write", set(0, offset)),
        readable,
        "SMU write at {offset}"
    );
    assert_eq!(
        decodes(
            "trial.intent",
            json!({
                "trial": "0001",
                "profile": {"0": 0, "8": 0},
                "regime": "R7",
                "workload": "mprime-avx2-36k-248k",
                "duration_s": 60,
                "condition": "alone",
                "core": 0,
                "offset": offset,
            })
        ),
        readable,
        "trial intent at {offset}"
    );
}

#[test_case::test_case(-51; "51")]
#[test_case::test_case(1; "1")]
#[test_case::test_case(10; "10")]
fn the_hardware_reports_offsets_outside_the_range_and_the_journal_keeps_them(case: i64) {
    let offset = case;

    assert!(
        decodes("smu.readback", json!({"core": 0, "offset": offset})),
        "readback at {offset}"
    );
    assert!(
        decodes("session.baseline", json!({"offsets": {"0": offset}})),
        "baseline at {offset}"
    );
}
