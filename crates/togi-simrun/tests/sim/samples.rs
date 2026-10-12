use togi_journal::{Event, EventKind, Payload, SessionId, TrialEnd};
use togi_machine::BiosContext;
use togi_simrun::{Persistence, events, simulate};

use crate::common::{hunt_machine, input, simulator};

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

#[test]
fn a_new_sessions_trial_gets_no_samples_from_an_earlier_invocation() {
    let dir = tempfile::tempdir().unwrap();
    let persistence = Persistence::Files(dir.path().to_path_buf());
    let sim = simulator(&hunt_machine(4), 1);
    let run = || {
        let mut input = input(sim.clone(), persistence.clone());
        input.write_samples = true;
        input.verify_every = None;
        simulate(input).unwrap();
    };
    run();
    let first = events(&persistence).unwrap();
    assert!(trial_end(&first, "0001").is_some());
    assert!(
        sim.seams().trials.samples("0001").next().is_some(),
        "the first session's trial 0001 kept no samples to leak"
    );

    sim.set_bios_context(BiosContext {
        bios_version: "new".into(),
        board: "sim".into(),
        cpu_model: "sim".into(),
        microcode: "0x2".into(),
        boost_limit_mhz: 5500,
    });
    sim.reboot();
    sim.crash_before_apply(1);
    run();

    let second = events(&persistence).unwrap();
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
    let archived = dir
        .path()
        .join("archive")
        .join(format!("{}-trials", session_id(&first)))
        .join("0001");
    assert!(
        togi_machine::read_samples(&archived).next().is_some(),
        "the earlier session's samples of trial 0001 are not in its archive at {}",
        archived.display()
    );
}
