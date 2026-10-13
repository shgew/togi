use std::io::{self, Write as _};
use std::num::{NonZeroU32, NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use clap::{Args, Parser, Subcommand, ValueEnum};
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use togi_tool_bench::{Fleet, Options, Selection, execute, inspect, sanity};

mod forecast_cli;
mod same_cli;

const USAGE: u8 = 2;

#[derive(Debug, Parser)]
#[command(
    name = "togi-tool-bench",
    version = togi_version::version(),
    about = "Run the suite of simulated sessions, measure each, and compare the run with a baseline",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    run: RunArgs,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Compare a run's records with the scores of a recorded baseline, metric by metric.
    Sanity(SanityArgs),
    /// Prove two trees decide the same on every simulated session, or only the suite's smoke sessions.
    Same(same_cli::SameArgs),
    /// Forecast a real run from a copy of its state directory with the suite's target scenario.
    Forecast(forecast_cli::ForecastArgs),
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Split {
    Dev,
    Holdout,
    All,
}

#[derive(Debug, Args)]
struct RunArgs {
    /// The scenario file; machine paths are relative to it.
    #[arg(long, value_name = "FILE", default_value = "tools/bench/suite.json")]
    suite: PathBuf,
    /// The seed split to run.
    #[arg(long, value_enum, default_value = "dev")]
    split: Split,
    /// Write one JSON record per run to this file.
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
    /// Write the records as a baseline file keeps them: without `model_check` and `wall_s`, which no comparison reads. Comparison and gates accept it as `--baseline`.
    #[arg(long, value_name = "FILE")]
    baseline_out: Option<PathBuf>,
    /// Compare the run with the records in this file and, when the suite registers a gate, judge it and exit 1 unless it passes.
    #[arg(long, value_name = "FILE")]
    baseline: Option<PathBuf>,
    /// Keep each run's state directory under a new directory in DIR; keeping skips the cache.
    #[arg(long, value_name = "DIR")]
    keep: Option<PathBuf>,
    /// Run N simulators at once; default the available parallelism.
    #[arg(long, value_name = "N")]
    jobs: Option<NonZeroUsize>,
    /// Hang safety limit per simulator in seconds; exceeding it is a harness error, never a session outcome.
    #[arg(long, value_name = "SECONDS", default_value = "1800")]
    timeout: NonZeroU64,
    /// Stop a session after N simulated boots: the run is censored.
    #[arg(long, value_name = "N", default_value = "1000")]
    max_boots: NonZeroU32,
    /// The directory of cached runs and their costs; default the user cache directory's togi/bench-rs.
    #[arg(long, value_name = "DIR", conflicts_with = "no_cache")]
    cache: Option<PathBuf>,
    /// Run every session instead of reusing cached ones.
    #[arg(long)]
    no_cache: bool,
    /// Use this simulator command instead of building togi-tool-sim from the tree.
    #[arg(long, value_name = "FILE")]
    sim: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct SanityArgs {
    /// The records of the Rust bench run, as `--out` wrote them.
    #[arg(long, value_name = "FILE")]
    records: PathBuf,
    /// The recorded baseline scores.
    #[arg(
        long,
        value_name = "FILE",
        default_value = "tools/bench/baseline.jsonl"
    )]
    baseline: PathBuf,
}

fn main() -> ExitCode {
    match Cli::try_parse() {
        Ok(cli) => match cli.command {
            None => bench(cli.run),
            Some(Command::Sanity(args)) => sanity_check(&args),
            Some(Command::Same(args)) => same_cli::run(args),
            Some(Command::Forecast(args)) => forecast_cli::run(args),
        },
        Err(error) => {
            if error.use_stderr() {
                let _ = write!(io::stderr(), "{}", error.render());
                ExitCode::from(USAGE)
            } else {
                let _ = write!(io::stdout(), "{}", error.render());
                ExitCode::SUCCESS
            }
        }
    }
}

