use std::num::NonZeroU32;

use jiff::tz::TimeZone;
use togi_config::Config;
use togi_journal::{Build, Event, FixStamp, MemFs, SCHEMA};
use togi_machine::CancellationToken;
use togi_render::Renderer;
use togi_session::{ConfigSource, StopPolicy};
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
        policy: StopPolicy {
            cycles: NonZeroU32::new(1),
            first_result: false,
        },
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
