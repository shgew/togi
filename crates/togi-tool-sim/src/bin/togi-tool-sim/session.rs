use std::env;
use std::io::{self, Write as _};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use jiff::tz::TimeZone;
use togi_config::{Config, DEFAULT_PATH};
use togi_journal::{Event, Seq};
use togi_machine::CancellationToken;
use togi_render::{OutputFacts, Renderer, Style};
use togi_session::{ConfigSource, Stop, StopPolicy};
use togi_simrun::{Finished, Persistence, Replay, ResumeError, SimInput, SimOutcome};
use togi_tool_trialfacts::Extracts;

use crate::log::Log;
use crate::{SessionArgs, build, machine, signals};

const FAILED: u8 = 1;
const CAPPED: u8 = 3;

/// Runs one session, resuming the journal of an existing state directory. Exit status 0 for a stop other than a
/// dead end, 1 for an error or a dead end, 3 when the boot cap ends an unfinished session.
pub(crate) fn run(args: SessionArgs, log: &mut Log) -> u8 {
    match simulate(args, log) {
        Ok(code) => code,
        Err(error) => {
            let _ = writeln!(log, "sim: {error}");
            FAILED
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Machine(#[from] machine::LoadError),
    #[error("create the state directory: {0}")]
    TempDir(#[source] io::Error),
    #[error("read the state directory's configuration: {0}")]
    Config(#[from] togi_simrun::HistoryError),
    #[error("resume the simulated machine: {0}")]
    Resume(#[from] ResumeError),
    #[error("watch for signals: {0}")]
    Signals(#[source] io::Error),
    #[error(transparent)]
    Simulate(#[from] togi_simrun::SimError),
}

fn simulate(args: SessionArgs, log: &mut Log) -> Result<u8, Error> {
    let cancel = CancellationToken::new();
    signals::cancel_on_signals(cancel.clone()).map_err(Error::Signals)?;
    let (model, replay) = machine::load(
        args.machine.as_deref(),
        args.seed,
        args.replay_facts,
        &mut Extracts::new(),
    )?;
    let dir = if let Some(dir) = args.state_dir.filter(|dir| !dir.as_os_str().is_empty()) {
        dir
    } else {
        let dir = temporary()?;
        log.announce(&format!("sim: state directory {}", dir.display()));
        dir
    };
    let config = togi_simrun::recorded_config(&dir, Config::default())?;
    let persistence = Persistence::Files(dir.clone());
    let simulator = togi_simrun::resume_simulator(&persistence, model, args.seed, replay)?;
    let renderer = Renderer::new(OutputFacts::detect(
        &io::stderr(),
        env::var_os("NO_COLOR").as_deref(),
        env::var_os("JOURNAL_STREAM").as_deref(),
    ));
    let zone = TimeZone::system();
    let outcome = togi_simrun::simulate(SimInput {
        config,
        source: ConfigSource {
            path: DEFAULT_PATH.to_owned(),
            file: false,
        },
        build: build(),
        simulator,
        persistence: persistence.clone(),
        replay: if args.cold_boots {
            Replay::Cold
        } else {
            Replay::Warm
        },
        verify_every: NonZeroU32::new(args.verify_every),
        max_boots: args.max_boots,
        policy: StopPolicy {
            cycles: Some(args.cycles),
            first_result: false,
        },
        write_samples: args.samples,
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
        SimOutcome::Stopped(finished) => {
            report(&finished, &persistence, renderer, &zone, log, &dir)
        }
    })
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
        let _ = writeln!(log, "sim: {error}");
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
                        "sim: read the evidence from {}: {error}",
                        dir.display()
                    );
                }
            }
            FAILED
        }
        Some(Stop::Signal | Stop::Cycles | Stop::FirstResult) if finished.errors.is_empty() => 0,
        Some(_) => FAILED,
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
