use std::path::{Path, PathBuf};

use togi_journal::MemFs;
use togi_journal::fs::{Fault, Fs, Op};
use togi_machine::{ResetKind, TargetOffset};
use togi_sim::{RestoreError, ScriptedMachine, ScriptedTopology, Simulator, TrialOutcome};
use togi_simrun::{Persistence, ResumeError, SimError, SimOutcome, resume_simulator, simulate};

use crate::common::{
    Bench, STATE, finish, four_core_simulator, input, nz, passing, reset_schedule,
    scripted_simulator, until_kind, watchdog_resets,
};

const MACHINE: &str = "machine.json";

/// A state directory a simulation can be split across processes in.
trait Store {
    fn persistence(&self) -> Persistence;
    /// Where the simulation keeps `name`.
    fn path(&self, name: &str) -> PathBuf;
    fn bytes(&self, name: &str) -> Option<Vec<u8>>;
    fn remove(&self, name: &str);

    /// `name` as text, with the state directory written as [`STATE`] so two directories compare.
    fn text(&self, name: &str) -> String {
        String::from_utf8(self.bytes(name).unwrap()).unwrap()
    }
}

impl Store for MemFs {
    fn persistence(&self) -> Persistence {
        Persistence::Memory(self.clone())
    }

    fn path(&self, name: &str) -> PathBuf {
        Path::new(STATE).join(name)
    }

    fn bytes(&self, name: &str) -> Option<Vec<u8>> {
        self.contents(self.path(name))
    }

    fn remove(&self, name: &str) {
        Fs::remove_file(self, &self.path(name)).unwrap();
    }
}

fn resumed(script: ScriptedMachine, seed: u64, store: &impl Store) -> Simulator {
    resume_simulator(&store.persistence(), script, seed).unwrap()
}

/// The whole night's input: the stop time is the first process's start plus the night, so a continuation that
/// resumes at a later clock stops at the same instant as an uninterrupted run.
fn night_input(sim: togi_sim::Simulator, persistence: Persistence) -> togi_simrun::SimInput {
    let end = four_core_simulator(1).now() + crate::common::NIGHT;
    let mut run = input(sim, persistence);
    run.stop_at = Some(end);
    run
}

fn to_the_end(sim: togi_sim::Simulator, store: &impl Store) {
    let outcome = simulate(night_input(sim, store.persistence())).unwrap();
    assert!(
        matches!(outcome, SimOutcome::Stopped(_)),
        "capped at the full cap"
    );
    assert!(
        store.bytes(MACHINE).is_none(),
        "a stopped simulation leaves no machine"
    );
}

fn capped(script: ScriptedMachine, seed: u64, store: &impl Store, boots: u32) -> serde_json::Value {
    let mut first = night_input(scripted_simulator(script, seed), store.persistence());
    first.max_boots = nz(boots);
    let outcome = simulate(first).unwrap();
    assert!(
        matches!(outcome, SimOutcome::Capped { boots: actual } if actual.get() == boots),
        "stopped within {boots} boots"
    );
    serde_json::from_slice(&store.bytes(MACHINE).unwrap()).unwrap()
}

#[test]
fn journal_only_resume_refuses_a_timestamp_without_room_for_reboot() {
    let bench = Bench::four_cores(1);
    let mut run = bench.input();
    run.observer = Some(until_kind(togi_journal::EventKind::SessionStart));
    finish(run);
    assert!(
        bench.fs.bytes(MACHINE).is_none(),
        "resume must use journal history"
    );
    let mut journal = Vec::new();
    for event in bench.events() {
        let mut value = serde_json::to_value(event).unwrap();
        value["time"] = serde_json::to_value(jiff::Timestamp::MAX).unwrap();
        serde_json::to_writer(&mut journal, &value).unwrap();
        journal.push(b'\n');
    }
    bench.fs.put(bench.fs.path("events.jsonl"), journal.clone());
    let error = resume_simulator(&bench.fs.persistence(), bench.sim.source(), 1)
        .err()
        .expect("the next boot timestamp cannot be represented");
    assert!(matches!(
        error,
        ResumeError::Construct(togi_sim::ArithmeticError::ResumeTime(_))
    ));
    assert_eq!(bench.fs.bytes("events.jsonl").unwrap(), journal);
    assert!(bench.fs.bytes(MACHINE).is_none());
}

fn split_matches_whole<S: Store>(
    new: impl Fn() -> S,
    script: ScriptedMachine,
    seed: u64,
    boots: u32,
) -> serde_json::Value {
    let whole = new();
    to_the_end(scripted_simulator(script, seed), &whole);

    let split = new();
    let saved = capped(script, seed, &split, boots);
    to_the_end(resumed(script, seed, &split), &split);

    let events = whole.text("events.jsonl");
    assert!(
        events.lines().count() > 100,
        "a short session proves little"
    );
    if let Some((line, (want, got))) = events
        .lines()
        .zip(split.text("events.jsonl").lines())
        .enumerate()
        .find(|(_, (want, got))| want != got)
    {
        panic!(
            "events.jsonl line {} differs\n  whole: {want}\n  split: {got}",
            line + 1
        );
    }
    assert_eq!(events, split.text("events.jsonl"));

    let forgotten = new();
    capped(script, seed, &forgotten, boots);
    forgotten.remove(MACHINE);
    to_the_end(resumed(script, seed, &forgotten), &forgotten);
    assert_ne!(
        events,
        forgotten.text("events.jsonl"),
        "a machine rebuilt from the journals alone continued as if nothing were lost"
    );
    saved
}