fn fail(error: &dyn std::fmt::Display) -> ExitCode {
    let _ = writeln!(io::stderr(), "togi-tool-bench: {error}");
    ExitCode::FAILURE
}

fn bench(args: RunArgs) -> ExitCode {
    let fleet = Arc::new(Fleet::new());
    if let Err(error) = cancel_on_signals(&fleet) {
        return fail(&format!("watch for signals: {error}"));
    }
    let cache = if args.no_cache {
        None
    } else {
        match args.cache.or_else(default_cache) {
            Some(cache) => Some(cache),
            None => {
                return fail(&"no cache directory: set HOME or XDG_CACHE_HOME, or pass --cache");
            }
        }
    };
    let tree = match inspect(
        &args
            .out
            .iter()
            .chain(&args.baseline_out)
            .map(PathBuf::as_path)
            .collect::<Vec<_>>(),
    ) {
        Ok(tree) => tree,
        Err(error) => return fail(&error),
    };
    let options = Options {
        suite: args.suite,
        selection: match args.split {
            Split::Dev => Selection::Dev,
            Split::Holdout => Selection::Holdout,
            Split::All => Selection::All,
        },
        out: args.out,
        baseline_out: args.baseline_out,
        tree,
        baseline: args.baseline,
        keep: args.keep,
        jobs: args
            .jobs
            .or_else(|| thread::available_parallelism().ok())
            .unwrap_or(NonZeroUsize::MIN),
        timeout: Duration::from_secs(args.timeout.get()),
        max_boots: args.max_boots,
        cache,
        simulator: args.sim,
    };
    let report = match execute(&options, &fleet) {
        Ok(report) => report,
        Err(error) => return fail(&error),
    };
    let mut stderr = io::stderr();
    for warning in &report.warnings {
        let _ = writeln!(stderr, "togi-tool-bench: {warning}");
    }
    if let Some(kept) = &report.kept {
        let _ = writeln!(
            stderr,
            "togi-tool-bench: kept the runs in {}",
            kept.display()
        );
    }
    if let Err(error) = report.write(&mut io::stdout().lock()) {
        return fail(&format!("write the report: {error}"));
    }
    if report.gate.as_ref().is_some_and(|gate| !gate.passed()) {
        return fail(&"gate failed");
    }
    ExitCode::SUCCESS
}

fn sanity_check(args: &SanityArgs) -> ExitCode {
    let compared = togi_tool_bench::read_records(&args.records, "records")
        .and_then(|rust| Ok((rust, sanity::read_baseline(&args.baseline)?)));
    let (rust, baseline) = match compared {
        Ok(pair) => pair,
        Err(error) => return fail(&error),
    };
    let lines = sanity::compare(&rust, &baseline);
    if lines.is_empty() {
        return fail(&"no run is in both the records and the baseline scores");
    }
    if let Err(error) = sanity::report(&mut io::stdout().lock(), &lines) {
        return fail(&format!("write the report: {error}"));
    }
    if lines.iter().any(|line| line.flagged) {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn default_cache() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").filter(|home| !home.is_empty());
    let base = std::env::var_os("XDG_CACHE_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.map(|home| {
                let mut dir = PathBuf::from(home);
                dir.push(if cfg!(target_os = "macos") {
                    "Library/Caches"
                } else {
                    ".cache"
                });
                dir
            })
        })?;
    Some(base.join("togi").join("bench-rs"))
}

/// Stops the fleet on SIGINT, SIGTERM and SIGHUP. Handlers feed a watcher thread, so the signal mask stays empty
/// and the processes the bench spawns do not start with these signals blocked.
fn cancel_on_signals(fleet: &Arc<Fleet>) -> io::Result<()> {
    let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP])?;
    let fleet = Arc::clone(fleet);
    thread::Builder::new()
        .name("signals".into())
        .spawn(move || {
            for _ in signals.forever() {
                fleet.cancel();
            }
        })
        .map(drop)
}
