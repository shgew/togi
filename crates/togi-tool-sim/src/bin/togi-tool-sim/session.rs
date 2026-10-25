use std::env;
use std::io::{self, Write as _};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use jiff::SignedDuration;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use togi_config::{Config, DEFAULT_PATH};
use togi_journal::{Event, Payload, Seq, ShutdownReason};
use togi_machine::CancellationToken;
use togi_render::{OutputFacts, Renderer, Style};
use togi_session::{ConfigSource, Stop};
use togi_simrun::{Finished, Persistence, Replay, ResumeError, SimInput, SimOutcome};
use togi_tool_trialfacts::Extracts;

use crate::log::Log;
use crate::{SessionArgs, build, machine, signals};

const FAILED: u8 = 1;
const CAPPED: u8 = 3;

/// Runs one session, resuming the journal of an existing state directory. The simulated operator ends the night
/// after `--night-hours`. Exit status 0 for that end, 1 for an error or a night that ends at a dead end, 3 when the
/// boot cap ends an unfinished session.
pub(crate) fn run(args: SessionArgs, log: &mut Log) -> u8 {
    run_with(
        args,
        log,
        |args| {
            machine::load(
                args.machine.as_deref(),
                args.seed,
                args.replay_facts,
                &mut Extracts::new(),
            )
        },
        |args, log| {
            let dir = if let Some(dir) = args.state_dir.filter(|dir| !dir.as_os_str().is_empty()) {
                dir
            } else {
                let dir = temporary()?;
                log.announce(&format!("sim: state directory {}", dir.display()));
                dir
            };
            Ok(Persistence::Files(dir))
        },
        || {
            let cancel = CancellationToken::new();
            signals::cancel_on_signals(cancel.clone()).map_err(Error::Signals)?;
            Ok(Context {
                cancel,
                renderer: Renderer::new(OutputFacts::detect(
                    &io::stderr(),
                    env::var_os("NO_COLOR").as_deref(),
                    env::var_os("JOURNAL_STREAM").as_deref(),
                )),
                zone: TimeZone::system(),
            })
        },
        togi_simrun::simulate,
    )
}

pub(crate) struct Context {
    pub cancel: CancellationToken,
    pub renderer: Renderer,
    pub zone: TimeZone,
}

