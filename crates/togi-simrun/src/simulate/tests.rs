use std::collections::BTreeMap;
use std::num::NonZeroU32;

use jiff::tz::TimeZone;
use test_case::test_case;
use togi_config::Config;
use togi_journal::{Build, EventKind, FixStamp, MemFs, SCHEMA};
use togi_machine::{CancellationToken, Signal};
use togi_render::Renderer;
use togi_session::{ConfigSource, Divergence};
use togi_sim::definition::{CoreDef, ModelDef, Weights};
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};

use super::{due, events, lock, simulate, simulate_checked, verify};
use crate::{Observe, Persistence, Replay, SimError, SimInput, SimOutcome};

fn nz(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).unwrap()
}

fn verified(max_boots: u32, every: Option<NonZeroU32>) -> Vec<u32> {
    (1..=max_boots)
        .filter(|&number| due(number, nz(max_boots), true, every))
        .collect()
}

#[test]
fn a_capped_simulation_checks_each_crashed_boot_at_most_once() {
    assert_eq!(verified(3, None), [3]);
    assert_eq!(verified(3, Some(nz(1))), [1, 2, 3]);
    assert_eq!(verified(3, Some(nz(2))), [2, 3]);
    assert_eq!(verified(5, Some(nz(2))), [2, 4, 5]);
}

#[test]
fn a_boot_that_ends_without_a_crash_is_always_checked() {
    assert!(due(1, nz(3), false, None));
    assert!(!due(1, nz(3), true, None));
}

fn sim_input(
    simulator: Simulator,
    persistence: Persistence,
    replay: Replay,
    verify_every: Option<NonZeroU32>,
) -> SimInput {
    let stop_at = Some(simulator.now() + jiff::SignedDuration::from_hours(24));
    SimInput {
        config: Config::default(),
        source: ConfigSource::default(),
        build: Build {
            version: "0.0.0".into(),
            rev: None,
            ruleset: 1,
            schema: SCHEMA,
            fixes: FixStamp::none(),
            evidence: 1,
        },
        simulator,
        persistence,
        replay,
        verify_every,
        max_boots: nz(3),
        stop_at,
        write_samples: false,
        defects: togi_defect::KNOWN_DEFECTS,
        log: None,
        renderer: Renderer::default(),
        zone: TimeZone::UTC,
        cancel: CancellationToken::new(),
        observer: None,
        dump_to: None,
        adapt_trials: None,
    }
}

/// Four cores, a neighbour on each CCD, with past-limit crashes at rate 1.
fn machine() -> Simulator {
    let definition = MachineDefinition {
        cores: 4,
        core: (0..4)
            .map(|id| CoreDef {
                id,
                alone: [-10; 5],
                together: [-10, -10, -10, -10, -10, -10, -50],
                flat: 0.0,
                idle: None,
                workload: BTreeMap::new(),
            })
            .collect(),
        model: ModelDef {
            past_limit_rate: 1.0,
            signals: Weights::from([(Signal::Crash, 1.0)]),
            crash_mce: 0.0,
            ..ModelDef::default()
        },
        ..MachineDefinition::default()
    };
    Simulator::new(
        Model::new(&definition, 1).unwrap(),
        RunSettings {
            seed: 1,
            ..RunSettings::default()
        },
    )
    .unwrap()
}

/// The machine, crashed at each of the first three trial ends, so each of the first three boots crashes before
/// the simulation's cap of three boots.
fn crashing_input(persistence: Persistence, replay: Replay, every: Option<NonZeroU32>) -> SimInput {
    let simulator = machine();
    let crasher = simulator.clone();
    let mut ends = 0;
    let mut input = sim_input(simulator, persistence, replay, every);
    input.observer = Some(Box::new(move |event| {
        if event.kind() == EventKind::TrialEnd && ends < 3 {
            ends += 1;
            crasher.crash();
        }
        Observe::Continue
    }));
    input
}

fn checked_boots(replay: Replay, every: Option<NonZeroU32>) -> Vec<u32> {
    let mut boots = Vec::new();
    let outcome = simulate_checked(
        crashing_input(Persistence::Memory(MemFs::new()), replay, every),
        &mut |capsule, events, latched, boot| {
            boots.push(boot);
            verify(capsule, events, latched, boot)
        },
    )
    .unwrap();
    if let SimOutcome::Stopped(finished) = &outcome {
        panic!("{:?} {:?} {boots:?}", finished.stop, finished.errors);
    }
    assert!(matches!(outcome, SimOutcome::Capped { .. }));
    boots
}

#[test_case(Replay::Warm; "warm")]
#[test_case(Replay::Cold; "cold")]
fn a_capped_simulation_checks_the_boots_its_cadence_names_in_either_replay(replay: Replay) {
    assert_eq!(checked_boots(replay, None), [3], "{replay:?}");
    assert_eq!(checked_boots(replay, Some(nz(1))), [1, 2, 3], "{replay:?}");
    assert_eq!(checked_boots(replay, Some(nz(2))), [2, 3], "{replay:?}");
}

#[test_case(Replay::Warm; "warm")]
#[test_case(Replay::Cold; "cold")]
fn a_failed_check_of_the_last_boot_fails_the_simulation_instead_of_capping_it(replay: Replay) {
    let mut boots = Vec::new();
    let outcome = simulate_checked(
        crashing_input(Persistence::Memory(MemFs::new()), replay, None),
        &mut |_, _, _, boot| {
            boots.push(boot);
            Err(SimError::NoCapsule { boot })
        },
    );
    assert!(
        matches!(outcome, Err(SimError::NoCapsule { boot: 3 })),
        "{replay:?}"
    );
    assert_eq!(boots, [3], "{replay:?}");
}

