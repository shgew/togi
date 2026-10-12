use std::fs;
use std::io::{self, Write as _};
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Instant;

use jiff::tz::TimeZone;
use togi_config::Config;
use togi_journal::{MemFs, StateDir};
use togi_machine::CancellationToken;
use togi_render::Renderer;
use togi_session::{ConfigSource, Stop, StopPolicy};
use togi_sim::{RunSettings, Simulator};
use togi_simrun::{Persistence, Replay, SimError, SimInput, SimOutcome};
use togi_tool_audit::{Completion, audit_recordings};
use togi_tool_sim::{
    ExpectedShard, Outcome, Run, RunResult, ShardManifest, Suite, encode_manifest, parse_manifest,
    shard,
};
use togi_tool_trialfacts::Extracts;

use crate::log::Log;
use crate::{CheckArgs, SuiteArgs, build, machine, signals};

const FAILED: u8 = 1;

/// Runs every session of one shard, each fresh in its own in-memory state directory, and writes the shard's
/// manifest. Exit status 0 only when every session ended clean.
pub(crate) fn run(args: &SuiteArgs, log: &mut Log) -> u8 {
    match run_shard(args, log) {
        Ok(true) => 0,
        Ok(false) => FAILED,
        Err(error) => {
            let _ = writeln!(log, "sim: {error}");
            FAILED
        }
    }
}

