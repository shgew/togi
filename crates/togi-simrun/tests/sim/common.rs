use std::num::NonZeroU32;
use std::path::PathBuf;

use jiff::tz::TimeZone;
use togi_config::Config;
use togi_journal::{Build, MemFs, SCHEMA};
use togi_machine::CancellationToken;
use togi_render::Renderer;
use togi_session::{ConfigSource, StopPolicy};
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator, file};
use togi_simrun::{Persistence, Replay, SimInput};

pub(crate) const STATE: &str = "/state";

pub(crate) fn build() -> Build {
    Build {
        version: "0.0.0".into(),
        rev: None,
        ruleset: 1,
        schema: SCHEMA,
        fixes: 0,
        evidence: 1,
    }
}

pub(crate) fn machine_file(name: &str) -> MachineDefinition {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/bench/machines")
        .join(format!("{name}.json"));
    file::load(path).unwrap()
}

pub(crate) fn simulator(definition: &MachineDefinition, seed: u64) -> Simulator {
    Simulator::new(
        Model::new(definition, seed).unwrap(),
        RunSettings {
            seed,
            ..RunSettings::default()
        },
    )
}

pub(crate) fn nz(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).unwrap()
}

pub(crate) fn input(simulator: Simulator, persistence: Persistence) -> SimInput {
    SimInput {
        config: Config::default(),
        source: ConfigSource::default(),
        build: build(),
        simulator,
        persistence,
        replay: Replay::Warm,
        verify_every: Some(nz(1)),
        max_boots: nz(1000),
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
        observer: None,
        dump_to: None,
    }
}

pub(crate) fn state_json(fs: &MemFs) -> Option<Vec<u8>> {
    fs.contents(format!("{STATE}/state.json"))
}
