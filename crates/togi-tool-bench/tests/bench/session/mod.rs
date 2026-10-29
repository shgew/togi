use std::num::NonZeroU32;

use jiff::SignedDuration;
use jiff::tz::TimeZone;
use togi_config::Config;
use togi_journal::{Build, Event, FixStamp, MemFs, Payload, SCHEMA, ShutdownReason};
use togi_machine::CancellationToken;
use togi_render::Renderer;
use togi_session::ConfigSource;
use togi_sim::replay::Replay;
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};
use togi_simrun::{Observe, Observer, Persistence, SimInput, SimOutcome, events, simulate};

/// A whole simulated session and the journal it left, kept in memory.
pub(crate) struct Session {
    pub events: Vec<Event>,
    pub outcome: SimOutcome,
}

pub(crate) fn session(
    definition: &MachineDefinition,
    seed: u64,
    replay: Option<Replay>,
    max_boots: u32,
) -> Session {
    session_with(definition, seed, replay, max_boots, None)
}

fn session_with(
    definition: &MachineDefinition,
    seed: u64,
    replay: Option<Replay>,
    max_boots: u32,
    observer: Option<Observer>,
) -> Session {
    let simulator = Simulator::new(
        Model::new(definition, seed).unwrap(),
        RunSettings {
            seed,
            replay,
            ..RunSettings::default()
        },
    )
    .unwrap();
    let night_end = simulator.now() + SignedDuration::from_hours(8);
    let persistence = Persistence::Memory(MemFs::new());
    let outcome = simulate(SimInput {
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
        persistence: persistence.clone(),
        replay: togi_simrun::Replay::Warm,
        verify_every: None,
        max_boots: NonZeroU32::new(max_boots).unwrap(),
        stop_at: Some(night_end),
        write_samples: false,
        defects: &[],
        log: None,
        renderer: Renderer::default(),
        zone: TimeZone::UTC,
        cancel: CancellationToken::new(),
        observer,
        dump_to: None,
    })
    .unwrap();
    Session {
        events: events(&persistence).unwrap(),
        outcome,
    }
}

/// A short real trial history for fake commands that finish successfully.
/// Command completion, unlike an early signal, is a deterministic, cacheable outcome.
pub(crate) fn concluded() -> Vec<Event> {
    let mut events = brief(&MachineDefinition::default(), 1).events;
    let Payload::Shutdown(shutdown) = &mut events.last_mut().unwrap().payload else {
        panic!("the short night must shut down");
    };
    assert_eq!(shutdown.reason, ShutdownReason::Signal);
    shutdown.reason = ShutdownReason::Command;
    events
}

/// The same valid recording without its terminal shutdown, for censored/termination checks.
pub(crate) fn unfinished() -> Vec<Event> {
    let mut events = concluded();
    events.pop().unwrap();
    events
}

/// Stop after one ended trial: the measurement and process contracts do not need a whole night.
pub(crate) fn brief(definition: &MachineDefinition, seed: u64) -> Session {
    session_with(
        definition,
        seed,
        None,
        1000,
        Some(Box::new(|event| {
            if matches!(event.payload, Payload::TrialEnd(_)) {
                Observe::Stop
            } else {
                Observe::Continue
            }
        })),
    )
}

/// Four cores that crash beyond -10. Two boot attempts interrupt the bounded campaign before its final recovery.
pub(crate) fn hunting() -> MachineDefinition {
    serde_json::from_str(
        r#"{
          "cores": 4,
          "model": {"past_limit_rate": 1.0, "signals": {"crash": 1}, "crash_mce": 0.0},
          "core": [
            {"id": 0, "alone": [-10, -10, -10, -10, -10], "together": [-10, -10, -10, -10, -10, -10, -50]},
            {"id": 1, "alone": [-10, -10, -10, -10, -10], "together": [-10, -10, -10, -10, -10, -10, -50]},
            {"id": 2, "alone": [-10, -10, -10, -10, -10], "together": [-10, -10, -10, -10, -10, -10, -50]},
            {"id": 3, "alone": [-10, -10, -10, -10, -10], "together": [-10, -10, -10, -10, -10, -10, -50]}
          ]
        }"#,
    )
    .unwrap()
}
