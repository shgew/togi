use std::num::NonZeroU32;

use jiff::SignedDuration;
use jiff::tz::TimeZone;
use togi_config::Config;
use togi_journal::{Build, Event, FixStamp, MemFs, SCHEMA};
use togi_machine::CancellationToken;
use togi_render::Renderer;
use togi_session::ConfigSource;
use togi_sim::replay::Replay;
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};
use togi_simrun::{Persistence, SimInput, SimOutcome, events, simulate};

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
        observer: None,
        dump_to: None,
    })
    .unwrap();
    Session {
        events: events(&persistence).unwrap(),
        outcome,
    }
}

/// Four cores that crash once a trial pushes them past their limits at -10: a Ruleset 13 night needs a neighbour on
/// every CCD, so four cores, and it crashes four times, so a session of fewer boots is capped.
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
