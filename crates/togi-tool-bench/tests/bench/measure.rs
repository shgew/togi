//! Measurements of journals of whole simulated sessions, which the real tuner and simulator produce in memory.

use std::path::Path;
use std::time::Duration;

use jiff::Timestamp;
use serde_json::{Value, json};
use togi_journal::{Event, Payload, SCHEMA};
use togi_machine::{LEGACY_REGIMES, Regime, TargetOffset, workloads};
use togi_sim::definition::CoreDef;
use togi_sim::replay::{Replay, ReplayFact, ReplayOutcome};
use togi_sim::{MachineDefinition, Model, file};
use togi_simrun::SimOutcome;
use togi_tool_bench::{Measured, Status, measure};
use togi_tool_trialfacts::Extracts;

use crate::common;
use crate::session::{Session, session};

fn definition(name: &str) -> MachineDefinition {
    let tree = common::Tree::committed();
    file::load_from(tree.path().join("machines").join(name), &tree)
        .unwrap()
        .0
}

fn measured(session: &Session, definition: &MachineDefinition, seed: u64) -> Measured {
    let model = Model::new(definition, seed).unwrap();
    measure(&session.events, &model, None).unwrap()
}

#[test]
fn a_boot_capped_session_is_censored_with_its_crashes_and_time_so_far() {
    let definition = crate::session::hunting();
    // Cap before the final recovery boot, with the first crash already recorded.
    let capped = session(&definition, 1, None, 2);
    assert!(matches!(capped.outcome, SimOutcome::Capped { .. }));
    let m = measured(&capped, &definition, 1);
    assert_eq!(m.crashes, 1);
    assert!(m.sim_hours > 0.0);
    assert_eq!(
        Status::of(3, !capped.events.is_empty(), false, false),
        Status::Censored
    );
}

#[test]
fn a_concluded_session_counts_its_trials_and_hazards() {
    let definition = MachineDefinition::default();
    let done = crate::session::brief(&definition, 1);
    assert!(matches!(done.outcome, SimOutcome::Stopped(_)));
    let m = measured(&done, &definition, 1);
    assert!(m.trials > 0 && m.trial_hours > 0.0);
    assert_eq!(m.real_answers, 0);
    assert!((m.real_answer_share).abs() < f64::EPSILON);
    assert_eq!(m.hazard_per_h.len(), LEGACY_REGIMES.len());
    assert!(m.hazard_per_h.values().all(|rate| *rate >= 0.0));
    assert!(
        (m.hazard_max_per_h - m.hazard_per_h.values().copied().fold(0.0, f64::max)).abs() < 1e-12
    );
}

#[test]
fn a_journal_with_no_events_measures_nothing() {
    let model = Model::new(&MachineDefinition::default(), 1).unwrap();
    let m = measure(&[], &model, None).unwrap();
    assert_eq!((m.sim_hours, m.trials, m.crashes), (0.0, 0, 0));
}

#[test]
fn measuring_twice_gives_the_same_numbers() {
    let definition = MachineDefinition::default();
    let capped = crate::session::brief(&definition, 3);
    assert_eq!(
        measured(&capped, &definition, 3),
        measured(&capped, &definition, 3)
    );
}

#[test]
fn legacy_per_core_facts_do_not_answer_current_nonzero_ccd_trials() {
    let path = Path::new("/bench/machines/target-fit-0.json");
    let definition = definition("target-fit-0.json");
    let replay = Extracts::new()
        .replay_with(path, &definition, |_| {
            togi_tool_trialfacts::decode(
                include_bytes!("../../../../tools/bench/facts/target.jsonl.gz").as_slice(),
            )
        })
        .unwrap();
    let replayed = session(&definition, 1, Some(replay.clone()), 6);
    let model = Model::new(&definition, 1).unwrap();
    let with = measure(&replayed.events, &model, Some(&replay)).unwrap();
    let without = measure(&replayed.events, &model, None).unwrap();
    assert!(with.trials > 0);
    assert_eq!(with.real_answers, 0);
    for event in &replayed.events {
        if let Payload::TrialIntent(intent) = &event.payload {
            assert!(intent.profile.iter().any(|(_, offset)| offset.get() != 0));
        }
    }
    assert!(with.real_answers <= with.trials);
    assert!(
        (with.real_answer_share - f64::from(with.real_answers) / f64::from(with.trials)).abs()
            < 1e-12
    );
    assert_eq!(without.real_answers, 0);
    assert_eq!(with.trials, without.trials);
}

#[test]
fn the_other_regimes_hazards_are_in_the_record() {
    let definition = MachineDefinition::default();
    let capped = crate::session::brief(&definition, 2);
    let m = measured(&capped, &definition, 2);
    for regime in LEGACY_REGIMES {
        assert!(m.hazard_per_h.contains_key(&regime));
    }
}

const EPOCH: i64 = 1_767_225_600;

fn at(second: i64) -> String {
    Timestamp::from_second(EPOCH + second).unwrap().to_string()
}

fn event(seq: usize, second: i64, kind: &str, data: &Value) -> Event {
    let line =
        json!({"seq": seq, "time": at(second), "boot": "b", "kind": kind, "msg": "", "data": data});
    Event::decode(&line.to_string()).unwrap()
}

fn journal(events: &[(i64, &str, Value)]) -> Vec<Event> {
    events
        .iter()
        .enumerate()
        .map(|(index, (second, kind, data))| event(index + 1, *second, kind, data))
        .collect()
}

fn start(cores: u8) -> Value {
    let cores: Vec<Value> = (0..cores)
        .map(|core| json!({"core": core, "ccd": 0, "cpus": [core]}))
        .collect();
    json!({
        "build": {"version": "0.0.0", "ruleset": 1, "schema": SCHEMA, "evidence": 1},
        "session": "20260101T000000Z",
        "cores": cores,
    })
}

