use std::collections::BTreeMap;
use std::fs;
use std::num::NonZeroU32;
use std::path::Path;

use jiff::tz::TimeZone;
use togi_config::Config;
use togi_journal::{Build, Event, FixStamp, MemFs, SCHEMA};
use togi_machine::{CancellationToken, Regime, Signal};
use togi_render::Renderer;
use togi_session::{ConfigSource, StopPolicy};
use togi_sim::definition::{CcdDef, CoreDef, JointDef, ModelDef, Weights};
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};
use togi_simrun::{Persistence, Replay, SimInput, SimOutcome, events, simulate};
use togi_tuner::{EVIDENCE_EPOCH, RULESET};

fn build() -> Build {
    Build {
        version: "0.0.0".into(),
        rev: None,
        ruleset: RULESET,
        schema: SCHEMA,
        fixes: FixStamp::none(),
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

/// An eight-core machine, four cores a CCD. Core 0 holds -30 and the others -50, so core 0 tops CCD 0's order
/// and its first R7 partial idles it; idle, core 0 fails deeper than -20. The tuner derives non-empty
/// partials, backs off after the partial fails, reruns it and re-derives its chain.
fn chain_machine() -> MachineDefinition {
    let mut definition = hunting_machine();
    definition.cores = 8;
    definition.core = (0..8)
        .map(|id| {
            let limit = if id == 0 { -30 } else { -50 };
            CoreDef {
                id,
                alone: [limit; 5],
                together: [limit; 7],
                flat: 0.0,
                idle: (id == 0).then_some(-20),
                workload: BTreeMap::new(),
            }
        })
        .collect();
    definition.joint = Vec::new();
    definition
}

/// The events of a simulated session through its first clean cycle, over every reboot its crashes cause.
pub(crate) fn hunting_session() -> Vec<Event> {
    session(&hunting_machine(), 1, 1, Config::default())
}

/// The events of a simulated eight-core session through its first clean cycle, whose R7 chains load
/// partials and are re-derived after a backoff.
pub(crate) fn chain_session() -> Vec<Event> {
    session(&chain_machine(), 1, 1, Config::default())
}

/// The events of a simulated session of the seeded default machine with a profile-dependent R7 hazard on
/// each loaded CCD, 2 failures per hour at a mean CCD depth of 25 doubling every 5 counts deeper, through
/// two clean cycles: it fails and backs off R7 in phase 1, in phase 2 and after the conclusion.
pub(crate) fn hazard_session() -> Vec<Event> {
    let definition = MachineDefinition {
        ccd: Some(CcdDef {
            log_rate: (2.0_f64 / 3600.0).ln(),
            slope: 2.0_f64.ln() / 5.0,
            ..CcdDef::default()
        }),
        ..MachineDefinition::default()
    };
    session(&definition, 3, 2, Config::default())
}

/// The committed bench machines the counterfactual tests simulate, embedded so no test reads them from disk.
pub(crate) const TARGET_FIT_0: &str =
    include_str!("../../../../tools/bench/machines/target-fit-0.json");
pub(crate) const TARGET_FIT_1: &str =
    include_str!("../../../../tools/bench/machines/target-fit-1.json");
pub(crate) const TARGET_FIT_2: &str =
    include_str!("../../../../tools/bench/machines/target-fit-2.json");

/// The events of a simulated session of the bench machine definition `machine` drawn from `seed`, through
/// its first clean cycle.
pub(crate) fn machine_session(machine: &str, seed: u64) -> Vec<Event> {
    machine_session_with(machine, seed, Config::default())
}

/// [`machine_session`] under `config`.
pub(crate) fn machine_session_with(machine: &str, seed: u64, config: Config) -> Vec<Event> {
    session(&togi_sim::file::decode(machine).unwrap(), seed, 1, config)
}

fn session(definition: &MachineDefinition, seed: u64, cycles: u32, config: Config) -> Vec<Event> {
    let simulator = Simulator::new(
        Model::new(definition, seed).unwrap(),
        RunSettings {
            seed,
            ..RunSettings::default()
        },
    );
    let fs = MemFs::new();
    let outcome = simulate(SimInput {
        config,
        source: ConfigSource::default(),
        build: build(),
        simulator,
        persistence: Persistence::Memory(fs.clone()),
        replay: Replay::Warm,
        verify_every: None,
        max_boots: NonZeroU32::new(1000).unwrap(),
        policy: StopPolicy {
            cycles: NonZeroU32::new(cycles),
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
