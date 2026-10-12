use std::collections::BTreeMap;
use std::fs;
use std::num::NonZeroU32;
use std::path::Path;

use jiff::tz::TimeZone;
use togi_config::Config;
use togi_journal::{Build, Event, MemFs, SCHEMA};
use togi_machine::{CancellationToken, Regime, Signal};
use togi_render::Renderer;
use togi_session::{ConfigSource, StopPolicy};
use togi_sim::definition::{CoreDef, JointDef, ModelDef, Weights};
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};
use togi_simrun::{Persistence, Replay, SimInput, SimOutcome, events, simulate};
use togi_tuner::{EVIDENCE_EPOCH, RULESET};

fn build() -> Build {
    Build {
        version: "0.0.0".into(),
        rev: None,
        ruleset: RULESET,
        schema: SCHEMA,
        fixes: 0,
        evidence: EVIDENCE_EPOCH,
    }
}

/// A four-core machine where every core holds -10 alone and together, and cores 1 and 3 together crash from
/// -10: the tuner finds the pair by a hunt after the first joint crash and reboots through each crash.
fn hunting_machine() -> MachineDefinition {
    let cores = (0..4)
        .map(|id| CoreDef {
            id,
            alone: [-50; 5],
            together: [-50; 7],
            flat: 0.0,
            idle: None,
            workload: BTreeMap::new(),
        })
        .collect();
    MachineDefinition {
        cores: 4,
        core: cores,
        model: ModelDef {
            past_limit_rate: 1.0,
            signals: Weights::from([(Signal::Crash, 1.0)]),
            crash_mce: 0.0,
            ..ModelDef::default()
        },
        joint: vec![JointDef {
            members: [(1, -10), (3, -10)].into_iter().collect(),
            regimes: vec![Regime::R6],
            rate: Some(10.0),
            ..JointDef::default()
        }],
        ..MachineDefinition::default()
    }
}

/// The events of a simulated session through its first clean cycle, over every reboot its crashes cause.
pub(crate) fn hunting_session() -> Vec<Event> {
    let definition = hunting_machine();
    let simulator = Simulator::new(
        Model::new(&definition, 1).unwrap(),
        RunSettings {
            seed: 1,
            ..RunSettings::default()
        },
    );
    let fs = MemFs::new();
    let outcome = simulate(SimInput {
        config: Config::default(),
        source: ConfigSource::default(),
        build: build(),
        simulator,
        persistence: Persistence::Memory(fs.clone()),
        replay: Replay::Warm,
        verify_every: None,
        max_boots: NonZeroU32::new(1000).unwrap(),
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
    assert!(matches!(outcome, SimOutcome::Stopped(_)));
    events(&Persistence::Memory(fs)).unwrap()
}

/// `events.jsonl` of `events` in `dir`, one encoded event a line.
pub(crate) fn write_journal(dir: &Path, events: &[Event]) {
    let mut text = String::new();
    for event in events {
        text.push_str(&event.encode().unwrap());
        text.push('\n');
    }
    fs::write(dir.join("events.jsonl"), text).unwrap();
}
