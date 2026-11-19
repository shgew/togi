use std::cell::Cell;
use std::num::NonZeroU32;
use std::rc::Rc;

use jiff::tz::TimeZone;
use togi_config::{Config, DEFAULT_PATH};
use togi_journal::{Build, Event, FixStamp, SCHEMA};
use togi_machine::CancellationToken;
use togi_render::Renderer;
use togi_session::ConfigSource;
use togi_sim::Simulator;
use togi_simrun::{Observe, Observer, Persistence, Replay, SimInput, SimOutcome};
use togi_tuner::{EVIDENCE_EPOCH, RULESET};

use crate::Error;

const MAX_EVENTS: usize = 10_000;

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

/// Run a scripted night through the real session and end it by signal at the selected event, or
/// let it reach its own dead end. The event and boot limits fail the fixture if the intended
/// path regresses rather than silently returning a much longer journal.
pub(super) fn simulate(
    machine: Simulator,
    persistence: &Persistence,
    max_boots: NonZeroU32,
    mut halt: impl FnMut(&Event) -> bool + 'static,
) -> Result<Vec<Event>, Error> {
    let exceeded = Rc::new(Cell::new(false));
    let guard = Rc::clone(&exceeded);
    let mut observed = 0;
    let observer: Observer = Box::new(move |event| {
        observed += 1;
        if observed > MAX_EVENTS {
            guard.set(true);
            Observe::Stop
        } else if halt(event) {
            Observe::Stop
        } else {
            Observe::Continue
        }
    });
    let outcome = togi_simrun::simulate(SimInput {
        config: Config::default(),
        source: ConfigSource {
            path: DEFAULT_PATH.to_owned(),
            file: false,
        },
        build: build(),
        simulator: machine,
        persistence: persistence.clone(),
        replay: Replay::Warm,
        verify_every: None,
        max_boots,
        stop_at: None,
        write_samples: false,
        defects: togi_defect::KNOWN_DEFECTS,
        log: None,
        renderer: Renderer::default(),
        zone: TimeZone::UTC,
        cancel: CancellationToken::new(),
        observer: Some(observer),
        dump_to: None,
        adapt_trials: None,
    })?;
    if exceeded.get() {
        return Err(Error::EventCap(MAX_EVENTS));
    }
    match outcome {
        SimOutcome::Stopped(finished) if finished.errors.is_empty() => {}
        SimOutcome::Stopped(finished) => return Err(Error::Session(finished.errors)),
        SimOutcome::Capped { boots } => return Err(Error::Capped(boots)),
    }
    Ok(togi_simrun::events(persistence)?)
}
