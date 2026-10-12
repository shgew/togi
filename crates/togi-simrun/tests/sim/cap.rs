use std::collections::BTreeSet;
use std::io::ErrorKind;

use togi_journal::{EventKind, LockError, Payload, StateDir};
use togi_machine::CoreId;
use togi_simrun::{Observe, Persistence, SimError, SimOutcome, simulate};

use crate::common::{Bench, hunt_machine, nz, simulator, state_json};

#[test]
fn boot_cap_leaves_a_partial_session_in_the_journal() {
    let bench = Bench::new(&hunt_machine(2), 1);
    let mut input = bench.input();
    input.max_boots = nz(3);
    let SimOutcome::Capped { boots } = simulate(input).unwrap() else {
        panic!("a crashing machine stopped within 3 boots");
    };
    assert_eq!(boots, nz(3));

    let events = bench.events();
    let boot_ids: BTreeSet<_> = events.iter().map(|event| event.boot.clone()).collect();
    let crashes = events
        .iter()
        .filter(|event| matches!(event.payload, Payload::CrashDetected(_)))
        .count();
    assert_eq!((boot_ids.len(), crashes), (3, 2));

    let state: serde_json::Value =
        serde_json::from_slice(&crate::common::state_json(&bench.fs).unwrap()).unwrap();
    let last = events.last().unwrap().seq.get();
    assert_eq!(state["facts"]["last_seq"], serde_json::json!(last));
}

#[test]
fn a_cap_whose_last_state_cannot_be_published_is_an_error_not_a_cap() {
    let bench = Bench::new(&hunt_machine(2), 1);
    let mut input = bench.input();
    input.max_boots = nz(3);
    let fs = bench.fs.clone();
    let mut crashes = 0;
    input.observer = Some(Box::new(move |event| {
        if event.kind() == EventKind::CrashDetected {
            crashes += 1;
            if crashes == 2 {
                fs.fail_state_publication();
            }
        }
        Observe::Continue
    }));
    let error = simulate(input).err().unwrap();
    assert!(
        matches!(error, SimError::Publish { boot: 3, .. }),
        "{error}"
    );
    let state: serde_json::Value = serde_json::from_slice(&state_json(&bench.fs).unwrap()).unwrap();
    let last = bench.events().last().unwrap().seq.get();
    assert!(state["facts"]["last_seq"].as_u64().unwrap() < last);
}

#[test]
fn a_held_lock_refuses_the_simulation_before_any_journal_or_offset_change() {
    let dir = tempfile::tempdir().unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "the test holds the state directory's writer lock on disk"
    )]
    let held = StateDir::lock_unsynced(dir.path()).unwrap();
    let sim = simulator(&hunt_machine(2), 1);
    let mut input = crate::common::input(sim.clone(), Persistence::Files(dir.path().to_path_buf()));
    input.max_boots = nz(2);
    let error = simulate(input).err().unwrap();
    assert!(
        matches!(error, SimError::Lock(LockError::Held { .. })),
        "{error}"
    );
    drop(held);

    let journal = dir.path().join("events.jsonl");
    assert_eq!(
        std::fs::metadata(&journal).map(|_| ()).unwrap_err().kind(),
        ErrorKind::NotFound
    );
    let mut machine = sim.seams();
    for core in 0..2 {
        let offset = machine.smu.offset(CoreId::new(core).unwrap()).unwrap();
        assert_eq!(offset.0, 0);
    }
}
