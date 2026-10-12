use std::io::{self, Write as _};
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use clap::Args;
use togi_tool_bench::Fleet;
use togi_tool_bench::same::{Options, execute};

use crate::{cancel_on_signals, default_cache, fail};

#[derive(Debug, Args)]
pub(crate) struct SameArgs {
    /// The tree to compare against, such as an export of the base revision.
    #[arg(value_name = "BASE")]
    base: PathBuf,
    /// The tree under test.
    #[arg(long, value_name = "DIR", default_value = ".")]
    head: PathBuf,
    /// The suite file; a relative path resolves in each tree.
    #[arg(long, value_name = "FILE", default_value = "tools/bench/suite.json")]
    suite: PathBuf,
    /// Compare only the sessions the suite lists under `smoke`.
    #[arg(long)]
    smoke: bool,
    /// Report every differing session instead of stopping at the first.
    #[arg(long)]
    keep_going: bool,
    /// Run every session on both trees instead of using cached digests; the runs are still cached.
    #[arg(long)]
    no_cache: bool,
    /// Keep every run directory, base and head apart, under a new directory in DIR; keeping implies --no-cache.
    #[arg(long, value_name = "DIR")]
    keep: Option<PathBuf>,
    /// Run N simulators at once; default the available parallelism.
    #[arg(long, value_name = "N")]
    jobs: Option<NonZeroUsize>,
    /// Hang safety limit per simulator in seconds; exceeding it is a harness error, never a session outcome.
    #[arg(long, value_name = "SECONDS", default_value = "1800")]
    timeout: std::num::NonZeroU64,
    /// Stop a session after N simulated boots: the run is censored.
    #[arg(long, value_name = "N", default_value = "1000")]
    max_boots: NonZeroU32,
    /// The directory of cached digests, session costs, builds and runs; default the user cache directory's
    /// togi/bench-rs.
    #[arg(long, value_name = "DIR")]
    cache: Option<PathBuf>,
    /// Run this simulator for the base instead of building it from the tree.
    #[arg(long, value_name = "FILE")]
    base_sim: Option<PathBuf>,
    /// Run this simulator for the head instead of building it from the tree.
    #[arg(long, value_name = "FILE")]
    head_sim: Option<PathBuf>,
}

pub(crate) fn run(args: SameArgs) -> ExitCode {
    let fleet = Arc::new(Fleet::new());
    if let Err(error) = cancel_on_signals(&fleet) {
        return fail(&format!("watch for signals: {error}"));
    }
    let Some(cache) = args.cache.or_else(default_cache) else {
        return fail(&"no cache directory: set HOME or XDG_CACHE_HOME, or pass --cache");
    };
    let options = Options {
        base: args.base,
        head: args.head,
        suite: args.suite,
        smoke: args.smoke,
        keep_going: args.keep_going,
        no_cache: args.no_cache,
        keep: args.keep,
        jobs: args
            .jobs
            .or_else(|| thread::available_parallelism().ok())
            .unwrap_or(NonZeroUsize::MIN),
        timeout: Duration::from_secs(args.timeout.get()),
        max_boots: args.max_boots,
        cache,
        simulators: [args.base_sim, args.head_sim],
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
            return fail(&failed);
        }
    };
    for warning in &report.warnings {
        let _ = writeln!(stderr, "togi-tool-bench: {warning}");
    }
    let timings = report.timings;
    let _ = writeln!(
        stderr,
        "togi-tool-bench: harness overhead: suites {:.0?}, builds {:.0?}, keys {:.0?}; sessions took {:.0?}",
        timings.suites, timings.builds, timings.keys, timings.sessions
    );
    if !report.identical {
        let outcome = &report.outcome;
        let _ = writeln!(
            stderr,
            "togi-tool-bench: base {} cached, {} run; head {} cached, {} run",
            outcome.cached[0], outcome.scheduled[0], outcome.cached[1], outcome.scheduled[1]
        );
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
    if report.different() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