/// Checks the manifests of every shard against the suite. Exit status 0 only when every session of the suite is
/// reported once, by the shard that ran it, and ended clean.
pub(crate) fn check(args: &CheckArgs, log: &mut Log) -> u8 {
    match check_manifests(args) {
        Ok(()) => 0,
        Err(error) => {
            let _ = writeln!(log, "sim: {error}");
            FAILED
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Suite(#[from] togi_tool_sim::SuiteError),
    #[error(transparent)]
    Shard(#[from] togi_tool_sim::ShardError),
    #[error("watch for signals: {0}")]
    Signals(#[source] io::Error),
    #[error("encode the manifest: {0}")]
    Encode(#[source] serde_json::Error),
    #[error("write {}: {source}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{}: {source}", path.display())]
    Manifest {
        path: PathBuf,
        #[source]
        source: togi_tool_sim::ManifestError,
    },
    #[error("{} is not a shard manifest named shard-I.jsonl", path.display())]
    Stray { path: PathBuf },
    #[error(transparent)]
    Coverage(#[from] togi_tool_sim::CoverageError),
}

fn run_shard(args: &SuiteArgs, log: &mut Log) -> Result<bool, Error> {
    let cancel = CancellationToken::new();
    signals::cancel_on_signals(cancel.clone()).map_err(Error::Signals)?;
    let runs = shard(&Suite::load(&args.suite)?.runs(), args.shards, args.shard)?;
    let jobs = args
        .jobs
        .unwrap_or_else(|| thread::available_parallelism().unwrap_or(NonZeroUsize::MIN))
        .get()
        .min(runs.len());
    let mut outcomes: Vec<Option<Outcome>> = vec![None; runs.len()];
    let next = AtomicUsize::new(0);
    let (sender, finished) = mpsc::channel();
    thread::scope(|scope| {
        for _ in 0..jobs {
            let (sender, runs, next, cancel) = (sender.clone(), &runs, &next, &cancel);
            scope.spawn(move || {
                let mut extracts = Extracts::new();
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(run) = runs.get(index) else {
                        break;
                    };
                    let started = Instant::now();
                    let outcome = execute(run, MemFs::new(), args.max_boots, &mut extracts, cancel);
                    if sender.send((index, started.elapsed(), outcome)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);
        for (index, elapsed, outcome) in finished {
            let _ = writeln!(
                log,
                "sim: {} {:.3}s {outcome}",
                runs[index].id,
                elapsed.as_secs_f64()
            );
            outcomes[index] = Some(outcome);
        }
    });
    let results: Vec<RunResult> = runs
        .into_iter()
        .zip(outcomes)
        .map(|(run, outcome)| RunResult {
            id: run.id,
            outcome: outcome.unwrap_or_else(|| Outcome::Failed {
                detail: "the run never reported an outcome".to_owned(),
            }),
        })
        .collect();
    let clean = results
        .iter()
        .all(|result| result.outcome == Outcome::Clean);
    let path = args.out.join(manifest_name(args.shard));
    let text = encode_manifest(&results).map_err(Error::Encode)?;
    fs::create_dir_all(&args.out)
        .and_then(|()| fs::write(&path, text))
        .map_err(|source| Error::Write { path, source })?;
    Ok(clean)
}

fn manifest_name(shard: usize) -> String {
    format!("shard-{shard}.jsonl")
}

fn shard_of(name: &str) -> Option<usize> {
    let digits = name.strip_prefix("shard-")?.strip_suffix(".jsonl")?;
    let shard: usize = digits.parse().ok()?;
    (manifest_name(shard) == name).then_some(shard)
}

fn check_manifests(args: &CheckArgs) -> Result<(), Error> {
    let runs = Suite::load(&args.suite)?.runs();
    let indices = match args.shard {
        Some(index) => index..=index,
        None => 0..=args.shards.get() - 1,
    };
    let mut expected = Vec::new();
    for index in indices {
        expected.push(ExpectedShard {
            shard: index,
            runs: shard(&runs, args.shards, index)?
                .into_iter()
                .map(|run| run.id)
                .collect(),
        });
    }
    let read = |path: &Path, source| Error::Read {
        path: path.to_path_buf(),
        source,
    };
    let mut manifests = Vec::new();
    for entry in fs::read_dir(&args.manifests).map_err(|source| read(&args.manifests, source))? {
        let path = entry
            .map_err(|source| read(&args.manifests, source))?
            .path();
        let Some(shard) = path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(shard_of)
        else {
            return Err(Error::Stray { path });
        };
        let text = fs::read_to_string(&path).map_err(|source| read(&path, source))?;
        let results = parse_manifest(&text).map_err(|source| Error::Manifest {
            path: path.clone(),
            source,
        })?;
        manifests.push(ShardManifest { shard, results });
    }
    manifests.sort_by_key(|manifest| manifest.shard);
    Ok(togi_tool_sim::check(&expected, &manifests)?)
}

/// Runs one session of the suite and judges it: clean only when it stopped after its cycles, the last boot's
/// resume state matched a fresh fold of the journal (the simulation checks that after every crash too), the tuner
/// replays the journal, and the audit finds no violation in any of the directory's journals and a published
/// `state.json` at the journal's last event. `fs` is the session's fresh in-memory state directory.
fn execute(
    run: &Run,
    fs: MemFs,
    max_boots: NonZeroU32,
    extracts: &mut Extracts,
    cancel: &CancellationToken,
) -> Outcome {
    let (model, replay) =
        match machine::load(run.machine.as_deref(), run.seed, run.replay, extracts) {
            Ok(loaded) => loaded,
            Err(error) => {
                return Outcome::Failed {
                    detail: error.to_string(),
                };
            }
        };
    let persistence = Persistence::Memory(fs.clone());
    let simulated = togi_simrun::simulate(SimInput {
        config: Config::default(),
        source: ConfigSource::default(),
        build: build(),
        simulator: Simulator::new(
            model,
            RunSettings {
                seed: run.seed,
                history: togi_sim::History::default(),
                replay,
            },
        ),
        persistence: persistence.clone(),
        replay: Replay::Warm,
        verify_every: NonZeroU32::new(1),
        max_boots,
        policy: StopPolicy {
            cycles: NonZeroU32::new(1),
            first_result: false,
        },
        write_samples: false,
        defects: togi_defect::KNOWN_DEFECTS,
        log: None,
        renderer: Renderer::default(),
        zone: TimeZone::UTC,
        cancel: cancel.child_token(),
        observer: None,
        dump_to: None,
    });
    let finished = match simulated {
        Ok(SimOutcome::Stopped(finished)) => finished,
        Ok(SimOutcome::Capped { boots }) => return Outcome::Capped { boots: boots.get() },
        Err(error @ (SimError::Diverged { .. } | SimError::NoCapsule { .. })) => {
            return Outcome::VerifyFailed {
                detail: error.to_string(),
            };
        }
        Err(error) => {
            return Outcome::Failed {
                detail: error.to_string(),
            };
        }
    };
    if !finished.errors.is_empty() {
        let errors: Vec<String> = finished.errors.iter().map(ToString::to_string).collect();
        return Outcome::Failed {
            detail: errors.join("; "),
        };
    }
    match &finished.stop {
        Some(Stop::Cycles) => {}
        Some(Stop::DeadEnd { dead_end, .. }) => {
            return Outcome::DeadEnd {
                condition: dead_end.condition,
                detail: dead_end.detail.clone(),
            };
        }
        Some(Stop::Signal) => {
            return Outcome::Stopped {
                reason: togi_journal::ShutdownReason::Signal,
            };
        }
        Some(Stop::FirstResult) => {
            return Outcome::Stopped {
                reason: togi_journal::ShutdownReason::FirstResult,
            };
        }
        None => {
            return Outcome::Failed {
                detail: "the session ended without a stop".to_owned(),
            };
        }
    }
    judge(&persistence, fs)
}

fn judge(persistence: &Persistence, fs: MemFs) -> Outcome {
    let events = match togi_simrun::events(persistence) {
        Ok(events) => events,
        Err(error) => {
            return Outcome::Failed {
                detail: format!("read the journal: {error}"),
            };
        }
    };
    if let Err(error) = togi_tuner::replay(&events) {
        return Outcome::ReplayFailed {
            detail: error.to_string(),
        };
    }
    let audits = match open(fs).map(|dir| audit_recordings(&dir, Completion::Finished)) {
        Ok(Ok(audits)) => audits,
        Ok(Err(error)) => {
            return Outcome::Failed {
                detail: format!("audit the journals: {error}"),
            };
        }
        Err(error) => {
            return Outcome::Failed {
                detail: format!("open the state directory: {error}"),
            };
        }
    };
    let violations: Vec<_> = audits
        .into_iter()
        .flat_map(|audit| audit.violations)
        .collect();
    if violations.is_empty() {
        Outcome::Clean
    } else {
        Outcome::AuditViolations { violations }
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "a suite session keeps its state directory in memory"
)]
fn open(fs: MemFs) -> Result<StateDir, togi_journal::LockError> {
    StateDir::simulated(fs)
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::path::Path;

    use togi_journal::MemFs;
    use togi_journal::fs::Fs as _;
    use togi_machine::CancellationToken;
    use togi_tool_audit::Check;
    use togi_tool_sim::{Outcome, Run, Split};
    use togi_tool_trialfacts::Extracts;

    use togi_simrun::Persistence;

    use super::{execute, judge};

    fn outcome(fs: MemFs) -> Outcome {
        let run = Run {
            id: serde_json::from_value(serde_json::Value::String("default/dev-1".to_owned()))
                .unwrap_or_else(|_: serde_json::Error| unreachable!("a run id is a string")),
            scenario: "default".to_owned(),
            split: Split::Dev,
            seed: 1,
            machine: None,
            replay: false,
            smoke: false,
        };
        execute(
            &run,
            fs,
            NonZeroU32::new(1000).unwrap(),
            &mut Extracts::new(),
            &CancellationToken::new(),
        )
    }

    #[test]
    fn a_finished_session_without_its_final_state_document_is_not_clean() {
        let fs = MemFs::new();
        assert_eq!(outcome(fs.clone()), Outcome::Clean);
        fs.remove_file(Path::new("/state/state.json")).unwrap();
        let judged = judge(&Persistence::Memory(fs.clone()), fs);
        let Outcome::AuditViolations { violations } = judged else {
            panic!("a finished session without a state.json must not be judged clean: {judged}");
        };
        assert!(
            violations
                .iter()
                .any(|violation| violation.check == Check::Projection),
            "{violations:?}"
        );
    }
}
