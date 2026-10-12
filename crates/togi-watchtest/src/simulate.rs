use std::collections::BTreeMap;
use std::num::NonZeroU32;

use jiff::tz::TimeZone;
use togi_config::{Config, DEFAULT_PATH};
use togi_journal::{Build, Event, MemFs, SCHEMA};
use togi_machine::{CancellationToken, CoreId, Regime, Signal};
use togi_render::Renderer;
use togi_session::{ConfigSource, StopPolicy};
use togi_sim::definition::{CoreDef, JointDef, ModelDef, Weights};
use togi_sim::model::Script;
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};
use togi_simrun::{Observe, Observer, Persistence, Replay, SimInput, SimOutcome};
use togi_tuner::{EVIDENCE_EPOCH, RULESET};

use crate::Error;

const MAX_BOOTS: NonZeroU32 = NonZeroU32::MIN.saturating_add(999);

/// When a simulated session stops on its own.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stop {
    /// After this many clean cycles once phase 2 has concluded.
    Cycles(NonZeroU32),
    /// At the first confirmed BIOS profile.
    FirstResult,
}

impl Stop {
    /// The clean-cycle count, which must not be zero: a session told to stop after no cycles never would.
    pub const fn cycles(count: u32) -> Self {
        match NonZeroU32::new(count) {
            Some(count) => Self::Cycles(count),
            None => panic!("a simulated session stops after at least one cycle"),
        }
    }
}

/// The build every simulated journal names, whatever release this crate is compiled into: version `0.0.0`, no
/// revision, and the tuner's current ruleset and evidence epoch.
pub fn build() -> Build {
    Build {
        version: "0.0.0".into(),
        rev: None,
        ruleset: RULESET,
        schema: SCHEMA,
        fixes: 0,
        evidence: EVIDENCE_EPOCH,
    }
}

/// A seeded simulated machine of `definition`.
pub fn machine(definition: &MachineDefinition, seed: u64) -> Result<Simulator, Error> {
    Ok(Simulator::new(
        Model::new(definition, seed)?,
        RunSettings {
            seed,
            ..RunSettings::default()
        },
    ))
}

/// The default sixteen-core machine, its limits drawn from `seed`.
pub fn default_machine(seed: u64) -> Result<Simulator, Error> {
    machine(&MachineDefinition::default(), seed)
}

/// Stops a simulation after the first appended event it reports true for.
pub type Until = Box<dyn FnMut(&Event) -> bool>;

/// Runs a simulated session through `stop` or until `until` reports true for an appended event, and returns its
/// journal at the current ruleset. Its `config.loaded` names [`build`], so a release's version bump leaves the
/// journal unchanged. An `until` stop is a signal: the journal ends with the shutdown it records. Without `until` no
/// observer runs.
pub fn simulate(
    machine: Simulator,
    config: Config,
    until: Option<Until>,
    stop: Stop,
) -> Result<Vec<Event>, Error> {
    let persistence = Persistence::Memory(MemFs::new());
    let (cycles, first_result) = match stop {
        Stop::Cycles(cycles) => (Some(cycles), false),
        Stop::FirstResult => (None, true),
    };
    let outcome = togi_simrun::simulate(SimInput {
        config,
        source: ConfigSource {
            path: DEFAULT_PATH.to_owned(),
            file: false,
        },
        build: build(),
        simulator: machine,
        persistence: persistence.clone(),
        replay: Replay::Warm,
        verify_every: None,
        max_boots: MAX_BOOTS,
        policy: StopPolicy {
            cycles,
            first_result,
        },
        write_samples: false,
        defects: &[],
        log: None,
        renderer: Renderer::default(),
        zone: TimeZone::UTC,
        cancel: CancellationToken::new(),
        observer: until.map(|mut until| -> Observer {
            Box::new(move |event| {
                if until(event) {
                    Observe::Stop
                } else {
                    Observe::Continue
                }
            })
        }),
        dump_to: None,
    })?;
    match outcome {
        SimOutcome::Stopped(finished) if finished.errors.is_empty() => {}
        SimOutcome::Stopped(finished) => return Err(Error::Session(finished.errors)),
        SimOutcome::Capped { boots } => return Err(Error::Capped(boots)),
    }
    Ok(togi_simrun::events(&persistence)?)
}

/// Pins trial `trial`'s outcome to a crash `at_s` seconds in.
pub fn crash_trial(machine: &Simulator, trial: &str, at_s: f64, core: u8) -> Result<(), Error> {
    machine.script_trial(
        trial,
        Script {
            signal: Some(Signal::Crash),
            at_s,
            core: CoreId::new(i64::from(core))
                .map_err(|error| Error::Fixture(error.to_string()))?,
            reset: None,
            then_crash: false,
        },
    )?;
    Ok(())
}

/// A sixteen-core machine that fails together only through a combination of cores 03 and 11 in R6, where a failure
/// naming no core still starts a hunt, so its hunt probes members; its configuration proposes every core's solo
/// limit at -11.
pub fn probe_machine() -> Result<(Simulator, Config), Error> {
    let limits = (0..16)
        .map(|id| CoreDef {
            id,
            alone: [-50; 5],
            together: [-50; 7],
            flat: 0.0,
            idle: None,
            workload: BTreeMap::new(),
        })
        .collect();
    let definition = MachineDefinition {
        cores: 16,
        core: limits,
        model: ModelDef {
            past_limit_rate: 1.0,
            signals: Weights::from([(Signal::Crash, 1.0)]),
            crash_mce: 0.0,
            ..ModelDef::default()
        },
        joint: vec![JointDef {
            members: [(3, -10), (11, -10)].into(),
            regimes: vec![Regime::R6],
            rate: Some(10.0),
            ..JointDef::default()
        }],
        ..MachineDefinition::default()
    };
    let solo_limits = (0..16)
        .map(|core| format!("\"{core}\":-11"))
        .collect::<Vec<_>>()
        .join(",");
    let config = Config::parse(&format!("{{\"candidate_solo_limits\":{{{solo_limits}}}}}"))?;
    Ok((machine(&definition, 1)?, config))
}