#[test]
fn a_resume_state_that_differs_from_the_journal_fails_the_simulation() {
    let simulator = Simulator::new(
        Model::new(&MachineDefinition::default(), 1).unwrap(),
        RunSettings {
            seed: 1,
            ..RunSettings::default()
        },
    )
    .unwrap();
    let persistence = Persistence::Memory(MemFs::new());
    let mut input = sim_input(simulator, persistence.clone(), Replay::Warm, None);
    input.observer = Some(Box::new(|event| {
        if event.kind() == EventKind::TrialEnd {
            Observe::Stop
        } else {
            Observe::Continue
        }
    }));
    let outcome = simulate(input).unwrap();
    let SimOutcome::Stopped(finished) = outcome else {
        panic!("the run did not stop at its first trial end");
    };
    let capsule = finished.capsule.unwrap();
    let journal = events(&persistence).unwrap();
    let (_, short) = journal.split_last().unwrap();

    verify(Some(&capsule), &journal, false, 3).unwrap();
    let error = verify(Some(&capsule), short, false, 3).unwrap_err();
    assert!(
        matches!(
            error,
            SimError::Diverged {
                boot: 3,
                source: Divergence::Last { .. }
            }
        ),
        "{error}"
    );
    assert!(error.to_string().contains("after boot 3"), "{error}");
    assert!(matches!(
        verify(None, &journal, false, 3),
        Err(SimError::NoCapsule { boot: 3 })
    ));
    verify(None, &journal, true, 3).unwrap();
}

#[test]
fn a_failed_check_after_a_warm_crash_still_publishes_the_crashed_boots_state() {
    let fs = MemFs::new();
    let projected = MemFs::new();
    let outcome = simulate_checked(
        crashing_input(Persistence::Memory(fs.clone()), Replay::Warm, Some(nz(1))),
        &mut |capsule, events, latched, boot| {
            verify(capsule, events, latched, boot)?;
            if boot < 2 {
                return Ok(());
            }
            let mut dir = lock(&Persistence::Memory(projected.clone())).unwrap();
            capsule.unwrap().publish(&mut dir).unwrap();
            Err(SimError::NoCapsule { boot })
        },
    );
    assert!(matches!(outcome, Err(SimError::NoCapsule { boot: 2 })));
    let published = fs.contents("/state/state.json");
    assert!(published.is_some());
    assert_eq!(published, projected.contents("/state/state.json"));
}

#[test]
fn a_dump_receives_the_stopped_memory_directory_after_publication() {
    let fs = MemFs::new();
    let mut input = sim_input(
        machine(),
        Persistence::Memory(fs.clone()),
        Replay::Warm,
        None,
    );
    input.observer = Some(Box::new(|event| {
        if event.kind() == EventKind::TrialEnd {
            Observe::Stop
        } else {
            Observe::Continue
        }
    }));
    input.dump_to = Some("/dump".into());
    let mut dumped = false;
    let outcome = super::simulate_with_dump(input, &mut verify, &mut |memory, destination| {
        assert_eq!(destination, std::path::Path::new("/dump"));
        assert_eq!(
            memory.contents("/state/events.jsonl"),
            fs.contents("/state/events.jsonl")
        );
        assert!(memory.contents("/state/state.json").is_some());
        assert!(memory.contents("/state/machine.json").is_none());
        dumped = true;
        Ok(())
    })
    .unwrap();
    assert!(matches!(outcome, SimOutcome::Stopped(_)));
    assert!(dumped);
}

#[test]
fn a_dump_of_a_capped_simulation_includes_the_machine_it_left() {
    let mut input = crashing_input(Persistence::Memory(MemFs::new()), Replay::Warm, None);
    input.dump_to = Some("/dump".into());
    let mut dumped = false;
    let outcome = super::simulate_with_dump(input, &mut verify, &mut |memory, _| {
        assert!(!memory.contents("/state/machine.json").unwrap().is_empty());
        assert!(!memory.contents("/state/events.jsonl").unwrap().is_empty());
        assert!(memory.contents("/state/state.json").is_some());
        dumped = true;
        Ok(())
    })
    .unwrap();
    assert!(matches!(outcome, SimOutcome::Capped { .. }));
    assert!(dumped);
}

#[test]
fn a_dump_that_cannot_be_written_fails_the_simulation_instead_of_capping_it() {
    let mut input = crashing_input(Persistence::Memory(MemFs::new()), Replay::Warm, None);
    input.dump_to = Some("/dump".into());
    let outcome = super::simulate_with_dump(input, &mut verify, &mut |memory, _| {
        assert!(memory.contents("/state/machine.json").is_some());
        Err(std::io::Error::other("dump refused"))
    });
    assert!(matches!(outcome, Err(SimError::Dump(_))));
}

#[test]
fn a_dump_asked_of_a_state_directory_on_disk_writes_nothing() {
    super::dump_with(None, std::path::Path::new("/dump"), &mut |_, _| {
        panic!("a file-backed simulation must not invoke the memory dump adapter")
    })
    .unwrap();
}
