//! Measurements of journals of whole simulated sessions, which the real tuner and simulator produce in memory.
#![expect(clippy::unwrap_used, reason = "helpers panic on a broken fixture")]

mod session;

use std::path::{Path, PathBuf};
use std::time::Duration;

use togi_machine::{CoreSet, REGIMES, Regime, TargetOffset, workloads};
use togi_sim::{Load, MachineDefinition, Model, file};
use togi_simrun::SimOutcome;
use togi_tool_bench::{Measured, Status, measure};
use togi_tool_trialfacts::Extracts;

use session::{Session, session};

fn machines() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/bench/machines")
}

fn measured(session: &Session, definition: &MachineDefinition, seed: u64) -> Measured {
    let model = Model::new(definition, seed).unwrap();
    measure(&session.events, &model, None).unwrap()
}

#[test]
fn a_boot_capped_session_is_censored_with_its_crashes_and_time_so_far() {
    let definition = MachineDefinition::default();
    let capped = session(&definition, 1, None, 4);
    assert!(matches!(capped.outcome, SimOutcome::Capped { .. }));
    let m = measured(&capped, &definition, 1);
    assert_eq!(m.crashes, 3);
    assert!(m.sim_hours > 0.0);
    assert_eq!(
        Status::of(3, !capped.events.is_empty(), false, false),
        Status::Censored
    );
}

#[test]
fn a_concluded_session_counts_its_cycles_trials_and_depth() {
    let definition = MachineDefinition::default();
    let done = session(&definition, 1, None, 1000);
    assert!(matches!(done.outcome, SimOutcome::Stopped(_)));
    let m = measured(&done, &definition, 1);
    assert!(m.passed_cycles >= 1);
    let first = m.first_passed_cycle_h.unwrap();
    assert!(first > 0.0 && first <= m.sim_hours);
    assert!(m.trials > 0 && m.trial_hours > 0.0);
    assert_eq!(m.final_profile.len(), 16);
    assert_eq!(
        m.depth,
        m.final_profile.iter().map(|(_, o)| o.0).sum::<i64>()
    );
    assert!(m.depth < 0);
    assert_eq!(m.real_answers, 0);
    assert!((m.real_answer_share).abs() < f64::EPSILON);
    assert_eq!(m.hazard_per_h.len(), REGIMES.len());
    assert!(m.hazard_per_h.values().all(|rate| *rate >= 0.0));
    assert!(
        (m.hazard_max_per_h - m.hazard_per_h.values().copied().fold(0.0, f64::max)).abs() < 1e-12
    );
}

#[test]
fn a_journal_with_no_events_measures_the_zero_profile() {
    let model = Model::new(&MachineDefinition::default(), 1).unwrap();
    let m = measure(&[], &model, None).unwrap();
    assert_eq!((m.sim_hours, m.trials, m.crashes, m.depth), (0.0, 0, 0, 0));
    assert_eq!(m.final_profile.len(), 16);
    assert!(m.first_passed_cycle_h.is_none());
}

#[test]
fn measuring_twice_gives_the_same_numbers() {
    let definition = MachineDefinition::default();
    let capped = session(&definition, 3, None, 2);
    assert_eq!(
        measured(&capped, &definition, 3),
        measured(&capped, &definition, 3)
    );
}

#[test]
fn a_machine_without_a_shared_rail_has_no_worst_r7_hazard() {
    let definition = MachineDefinition::default();
    let capped = session(&definition, 1, None, 2);
    assert!(
        measured(&capped, &definition, 1)
            .worst_r7_hazard_per_h
            .is_none()
    );
}

#[test]
fn the_worst_r7_hazard_covers_at_least_every_all_core_load() {
    let definition = file::load(machines().join("shared-voltage.json")).unwrap();
    let capped = session(&definition, 1, None, 3);
    let m = measured(&capped, &definition, 1);
    let worst = m.worst_r7_hazard_per_h.unwrap();
    let model = Model::new(&definition, 1).unwrap();
    let profile: Vec<TargetOffset> = (0..16)
        .map(|index| {
            let core = togi_machine::CoreId::new(index).unwrap();
            m.final_profile
                .get(core)
                .map_or(TargetOffset::ZERO, |o| TargetOffset::clamped(o.0))
        })
        .collect();
    let all: CoreSet = (0..16)
        .filter_map(|i| togi_machine::CoreId::new(i).ok())
        .collect();
    for workload in workloads(Regime::R7) {
        let load = Load {
            regime: Regime::R7,
            workload: workload.id,
            cores: all,
            duration: Duration::ZERO,
        };
        let rate = model.hazard(&profile, &load).unwrap() * 3600.0;
        assert!(worst >= rate - 1e-12, "{}: {worst} < {rate}", workload.id);
    }
}

#[test]
fn trials_a_real_fact_answers_are_counted_against_the_trials_that_ended() {
    let path = machines().join("target-fit-0.json");
    let definition = file::load(&path).unwrap();
    let replay = Extracts::new().replay(&path, &definition).unwrap();
    let replayed = session(&definition, 1, Some(replay.clone()), 6);
    let model = Model::new(&definition, 1).unwrap();
    let with = measure(&replayed.events, &model, Some(&replay)).unwrap();
    let without = measure(&replayed.events, &model, None).unwrap();
    assert!(with.real_answers > 0, "a replayed run has answered trials");
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
    let capped = session(&definition, 2, None, 2);
    let m = measured(&capped, &definition, 2);
    for regime in REGIMES {
        assert!(m.hazard_per_h.contains_key(&regime));
    }
}
