use serde_json::{Value, json};
use test_case::test_case;

use super::{colored, event_with, plain, utc};
use crate::{Style, style_for};

fn vector() -> Value {
    json!({"cores": {"0": -5}, "mapping": {"phase": "core", "units": [{"unit": {"core": 0}, "cores": [0], "parent": {"ccd": 0}}]}})
}

fn decision(action: Value) -> Value {
    json!({"decision": action, "cold_boot": "boot-a", "before": vector(), "proposed": vector(), "hold": {"application": 1, "credited": [], "eligible_s": 180000}, "ceilings": [], "events": []})
}

fn edge(state: Value) -> Value {
    json!({"unit": {"core": 0}, "firmware": {"bios_version": "test", "board": "test", "cpu_model": "test", "microcode": "test", "boost_limit_mhz": 5000}, "epoch": 1, "old": {"kind": "unknown"}, "new": state, "ceiling": -25, "margin": 5})
}

fn stop(reason: Value) -> Value {
    json!({"unit": {"core": 0}, "reason": reason, "profile": vector(), "last_clean": -5, "witnesses": [7]})
}

fn charge(exhausted: bool) -> Value {
    json!({"campaign": {"session": "20261002T011407Z", "seq": 1}, "event": {"kind": "boot", "boot": "boot-a"}, "aliases": [], "profile": vector(), "phase": "learning", "unit_phase": "core", "provisional": true, "consumed": 2, "remaining": 0, "exhausted": exhausted})
}

fn restoration(state: Value) -> Value {
    json!({"saved": vector(), "state": state})
}

#[test_case("unit.edge", edge(json!({"kind": "observed", "offset": -30, "witness": 4})), Style::Red; "observed")]
#[test_case("unit.edge", edge(json!({"kind": "unknown"})), Style::Plain; "unknown")]
#[test_case("unit.edge", edge(json!({"kind": "censored", "limit": -50, "witnesses": [4]})), Style::Plain; "censored")]
#[test_case("climb.decision", decision(json!({"kind": "hold", "blocked_by": []})), Style::Plain; "held")]
#[test_case("climb.decision", decision(json!({"kind": "promote", "unit": {"core": 0}, "from": 0, "to": -5, "step": 5})), Style::Green; "promotion")]
#[test_case("climb.decision", decision(json!({"kind": "retreat", "units": [{"core": 0}], "cause": {"kind": "adverse_ceiling", "edges": [4]}})), Style::Yellow; "retreat")]
#[test_case("climb.decision", decision(json!({"kind": "restore", "restoration": "firmware", "units": [{"core": 0}]})), Style::Green; "restore")]
#[test_case("climb.decision", decision(json!({"kind": "firmware_zero"})), Style::Yellow; "firmware_zero")]
#[test_case("unit.stop", stop(json!({"kind": "ceiling"})), Style::Green; "ceiling_finish")]
#[test_case("unit.stop", stop(json!({"kind": "allowance_exhausted"})), Style::Yellow; "allowance_finish")]
#[test_case("unit.stop", stop(json!({"kind": "adverse_ceiling"})), Style::Yellow; "adverse_ceiling_stop")]
#[test_case("unit.stop", stop(json!({"kind": "veto"})), Style::Yellow; "veto_stop")]
#[test_case("unit.stop", stop(json!({"kind": "first_backstep", "event": {"kind": "boot", "boot": "boot-a"}, "application": 1})), Style::Yellow; "first_backstep")]
#[test_case("unit.stop", stop(json!({"kind": "predecessor", "original": {"session": "20261002T011407Z", "seq": 4}})), Style::Yellow; "predecessor_stop")]
#[test_case("unit.stop", stop(json!({"kind": "restoration_backstep", "event": {"kind": "boot", "boot": "boot-a"}})), Style::Yellow; "restoration_backstep")]
#[test_case("campaign.charge", charge(false), Style::Red; "provisional_charge")]
#[test_case("campaign.charge", charge(true), Style::Yellow; "exhausted_charge")]
#[test_case("firmware.restoration", restoration(json!({"kind": "saved"})), Style::Plain; "saved")]
#[test_case("firmware.restoration", restoration(json!({"kind": "qualified", "certificate": 4})), Style::Green; "qualified")]
#[test_case("firmware.restoration", restoration(json!({"kind": "held", "hold": {"application": 1, "credited": [], "eligible_s": 1}})), Style::Plain; "stock_hold")]
#[test_case("firmware.restoration", restoration(json!({"kind": "restored", "application": 4})), Style::Green; "restored")]
#[test_case("firmware.restoration", restoration(json!({"kind": "stepped_back", "event": {"kind": "boot", "boot": "boot-a"}})), Style::Yellow; "restoration_retreat")]
#[test_case("unit.mapping", json!({"phase": "core", "units": [{"unit": {"core": 0}, "cores": [0], "parent": {"ccd": 0}}]}), Style::Plain; "unit_mapping")]
#[test_case("campaign.start", json!({"campaign": {"session": "20261002T011407Z", "seq": 1}, "allowance": 2, "origin": {"kind": "fresh"}, "history": {"kind": "complete"}}), Style::Plain; "campaign_start")]
#[test_case("successor.imported", json!({"sources": [], "counts": {"selected": 0, "imported": 0, "duplicates": 0, "unknown": 0, "excluded": 0}, "vetoes": [], "night_observations": [], "campaign": {"kind": "complete"}}), Style::Plain; "successor_import")]
fn ruleset14_style_comes_from_payload_not_message(kind: &str, data: Value, expected: Style) {
    let event = event_with(kind, "FAIL crash success held finished", &data);
    assert_eq!(style_for(&event.payload), expected);
    let plain_line = plain().line(&event, &utc()).to_string();
    let colored_line = colored().line(&event, &utc()).to_string();
    assert!(!plain_line.contains('\x1b'));
    if expected == Style::Plain {
        assert_eq!(colored_line, plain_line);
    } else {
        assert!(colored_line.contains('\x1b'));
    }
}

#[test_case("cpu_suspect", Style::Red; "cpu_suspect")]
#[test_case("non_cpu", Style::Plain; "non_cpu")]
#[test_case("unexplained", Style::Plain; "unexplained")]
fn reclassification_color_uses_the_new_classification(kind: &str, expected: Style) {
    let to = if kind == "cpu_suspect" {
        json!({"kind": kind, "certainty": "definite"})
    } else {
        json!({"kind": kind})
    };
    let data = json!({"campaign": {"session": "20261002T011407Z", "seq": 1}, "event": {"kind": "boot", "boot": "boot-a"}, "from": {"kind": "cpu_suspect", "certainty": "provisional"}, "to": to});
    assert_eq!(
        style_for(&event_with("campaign.reclassified", "exhausted", &data).payload),
        expected
    );
}
