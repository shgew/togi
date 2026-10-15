use togi_journal::{Event, EventKind, MemFs, Payload, SessionId, TrialEnd};
use togi_machine::{BiosContext, TrialConditions};
use togi_simrun::{Observer, Persistence, events, simulate};

use crate::common::{hunt_machine, input, simulator, until_kind};

fn trial_end<'a>(events: &'a [Event], trial: &str) -> Option<&'a TrialEnd> {
    events.iter().find_map(|event| match &event.payload {
        Payload::TrialEnd(end) if end.trial.as_str() == trial => Some(end),
        _ => None,
    })
}

fn session_id(events: &[Event]) -> &SessionId {
    events
        .iter()
        .find_map(|event| match &event.payload {
            Payload::SessionStart(start) => Some(&start.session),
            _ => None,
        })
        .unwrap()
}

/// Runs a session, to its end or, with `observer`, until it stops, changes the BIOS so the next invocation starts
/// a new session, and crashes that session's trial 0001 before it starts. Returns both sessions' events and
/// the first session's trial 0001 samples.
fn a_new_session_after_an_earlier_one(
    persistence: &Persistence,
    restore: bool,
    write_samples: bool,
    observer: Option<Observer>,
) -> (Vec<Event>, Vec<Event>, Vec<TrialConditions>) {
    let mut sim = simulator(&hunt_machine(4), 1);
    let run = |sim: &togi_sim::Simulator, observer: Option<Observer>| {
        let mut input = input(sim.clone(), persistence.clone());
        input.write_samples = write_samples;
        input.verify_every = None;
        input.observer = observer;
        simulate(input).unwrap();
    };
    run(&sim, observer);
    let first = events(persistence).unwrap();
    assert!(trial_end(&first, "0001").is_some());
    let samples: Vec<_> = sim.seams().trials.samples("0001").collect();
    assert!(
        !samples.is_empty(),
        "the first session's trial 0001 kept no samples to leak"
    );
    if restore {
        let saved = serde_json::to_vec(&sim.persisted()).unwrap();
        sim = togi_sim::Simulator::restore(
            togi_sim::Model::new(&hunt_machine(4), 1).unwrap(),
            None,
            serde_json::from_slice(&saved).unwrap(),
        )
        .unwrap();
        assert!(sim.seams().trials.samples("0001").next().is_some());
    }

    sim.set_bios_context(BiosContext {
        bios_version: "new".into(),
        board: "sim".into(),
        cpu_model: "sim".into(),
        microcode: "0x2".into(),
        boost_limit_mhz: 5500,
    });
    sim.reboot();
    sim.crash_before_apply(1);
    run(&sim, None);

    let second = events(persistence).unwrap();
    assert_ne!(session_id(&first), session_id(&second));
    let is_trial_0001 = |event: &Event| match &event.payload {
        Payload::TrialIntent(intent) => intent.trial.as_str() == "0001",
        Payload::TrialStart(start) => start.trial.as_str() == "0001",
        Payload::TrialEnd(end) => end.trial.as_str() == "0001",
        _ => false,
    };
    let kinds: Vec<EventKind> = second
        .iter()
        .filter(|event| is_trial_0001(event) || event.kind() == EventKind::CrashDetected)
        .map(Event::kind)
        .take(3)
        .collect();
    assert_eq!(
        kinds,
        [
            EventKind::TrialIntent,
            EventKind::CrashDetected,
            EventKind::TrialEnd
        ],
        "the new session's trial 0001 should crash before it starts"
    );
    let end = trial_end(&second, "0001").unwrap();
    assert_eq!(
        (end.last_sample_s, end.stalled_core, end.worker_stalled_ms),
        (None, None, None),
        "trial 0001 never ran but recorded the earlier session's samples: {end:?}"
    );
    (first, second, samples)
}

#[test]
fn a_new_sessions_trial_gets_no_samples_from_an_earlier_invocation() {
    a_new_session_after_an_earlier_one(
        &Persistence::Memory(MemFs::new()),
        true,
        false,
        Some(until_kind(EventKind::TrialEnd)),
    );
}

#[test]
fn a_new_in_memory_sessions_trial_gets_no_samples_from_an_earlier_one() {
    a_new_session_after_an_earlier_one(
        &Persistence::Memory(MemFs::new()),
        false,
        false,
        Some(until_kind(EventKind::TrialEnd)),
    );
}

#[test]
fn real_fs_a_new_sessions_trial_gets_no_serialized_samples_from_an_earlier_invocation() {
    let dir = tempfile::tempdir().unwrap();
    let (first, _, samples) = a_new_session_after_an_earlier_one(
        &Persistence::Files(dir.path().to_path_buf()),
        false,
        true,
        Some(until_kind(EventKind::TrialEnd)),
    );
    let archived = dir
        .path()
        .join("archive")
        .join(format!("{}-trials", session_id(&first)))
        .join("0001");
    assert!(archived.join("samples.jsonl.gz").is_file());
    assert!(!archived.join("samples.jsonl").exists());
    assert_eq!(
        togi_machine::read_samples(&archived).collect::<Vec<_>>(),
        samples,
        "the earlier session's compressed samples must remain intact after trial 0001 is reused"
    );
}