fn watchdog_resets_with_kernel_evidence() -> ScriptedMachine {
    ScriptedMachine::new(
        ScriptedTopology::FourCores,
        TargetOffset::ZERO,
        |spec, _| match reset_schedule(spec) {
            Some((at, mce)) => TrialOutcome::ResetWithMce {
                mce,
                stalled: None,
                kind: ResetKind::Watchdog,
                at,
            },
            None => TrialOutcome::Pass,
        },
    )
}

fn keeps_pending_machine_checks(saved: &serde_json::Value) {
    assert!(
        saved["boots"]
            .as_array()
            .unwrap()
            .last()
            .is_some_and(|boot| !boot["mces"].as_array().unwrap().is_empty()),
        "{saved}"
    );
}

#[test]
fn a_capped_simulation_continues_in_another_process_as_if_uninterrupted() {
    let saved = split_matches_whole(MemFs::new, watchdog_resets(), 1, 3);
    let last = saved["boots"].as_array().unwrap().last().unwrap();
    assert!(last["reason"]["kind"].is_string(), "{saved}");
}

#[test]
fn a_capped_simulation_keeps_its_pending_machine_checks_for_the_next_process() {
    keeps_pending_machine_checks(&split_matches_whole(
        MemFs::new,
        watchdog_resets_with_kernel_evidence(),
        1,
        4,
    ));
}

#[test]
fn a_continuation_on_another_machine_is_refused_and_keeps_the_saved_machine() {
    let script = watchdog_resets();
    let whole = MemFs::new();
    to_the_end(scripted_simulator(script, 1), &whole);

    let split = MemFs::new();
    let saved = capped(script, 1, &split, 3);
    let journaled = split.text("events.jsonl");
    // Four cores is the least a Ruleset 13 night runs on, and the simulator itself refuses a machine with another
    // core count before the session sees a changed topology.
    let Err(ResumeError::Restore(RestoreError::Cores {
        persisted: 4,
        script: 8,
    })) = resume_simulator(
        &split.persistence(),
        passing(ScriptedTopology::EightCores),
        1,
    )
    else {
        panic!("a machine with other cores was resumed");
    };
    let kept: serde_json::Value = serde_json::from_slice(&split.bytes(MACHINE).unwrap()).unwrap();
    assert_eq!(kept, saved);
    assert_eq!(split.text("events.jsonl"), journaled);

    to_the_end(resumed(script, 1, &split), &split);
    assert_eq!(whole.text("events.jsonl"), split.text("events.jsonl"));
}

fn unreadable_machine(store: &impl Store) {
    let Err(ResumeError::Read { path, .. }) =
        resume_simulator(&store.persistence(), watchdog_resets(), 1)
    else {
        panic!("an unreadable machine was not a read failure");
    };
    assert_eq!(path, store.path(MACHINE));
}

#[test]
fn a_machine_that_cannot_be_read_stops_the_continuation() {
    let store = MemFs::new();
    capped(watchdog_resets(), 1, &store, 3);
    let saved = store.bytes(MACHINE).unwrap();
    store.inject(Fault::before(Op::Read, 1).named(MACHINE));
    unreadable_machine(&store);
    assert_eq!(store.bytes(MACHINE), Some(saved));
}

#[test]
fn a_machine_path_that_is_a_directory_stops_the_continuation() {
    let store = MemFs::new();
    Fs::create_dir(&store, Path::new(STATE)).unwrap();
    Fs::create_dir(&store, &store.path(MACHINE)).unwrap();
    unreadable_machine(&store);
    assert!(store.bytes(MACHINE).is_none());
}

#[test]
fn a_continuation_that_cannot_remove_the_superseded_machine_fails() {
    let script = watchdog_resets();
    let store = MemFs::new();
    capped(script, 1, &store, 3);
    let saved = store.bytes(MACHINE).unwrap();
    let journaled = store.text("events.jsonl").lines().count();
    let sim = resumed(script, 1, &store);
    store.inject(Fault::before(Op::Remove, 1).named(MACHINE));

    let Err(SimError::Machine { path, .. }) = simulate(night_input(sim, store.persistence()))
    else {
        panic!("a kept superseded machine was not a machine failure");
    };
    assert_eq!(path, store.path(MACHINE));
    assert!(
        store.text("events.jsonl").lines().count() > journaled,
        "the continuation appended before it removed the machine"
    );
    assert_eq!(store.bytes(MACHINE), Some(saved));
}

#[test]
fn a_continuation_that_fails_after_a_warm_crash_still_closes_the_journal() {
    let script = watchdog_resets();
    let store = MemFs::new();
    capped(script, 1, &store, 3);
    let before = store.bytes("events.jsonl");
    let reference = MemFs::new();
    capped(script, 1, &reference, 3);
    let mut one_boot = night_input(resumed(script, 1, &reference), reference.persistence());
    one_boot.max_boots = nz(1);
    assert!(matches!(
        simulate(one_boot).unwrap(),
        SimOutcome::Capped { .. }
    ));

    let sim = resumed(script, 1, &store);
    store.inject(Fault::before(Op::Remove, 1).named(MACHINE));
    assert!(matches!(
        simulate(night_input(sim, store.persistence())),
        Err(SimError::Machine { .. })
    ));

    assert_ne!(store.bytes("events.jsonl"), before);
    assert_eq!(store.bytes("events.jsonl"), reference.bytes("events.jsonl"));
}
