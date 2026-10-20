use jiff::Timestamp;
use serde_json::json;
use test_case::test_case;
use togi_machine::{KernelCursor, Regime};

use super::fixtures::{bare_config, boot, build, core, seq, target};
use crate::*;

fn full() -> ConfigLoaded {
    ConfigLoaded {
        build: build(),
        config: ConfigSnapshot {
            start_offsets: [(core(3), target(-10)), (core(15), target(0))]
                .into_iter()
                .collect(),
            candidate_solo_limits: [(core(2), target(-36))].into_iter().collect(),
            durations: ConfigDurations {
                search_trial_s: 60,
                short_trial_s: 86_400,
                checking_trial_s: 1,
                checking_idle_s: 7_200,
                checking_all_core_s: 4,
            },
            evidence: ConfigEvidence {
                miss: 0.001,
                rate: 0.25,
            },
            checking: ConfigChecking {
                cycle: vec![Regime::R6, Regime::R1, Regime::R6],
            },
            dead_ends: ConfigDeadEnds {
                inconclusive_in_a_row: 1,
                stray_crashes_in_a_row: 100,
            },
            backends: ConfigBackends {
                mprime: Some("/opt/mprime".into()),
                ycruncher: Some("/opt/y-cruncher".into()),
            },
            backend_user: Some("togi-trial".into()),
        },
        path: "/etc/togi/config.json".into(),
        file: true,
        kernel_cursor: Some(KernelCursor::from("cursor-1".to_owned())),
        kernel_error: Some("journal unavailable".into()),
    }
}

fn minimal() -> ConfigLoaded {
    ConfigLoaded {
        build: build(),
        config: bare_config(),
        path: "/etc/togi/config.json".into(),
        file: false,
        kernel_cursor: None,
        kernel_error: None,
    }
}

fn line(payload: ConfigLoaded) -> String {
    Event::new(
        seq(2),
        Stamp {
            time: "2026-10-02T01:14:07.120Z".parse::<Timestamp>().unwrap(),
            mono: None,
        },
        boot(),
        vec![seq(1)],
        payload.into(),
    )
    .encode()
    .unwrap()
}

fn pinned(snapshot: &str, payload: ConfigLoaded) {
    let encoded = line(payload.clone());
    let (_, expected) = snapshot.split_once("\n---\n").unwrap();
    assert_eq!(encoded, expected.trim_end_matches('\n'));
    let decoded = Event::decode(&encoded).unwrap();
    assert_eq!(decoded.payload, Payload::from(payload));
    assert_eq!(decoded.encode().unwrap(), encoded);
}

#[test]
fn a_fully_populated_config_loaded_matches_its_golden() {
    pinned(include_str!("snapshots/config_loaded_full.snap"), full());
}

#[test]
fn a_minimal_config_loaded_matches_its_golden() {
    pinned(
        include_str!("snapshots/config_loaded_minimal.snap"),
        minimal(),
    );
}

#[test]
fn the_snapshot_wire_keys_are_those_of_the_spec() {
    let value = serde_json::to_value(&minimal().config).unwrap();
    assert_eq!(
        value,
        json!({
            "start_offsets": {},
            "candidate_solo_limits": {},
            "durations": {
                "search_trial_s": 90,
                "short_trial_s": 120,
                "checking_trial_s": 120,
                "checking_idle_s": 900,
                "checking_all_core_s": 1200
            },
            "evidence": {"miss": 0.05, "rate": 0.5},
            "checking": {"cycle": ["R7", "R2", "R1"]},
            "dead_ends": {"inconclusive_in_a_row": 3, "stray_crashes_in_a_row": 3},
            "backends": {"mprime": null, "ycruncher": null},
            "backend_user": null
        })
    );
}

#[test_case(""; "the snapshot")]
#[test_case("/durations"; "durations")]
#[test_case("/evidence"; "evidence")]
#[test_case("/checking"; "checking")]
#[test_case("/dead_ends"; "dead ends")]
#[test_case("/backends"; "backends")]
fn every_nested_snapshot_object_refuses_an_unknown_field(pointer: &str) {
    let mut value = serde_json::to_value(minimal().config).unwrap();
    value.pointer_mut(pointer).unwrap()["bogus"] = json!(1);
    assert!(serde_json::from_value::<ConfigSnapshot>(value).is_err());
}

#[test]
fn a_snapshot_records_values_without_the_configuration_policy() {
    let mut value = serde_json::to_value(minimal().config).unwrap();
    value["durations"]["search_trial_s"] = json!(0);
    value["backends"]["mprime"] = json!("relative/path");
    let snapshot = serde_json::from_value::<ConfigSnapshot>(value).unwrap();
    assert_eq!(snapshot.durations.search_trial_s, 0);
}

#[test_case("/start_offsets", json!({"3": 10}); "a start offset on a core that does not exist")]
#[test_case("/start_offsets", json!({"16": -1}); "a start offset on a core past the last")]
#[test_case("/durations/search_trial_s", json!(-1); "a negative duration")]
#[test_case("/checking/cycle", json!(["R10"]); "an unknown regime")]
fn a_snapshot_still_refuses_what_its_own_types_refuse(pointer: &str, bad: serde_json::Value) {
    let mut value = serde_json::to_value(minimal().config).unwrap();
    *value.pointer_mut(pointer).unwrap() = bad;
    assert!(serde_json::from_value::<ConfigSnapshot>(value).is_err());
}
