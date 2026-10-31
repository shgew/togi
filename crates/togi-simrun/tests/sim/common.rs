use std::collections::BTreeMap;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use jiff::SignedDuration;
use jiff::tz::TimeZone;
use togi_config::Config;
use togi_journal::fs::FileKind;
use togi_journal::{Build, Event, EventKind, FixStamp, MemFs, SCHEMA, StateDir};
use togi_machine::{CancellationToken, Signal};
use togi_render::Renderer;
use togi_session::ConfigSource;
use togi_sim::definition::{CoreDef, ModelDef, Weights};
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};
use togi_simrun::{
    Finished, Observe, Observer, Persistence, Replay, SimInput, SimOutcome, events, simulate,
};

pub(crate) const STATE: &str = "/state";

/// How long the simulated operator lets a night run before ending it: long enough for mapping and the first crash
/// on the default machine.
pub(crate) const NIGHT: SignedDuration = SignedDuration::from_hours(24);

pub(crate) fn build() -> Build {
    Build {
        version: "0.0.0".into(),
        rev: None,
        ruleset: 1,
        schema: SCHEMA,
        fixes: FixStamp::none(),
        evidence: 1,
    }
}

struct Machines;

impl togi_sim::file::Source for Machines {
    fn read_to_string(&self, path: &Path) -> std::io::Result<String> {
        let text = match path.file_name().and_then(|name| name.to_str()) {
            Some("shared-voltage.json") => {
                include_str!("../../../../tools/bench/machines/shared-voltage.json")
            }
            Some("target-fit-0.json") => {
                include_str!("../../../../tools/bench/machines/target-fit-0.json")
            }
            Some("flat-hazard.json") => {
                include_str!("../../../../tools/bench/machines/flat-hazard.json")
            }
            Some("target-shared-voltage.json") => {
                include_str!("../../../../tools/bench/machines/target-shared-voltage.json")
            }
            Some("target-r7-request-gap.json") => {
                include_str!("../../../../tools/bench/machines/target-r7-request-gap.json")
            }
            Some("misleading-mce.json") => {
                include_str!("../../../../tools/bench/machines/misleading-mce.json")
            }
            Some("late-onset.json") => {
                include_str!("../../../../tools/bench/machines/late-onset.json")
            }
            _ => return Err(std::io::ErrorKind::NotFound.into()),
        };
        Ok(text.to_owned())
    }

    fn canonicalize(&self, path: &Path) -> std::io::Result<PathBuf> {
        Ok(path.to_owned())
    }
}

pub(crate) fn machine_file(name: &str) -> MachineDefinition {
    togi_sim::file::load_from(format!("/machines/{name}.json"), &Machines)
        .unwrap()
        .0
}

pub(crate) fn simulator(definition: &MachineDefinition, seed: u64) -> Simulator {
    Simulator::new(
        Model::new(definition, seed).unwrap(),
        RunSettings {
            seed,
            ..RunSettings::default()
        },
    )
    .unwrap()
}

pub(crate) fn nz(n: u32) -> NonZeroU32 {
    NonZeroU32::new(n).unwrap()
}

