use std::io::{self, Write as _};
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use clap::Args;
use togi_tool_bench::forecast::{Options, execute};
use togi_tool_bench::{Fleet, inspect};

use crate::{cancel_on_signals, fail};

#[derive(Debug, Args)]
pub(crate) struct ForecastArgs {
    /// A copy of the real run's state directory; it is read, never written.
    #[arg(value_name = "STATE_DIR")]
    state: PathBuf,
    /// The scenario file; its `target` scenario is the ensemble.
    #[arg(long, value_name = "FILE", default_value = "tools/bench/suite.json")]
    suite: PathBuf,
    /// Write the forecast record to this file.
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
    /// Keep every run's directory under a new directory in DIR.
    #[arg(long, value_name = "DIR")]
    keep: Option<PathBuf>,
    /// Run N simulators at once; default the available parallelism.
    #[arg(long, value_name = "N")]
    jobs: Option<NonZeroUsize>,
    /// Hang safety limit per simulator in seconds; exceeding it is a harness error, never a run outcome.
    #[arg(long, value_name = "SECONDS", default_value = "1800")]
    timeout: NonZeroU64,
    /// Stop a run after N simulated boots: the run is censored.
    #[arg(long, value_name = "N", default_value = "1000")]
    max_boots: NonZeroU32,
    /// Use this simulator command instead of building togi-tool-sim from the tree.
    #[arg(long, value_name = "FILE")]
    sim: Option<PathBuf>,
}

pub(crate) fn run(args: ForecastArgs) -> ExitCode {
    let fleet = Arc::new(Fleet::new());
    if let Err(error) = cancel_on_signals(&fleet) {
        return fail(&format!("watch for signals: {error}"));
    }
    let tree = match inspect(&args.out.iter().map(PathBuf::as_path).collect::<Vec<_>>()) {
        Ok(tree) => tree,
        Err(error) => return fail(&togi_machine::error_chain(&error)),
    };
    let options = Options {
        state: args.state,
        suite: args.suite,
        tree,
        out: args.out,
        keep: args.keep,
        jobs: args
            .jobs
            .or_else(|| thread::available_parallelism().ok())
            .unwrap_or(NonZeroUsize::MIN),
        timeout: Duration::from_secs(args.timeout.get()),
        max_boots: args.max_boots,
        simulator: args.sim,
    };
    let mut stderr = io::stderr();
    let report = match execute(&options, &fleet) {
        Ok(report) => report,
        Err(failed) => {
            if let Some(kept) = &failed.kept {
                let _ = writeln!(
                    stderr,
                    "togi-tool-bench: kept the runs in {}",
                    kept.display()
                );
            }
            return fail(&togi_machine::error_chain(&failed));
        }
    };
    if let Some(kept) = &report.kept {
        let _ = writeln!(
            stderr,
            "togi-tool-bench: kept the runs in {}",
            kept.display()
        );
    }
    match report.write(&mut io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(&format!("write the report: {error}")),
    }
}
