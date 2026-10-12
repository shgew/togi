use std::num::NonZeroU32;

use jiff::tz::TimeZone;
use togi_config::Config;
use togi_journal::{Build, EventKind, MemFs, SCHEMA};
use togi_machine::CancellationToken;
use togi_render::Renderer;
use togi_session::{ConfigSource, Divergence, StopPolicy};
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};

use super::{due, events, simulate, verify};
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

#[test]
fn a_resume_state_that_differs_from_the_journal_fails_the_simulation() {
    let fs = MemFs::new();
    let simulator = Simulator::new(
        Model::new(&MachineDefinition::default(), 1).unwrap(),
        RunSettings {
            seed: 1,
            ..RunSettings::default()
        },
    );
    let persistence = Persistence::Memory(fs);
    let outcome = simulate(SimInput {
        config: Config::default(),
        source: ConfigSource::default(),
        build: Build {
            version: "0.0.0".into(),
            rev: None,
            ruleset: 1,
            schema: SCHEMA,
            fixes: 0,
            evidence: 1,
        },
        simulator,
        persistence: persistence.clone(),
        replay: Replay::Warm,
        verify_every: None,
        max_boots: nz(3),
        policy: StopPolicy {
            cycles: Some(nz(1)),
            first_result: false,
        },
        write_samples: false,
        defects: &[],
        log: None,
        renderer: Renderer::default(),
        zone: TimeZone::UTC,
        cancel: CancellationToken::new(),
        observer: Some(Box::new(|event| {
            if event.kind() == EventKind::TrialEnd {
                Observe::Stop
            } else {
                Observe::Continue
            }
        })),
        dump_to: None,
    })
    .unwrap();
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