pub(crate) fn input(simulator: Simulator, persistence: Persistence) -> SimInput {
    let stop_at = Some(simulator.now() + NIGHT);
    SimInput {
        config: Config::default(),
        source: ConfigSource::default(),
        build: build(),
        simulator,
        persistence,
        replay: Replay::Warm,
        verify_every: Some(nz(1)),
        max_boots: nz(1000),
        stop_at,
        write_samples: false,
        defects: togi_defect::KNOWN_DEFECTS,
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

/// A simulated machine and the in-memory state directory its simulation keeps across runs.
pub(crate) struct Bench {
    pub sim: Simulator,
    pub fs: MemFs,
}

impl Bench {
    pub(crate) fn new(definition: &MachineDefinition, seed: u64) -> Self {
        Self {
            sim: simulator(definition, seed),
            fs: MemFs::new(),
        }
    }

    pub(crate) fn persistence(&self) -> Persistence {
        Persistence::Memory(self.fs.clone())
    }

    pub(crate) fn input(&self) -> SimInput {
        input(self.sim.clone(), self.persistence())
    }

    /// The same state directory on a machine built for `definition`, continuing after what the journals recorded.
    pub(crate) fn resumed(&self, definition: &MachineDefinition, seed: u64) -> Self {
        let history = togi_simrun::history_in(&self.dir()).unwrap();
        Self {
            sim: Simulator::new(
                Model::new(definition, seed).unwrap(),
                RunSettings {
                    seed,
                    history,
                    replay: None,
                },
            )
            .unwrap(),
            fs: self.fs.clone(),
        }
    }

    pub(crate) fn events(&self) -> Vec<Event> {
        events(&self.persistence()).unwrap()
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "tests read an in-memory state directory"
    )]
    pub(crate) fn dir(&self) -> StateDir {
        StateDir::simulated(self.fs.clone()).unwrap()
    }

    pub(crate) fn recorded_config(&self, fresh: Config) -> Config {
        togi_simrun::recorded_config(&self.persistence(), fresh).unwrap()
    }

    /// A copy of the state directory with its own filesystem, for resuming the same history twice.
    pub(crate) fn fork(&self, definition: &MachineDefinition, seed: u64) -> Self {
        let copy = MemFs::new();
        for (path, kind) in self.fs.entries() {
            if kind == FileKind::File {
                copy.put(&path, self.fs.contents(&path).unwrap());
            }
        }
        Self {
            sim: self.sim.clone(),
            fs: copy,
        }
        .resumed(definition, seed)
    }
}

/// Runs to the end of the simulation and returns how its last run ended; a capped simulation is a failure.
pub(crate) fn finish(input: SimInput) -> Finished {
    match simulate(input).unwrap() {
        SimOutcome::Stopped(finished) => *finished,
        SimOutcome::Capped { boots } => panic!("capped after {boots} boots"),
    }
}

/// Stops the run after the first event of `kind`.
pub(crate) fn until_kind(kind: EventKind) -> Observer {
    Box::new(move |event| {
        if event.kind() == kind {
            Observe::Stop
        } else {
            Observe::Continue
        }
    })
}

/// Crashes the machine once, at the `nth` (1 is the first) appended event of `kind`.
pub(crate) fn crash_at_nth(sim: &Simulator, kind: EventKind, nth: usize) -> Observer {
    let sim = sim.clone();
    let mut seen = 0;
    Box::new(move |event| {
        if event.kind() == kind {
            seen += 1;
            if seen == nth {
                sim.crash();
            }
        }
        Observe::Continue
    })
}

/// Crashes the machine at each of the first `count` appended events of `kind`, so each of the first boots crashes.
pub(crate) fn crash_first(sim: &Simulator, kind: EventKind, count: usize) -> Observer {
    let sim = sim.clone();
    let mut seen = 0;
    Box::new(move |event| {
        if event.kind() == kind && seen < count {
            seen += 1;
            sim.crash();
        }
        Observe::Continue
    })
}

/// Every core alone and together at -10, nothing else failing but past-limit crashes at rate 1. The machine's cores
/// split in halves into two CCDs, and a Ruleset 13 night needs a neighbour on every CCD, so it has at least four.
pub(crate) fn hunt_machine(cores: i64) -> MachineDefinition {
    let cores = cores.max(4);
    let limits = (0..cores)
        .map(|id| CoreDef {
            id,
            alone: [-10; 5],
            together: [-10, -10, -10, -10, -10, -10, -50],
            flat: 0.0,
            idle: None,
            workload: BTreeMap::new(),
        })
        .collect();
    MachineDefinition {
        cores,
        core: limits,
        model: ModelDef {
            past_limit_rate: 1.0,
            signals: Weights::from([(Signal::Crash, 1.0)]),
            crash_mce: 0.0,
            ..ModelDef::default()
        },
        ..MachineDefinition::default()
    }
}