fn two_cores() -> MachineDefinition {
    let core = |id, flat| CoreDef {
        id,
        alone: [-20; 5],
        together: [-20; 7],
        flat,
        idle: None,
        workload: std::collections::BTreeMap::new(),
    };
    MachineDefinition {
        cores: 2,
        core: vec![core(0, 0.00001), core(1, 0.00002)],
        ..MachineDefinition::default()
    }
}

fn hand_built() -> Vec<Event> {
    journal(&[
        (0, "session.start", start(2)),
        (0, "session.baseline", json!({"offsets": {"0": 0, "1": 0}})),
        (720, "profile.change", json!({"to": {"0": -10, "1": -11}})),
        (
            1080,
            "trial.end",
            json!({"trial": "1", "outcome": "pass", "duration_s": 90}),
        ),
        (1440, "crash.detected", json!({"previous_boot": "b"})),
        (
            1800,
            "trial.end",
            json!({"trial": "2", "outcome": "failure", "signal": "crash", "duration_s": 30}),
        ),
        (
            3600,
            "checking.cycle",
            json!({"cycle": 1, "event": "end", "passed": true}),
        ),
        (
            3960,
            "checking.cycle",
            json!({"cycle": 2, "event": "end", "passed": true}),
        ),
        (
            5040,
            "hunt.start",
            json!({
                "hunt": 1, "failure": 1, "regime": "R1",
                "workload": workloads(Regime::R1)[0].id, "cores": [0],
                "duration_s": 60, "failing": {"0": -11, "1": -11},
                "parked": {"0": -10, "1": -11}, "candidates": [0, 1],
                "trials": 1, "trial_s": 30, "miss": 0.0, "rate": 0.0,
                "ranking": [0, 1],
            }),
        ),
        (
            5400,
            "combination",
            json!({
                "combination": 1, "hunt": 1, "reason": "",
                "members": [{"core": 0, "offset": -12}, {"core": 1, "offset": -12}],
            }),
        ),
        (
            7200,
            "profile.applied",
            json!({"offsets": {"0": 0, "1": 0}, "condition": "parked"}),
        ),
    ])
}

fn close(got: f64, want: f64) {
    assert!((got - want).abs() <= 1e-12, "{got} != {want}");
}

#[test]
fn a_hand_built_journal_measures_every_field_exactly() {
    let model = Model::new(&two_cores(), 1).unwrap();
    let m = measure(&hand_built(), &model, None).unwrap();
    close(m.sim_hours, 2.0);
    assert_eq!((m.crashes, m.trials), (1, 2));
    close(m.trial_hours, 120.0 / 3600.0);
    assert_eq!((m.real_answers, m.real_answer_share), (0, 0.0));
    assert_eq!(m.hazard_per_h.len(), LEGACY_REGIMES.len());
    for rate in m.hazard_per_h.values() {
        close(*rate, 0.108);
    }
    close(m.hazard_max_per_h, 0.108);
}

#[test]
fn a_journal_cut_before_any_passed_cycle_counts_what_it_holds() {
    let model = Model::new(&two_cores(), 1).unwrap();
    let m = measure(&hand_built()[..4], &model, None).unwrap();
    assert_eq!((m.crashes, m.trials), (0, 1));
    close(m.sim_hours, 0.3);
    close(m.trial_hours, 90.0 / 3600.0);
}

#[test]
fn only_started_decisive_trials_matching_a_real_fact_are_real_answers() {
    let model = Model::new(
        &MachineDefinition {
            cores: 2,
            ..MachineDefinition::default()
        },
        1,
    )
    .unwrap();
    let work = workloads(Regime::R1)[0].id;
    let other = workloads(Regime::R1)[1].id;
    let fact = ReplayFact {
        context: model.bios_context().clone(),
        regime: Regime::R1,
        workload: work.to_owned(),
        cores: [togi_machine::CoreId::new(0).unwrap()]
            .into_iter()
            .collect(),
        duration: Duration::from_secs(60),
        profile: vec![TargetOffset::clamped(-20), TargetOffset::clamped(-21)],
        outcome: ReplayOutcome::Pass,
    };
    let replay = Replay::new([fact]).unwrap();
    let matching = json!({"0": -20, "1": -21});
    let trials = [
        (work, &matching, "pass", true),
        (work, &matching, "failure", true),
        (other, &matching, "pass", true),
        (work, &json!({"0": -20, "1": -22}), "pass", true),
        (work, &matching, "inconclusive", true),
        (work, &matching, "failure", false),
    ];
    let mut events = vec![
        (0, "session.start", start(2)),
        (0, "session.baseline", json!({"offsets": {"0": 0, "1": 0}})),
    ];
    for (index, (workload, profile, outcome, started)) in trials.iter().enumerate() {
        let id = index.to_string();
        events.push((
            1,
            "trial.intent",
            json!({
                "trial": id, "profile": profile, "core": 0, "regime": "R1",
                "workload": workload, "duration_s": 60, "condition": "alone",
            }),
        ));
        if *started {
            events.push((
                1,
                "trial.start",
                json!({"trial": id, "scope": "s", "pid": 1, "cpus": [0], "argv": []}),
            ));
        }
        let mut end = json!({"trial": id, "outcome": outcome, "duration_s": 60});
        if *outcome == "failure" {
            end["signal"] = json!("crash");
        }
        events.push((1, "trial.end", end));
    }
    let m = measure(&journal(&events), &model, Some(&replay)).unwrap();
    assert_eq!(m.trials, 6);
    assert_eq!(m.real_answers, 2);
    close(m.real_answer_share, 1.0 / 3.0);
}