pub(crate) fn run_with(
    args: SessionArgs,
    log: &mut Log,
    load: impl FnOnce(
        &SessionArgs,
    )
        -> Result<(togi_sim::Model, Option<togi_sim::replay::Replay>), machine::LoadError>,
    persistence: impl FnOnce(SessionArgs, &mut Log) -> Result<Persistence, Error>,
    context: impl FnOnce() -> Result<Context, Error>,
    execute: impl FnOnce(SimInput) -> Result<SimOutcome, togi_simrun::SimError>,
) -> u8 {
    match simulate(args, log, load, persistence, context, execute) {
        Ok(code) => code,
        Err(error) => {
            let _ = writeln!(log, "sim: {}", togi_machine::error_chain(&error));
            FAILED
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error(transparent)]
    Machine(#[from] machine::LoadError),
    #[error("create the state directory")]
    TempDir(#[source] io::Error),
    #[error("read the state directory's configuration")]
    Config(#[from] togi_simrun::HistoryError),
    #[error("resume the simulated machine")]
    Resume(#[from] ResumeError),
    #[error("watch for signals")]
    Signals(#[source] io::Error),
    #[error(transparent)]
    Simulate(#[from] togi_simrun::SimError),
}

fn simulate(
    args: SessionArgs,
    log: &mut Log,
    load: impl FnOnce(
        &SessionArgs,
    )
        -> Result<(togi_sim::Model, Option<togi_sim::replay::Replay>), machine::LoadError>,
    persistence: impl FnOnce(SessionArgs, &mut Log) -> Result<Persistence, Error>,
    context: impl FnOnce() -> Result<Context, Error>,
    execute: impl FnOnce(SimInput) -> Result<SimOutcome, togi_simrun::SimError>,
) -> Result<u8, Error> {
    let Context {
        cancel,
        renderer,
        zone,
    } = context()?;
    let (model, replay) = load(&args)?;
    let seed = args.seed;
    let cold_boots = args.cold_boots;
    let verify_every = args.verify_every;
    let max_boots = args.max_boots;
    let night = SignedDuration::from_hours(i64::from(args.night_hours.get()));
    let write_samples = args.samples;
    let persistence = persistence(args, log)?;
    let config = togi_simrun::recorded_config(&persistence, Config::default())?;
    let simulator = togi_simrun::resume_simulator(&persistence, model, seed, replay)?;
    // The night ends a fixed time after it started, however many processes the machine's boots span. A night the
    // operator already closed is over; a rerun opens a fresh one at the simulator's clock.
    let started = night_started(&togi_simrun::events(&persistence)?, simulator.now());
    let stop_at = Some(started + night);
    let dir = match &persistence {
        Persistence::Files(dir) => dir.as_path(),
        Persistence::Memory(_) => Path::new("/state"),
    };
    let outcome = execute(SimInput {
        config,
        source: ConfigSource {
            path: DEFAULT_PATH.to_owned(),
            file: false,
        },
        build: build(),
        simulator,
        persistence: persistence.clone(),
        replay: if cold_boots {
            Replay::Cold
        } else {
            Replay::Warm
        },
        verify_every: NonZeroU32::new(verify_every),
        max_boots,
        stop_at,
        write_samples,
        defects: togi_defect::KNOWN_DEFECTS,
        log: Some(Box::new(log.clone())),
        renderer,
        zone: zone.clone(),
        cancel,
        observer: None,
        dump_to: None,
    })?;
    Ok(match outcome {
        SimOutcome::Capped { boots } => {
            let _ = writeln!(
                log,
                "sim: the simulated machine reached its boot cap without stopping after {boots} boots"
            );
            CAPPED
        }
        SimOutcome::Stopped(finished) => report(&finished, &persistence, renderer, &zone, log, dir),
    })
}

/// When the night this run belongs to started: the first event after the operator's last closing signal stop, so an
/// unfinished or crashed night keeps its original start and a closed one gives way to a fresh night at `now`.
fn night_started(events: &[Event], now: Timestamp) -> Timestamp {
    let opened = events
        .iter()
        .rposition(|event| {
            matches!(&event.payload, Payload::Shutdown(shutdown) if shutdown.reason == ShutdownReason::Signal)
        })
        .map_or(0, |closed| closed + 1);
    events.get(opened).map_or(now, |first| first.time)
}

fn temporary() -> Result<PathBuf, Error> {
    tempfile::Builder::new()
        .prefix("togi-sim-")
        .tempdir()
        .map(tempfile::TempDir::keep)
        .map_err(Error::TempDir)
}

fn report(
    finished: &Finished,
    persistence: &Persistence,
    renderer: Renderer,
    zone: &TimeZone,
    log: &mut Log,
    dir: &Path,
) -> u8 {
    for error in &finished.errors {
        let _ = writeln!(log, "sim: {}", togi_machine::error_chain(error));
    }
    match &finished.stop {
        Some(Stop::DeadEnd {
            dead_end, causes, ..
        }) => {
            let line = format!("sim: dead end {}: {}", dead_end.condition, dead_end.detail);
            let _ = writeln!(log, "{}", renderer.text(Style::RedBold, &line));
            match togi_simrun::events(persistence) {
                Ok(events) => {
                    for event in evidence(&events, causes) {
                        let _ = writeln!(
                            log,
                            "{}",
                            renderer.prefixed_line(event, zone, "  evidence: ")
                        );
                    }
                }
                Err(error) => {
                    let _ = writeln!(
                        log,
                        "sim: read the evidence from {}: {}",
                        dir.display(),
                        togi_machine::error_chain(&error)
                    );
                }
            }
            FAILED
        }
        Some(Stop::Signal) if finished.errors.is_empty() => 0,
        Some(Stop::Night(reason)) => {
            let line = format!("sim: the night ended at a dead end: {reason:?}");
            let _ = writeln!(log, "{}", renderer.text(Style::RedBold, &line));
            FAILED
        }
        Some(Stop::Signal) => FAILED,
        None => {
            if finished.errors.is_empty() {
                let _ = writeln!(log, "sim: the session ended without a stop");
            }
            FAILED
        }
    }
}

fn evidence<'a>(events: &'a [Event], causes: &[Seq]) -> impl Iterator<Item = &'a Event> {
    events.iter().filter(|event| causes.contains(&event.seq))
}
