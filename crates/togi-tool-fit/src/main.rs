use std::io::Write as _;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::error::ErrorKind;
use clap::{CommandFactory as _, Parser};
use togi_tool_fit::compare::compare_files;
use togi_tool_fit::{Ensemble, Mode, Options, Sessions, run};

/// Fit simulated machines to the real trial facts and check the fit forward on later sessions.
#[derive(Debug, Parser)]
#[command(name = "togi-tool-fit")]
struct Cli {
    /// The gzip extract of decisive trial facts. Without --current-extract it may hold no session recorded
    /// after the original research artifact, and one holding the artifact's sessions must be that artifact.
    #[arg(long)]
    facts: Option<PathBuf>,
    /// Read every session of --facts: the owner-approved fit or check of the current extract, never a
    /// research campaign's.
    #[arg(long)]
    current_extract: bool,
    /// The directory machine files are written to; required unless --forward-only or --write-artifact.
    #[arg(long, conflicts_with_all = ["forward_only", "write_artifact"])]
    out: Option<PathBuf>,
    /// The bootstrap seed: refit N resamples with the stream seeded SEED+N.
    #[arg(long, default_value_t = 263)]
    seed: u64,
    /// The number of whole-trial bootstrap refits.
    #[arg(long, default_value_t = 8)]
    bootstrap: usize,
    /// The threads to run on; every CPU when absent. The results do not depend on it.
    #[arg(long)]
    jobs: Option<NonZeroUsize>,
    /// Run only the forward-chained check: fit no ensemble and write no machine files.
    #[arg(long)]
    forward_only: bool,
    /// With --forward-only, leave the newest N sessions out of the check.
    #[arg(long, default_value_t = 0, requires = "forward_only")]
    seal: usize,
    /// With --forward-only and --current-extract, also score these committed machine files, fitted to the
    /// current extract, as one ensemble, on the same held-out sessions beside the fit: each trial's
    /// probability is the mean over all of them. They are not fitted.
    #[arg(
        long,
        num_args = 1..,
        value_name = "MACHINE",
        requires = "forward_only",
        requires = "current_extract"
    )]
    reference_machine: Vec<PathBuf>,
    /// Fit all decisive facts to target-shared-voltage.json; in-sample only, not forward-validated.
    #[arg(
        long,
        conflicts_with_all = ["forward_only", "seal", "seed", "bootstrap"]
    )]
    shared_voltage_in_sample: bool,
    /// Write the original research artifact, the rows of its seven sessions in --facts, to the new file
    /// FILE after checking their hash; fit nothing.
    #[arg(
        long,
        value_name = "FILE",
        conflicts_with_all = [
            "current_extract",
            "forward_only",
            "shared_voltage_in_sample",
            "seed",
            "bootstrap"
        ]
    )]
    write_artifact: Option<PathBuf>,
    /// With --forward-only, also write the check's full-precision records, one JSON line for the input,
    /// each scored trial, each held-out session and the pool, to the new file FILE.
    #[arg(long, value_name = "FILE", requires = "forward_only")]
    records: Option<PathBuf>,
    /// Pair a candidate's --records file with a reference's, run on the same rows and seal, and compare
    /// them exactly; reads no extract and fits nothing.
    #[arg(
        long,
        num_args = 2,
        value_names = ["CANDIDATE", "REFERENCE"],
        conflicts_with_all = [
            "facts",
            "current_extract",
            "out",
            "seed",
            "bootstrap",
            "jobs",
            "forward_only",
            "shared_voltage_in_sample",
            "write_artifact"
        ]
    )]
    compare: Vec<PathBuf>,
    /// With --compare, also write the comparison as a JSON object of numbers to the new file FILE.
    #[arg(long, value_name = "FILE", requires = "compare")]
    metrics: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut stdout = std::io::stdout();
    if let [candidate, reference] = cli.compare.as_slice() {
        return match compare_files(candidate, reference, cli.metrics.as_deref(), &mut stdout) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => fail(&error),
        };
    }
    let Some(facts) = cli.facts else {
        Cli::command()
            .error(
                ErrorKind::MissingRequiredArgument,
                "--facts FILE is required unless --compare",
            )
            .exit()
    };
    let mode = match (cli.write_artifact, cli.out) {
        (Some(out), _) => Mode::WriteArtifact { out },
        (None, _) if cli.forward_only => Mode::Forward {
            seal: cli.seal,
            reference: cli.reference_machine,
            records: cli.records,
        },
        (None, Some(out)) if cli.shared_voltage_in_sample => Mode::SharedVoltage { out },
        (None, Some(out)) => Mode::Ensemble {
            ensemble: Ensemble {
                seed: cli.seed,
                bootstrap: cli.bootstrap,
            },
            out,
        },
        (None, None) => Cli::command()
            .error(
                ErrorKind::MissingRequiredArgument,
                "--out DIR is required unless --forward-only or --write-artifact",
            )
            .exit(),
    };
    let options = Options {
        facts,
        sessions: if cli.current_extract {
            Sessions::Current
        } else {
            Sessions::Artifact
        },
        jobs: cli.jobs,
        mode,
    };
    match run(&options, &mut stdout) {
        Ok(elapsed) => {
            let printed = match elapsed.forward {
                Some(forward) => writeln!(stdout, "Forward-chained elapsed: {forward:.3?}"),
                None => Ok(()),
            }
            .and_then(|()| writeln!(stdout, "Fit elapsed: {:.3?}", elapsed.total));
            match printed {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    let _ = writeln!(
                        std::io::stderr(),
                        "togi-tool-fit: write the timing: {error}"
                    );
                    ExitCode::FAILURE
                }
            }
        }
        Err(error) => fail(&error),
    }
}

fn fail(error: &togi_tool_fit::Error) -> ExitCode {
    let _ = writeln!(
        std::io::stderr(),
        "togi-tool-fit: {}",
        togi_machine::error_chain(error)
    );
    ExitCode::FAILURE
}
