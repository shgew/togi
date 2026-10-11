use serde_json::json;
use togi_journal::Event;

use crate::fixtures::*;

fn decodes(kind: &str, data: serde_json::Value) -> bool {
    let record = json!({"seq": 2, "time": "2026-01-01T00:00:00Z", "boot": "b", "kind": kind, "msg": kind, "data": data});
    Event::decode(&record.to_string()).is_ok()
}

#[test]
fn an_offset_the_tuner_chooses_cannot_leave_the_range_in_a_journal() {
    for (offset, readable) in [(-50, true), (0, true), (-51, false), (1, false)] {
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
                "core.phase",
                json!({"core": 0, "to": "search", "offset": offset, "reason": "r"})
            ),
            readable,
            "core phase at {offset}"
        );
        assert_eq!(
            decodes(
                "tuner.decision",
                json!({"core": 0, "activity": "search", "decision": "backoff", "from_offset": 0, "to_offset": offset, "reason": "r"})
            ),
            readable,
            "tuner decision to {offset}"
        );
    }
}

#[test]
fn the_hardware_reports_offsets_outside_the_range_and_the_journal_keeps_them() {
    for offset in [-51, 1, 10] {
        assert!(
            decodes("smu.readback", json!({"core": 0, "offset": offset})),
            "readback at {offset}"
        );
        assert!(
            decodes("session.baseline", json!({"offsets": {"0": offset}})),
            "baseline at {offset}"
        );
    }
}
