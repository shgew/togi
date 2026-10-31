use std::num::NonZeroU32;

use jiff::SignedDuration;
use jiff::tz::TimeZone;
use togi_config::{Config, DEFAULT_PATH};
use togi_journal::{Build, Event, FixStamp, MemFs, SCHEMA};
use togi_machine::{CancellationToken, CoreId, Signal};
use togi_render::Renderer;
use togi_session::ConfigSource;
use togi_sim::model::Script;
use togi_sim::{MachineDefinition, Model, RunSettings, Simulator};
use togi_simrun::{Observe, Observer, Persistence, Replay, SimInput, SimOutcome};
use togi_tuner::{EVIDENCE_EPOCH, RULESET};

use crate::Error;

const MAX_BOOTS: NonZeroU32 = NonZeroU32::MIN.saturating_add(999);

/// The build every simulated journal names, whatever release this crate is compiled into: version `0.0.0`, no
/// revision, and the tuner's current ruleset and evidence epoch.
pub fn build() -> Build {
    Build {
        version: "0.0.0".into(),
        rev: None,
        ruleset: RULESET,
        schema: SCHEMA,
        fixes: FixStamp::none(),
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
    )?)
}

/// The default sixteen-core machine, its limits drawn from `seed`.
pub fn default_machine(seed: u64) -> Result<Simulator, Error> {
    machine(&MachineDefinition::default(), seed)
}

/// Stops a simulation after the first appended event it reports true for.
pub type Halt = Box<dyn FnMut(&Event) -> bool>;

/// Runs a simulated session through one night of length `night` and returns its journal at the current ruleset. The
/// night has no deadline: the simulated operator ends it like a signal at the first appended event `night` or more
/// after the simulated clock's start, interrupting a trial that is running then. The session ends it earlier at a dead
/// end or once every backend is unavailable, and `halt` stops it, also like a signal, after the first appended event
/// it reports true for. Its `config.loaded` names [`build`], so a release's version bump leaves the journal unchanged.
pub fn simulate(
    machine: Simulator,
    config: Config,
    halt: Option<Halt>,
    night: SignedDuration,
) -> Result<Vec<Event>, Error> {
    let persistence = Persistence::Memory(MemFs::new());
    let stop_at = machine.now().checked_add(night)?;
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
        stop_at: Some(stop_at),
        write_samples: false,
        defects: togi_defect::KNOWN_DEFECTS,
        log: None,
        renderer: Renderer::default(),
        zone: TimeZone::UTC,
        cancel: CancellationToken::new(),
        observer: halt.map(|mut halt| -> Observer {
            Box::new(move |event| {
                if halt(event) {
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
pub fn crash_trial(machine: &Simulator, trial: &str, at_s: f64, core: CoreId) -> Result<(), Error> {
    machine.script_trial(
        trial,
        Script {
            signal: Some(Signal::Crash),
            at_s,
            core,
            reset: None,
            then_crash: false,
        },
    )?;
    Ok(())
}
