use std::io::{self, Write as _};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
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
    /// Both trees must declare support for the same scripted input contract as this command.
    #[arg(value_name = "BASE")]
    base: PathBuf,
    /// The tree under test.
    #[arg(long, value_name = "DIR", default_value = ".")]
    head: PathBuf,
    /// Compare only the mapping-stop and mce-resume scripted cases.
    #[arg(long)]
    smoke: bool,
    #[command(flatten)]
    comparison: SemanticArgs,
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
    /// The directory of cached digests, session costs and the base's build; default the user cache directory's
    /// togi/bench.
    #[arg(long, value_name = "DIR")]
    cache: Option<PathBuf>,
    /// Use this contract-compatible simulator instead of building the base's.
    #[arg(long, value_name = "FILE")]
    base_sim: Option<PathBuf>,
    /// Use this contract-compatible simulator instead of building the head's.
    #[arg(long, value_name = "FILE")]
    head_sim: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct SemanticArgs {
    /// Compare retained journal semantics, rebasing sequence references on both sides.
    /// Runs every selected session freshly on both sides and reports all differing classes, including messages.
    #[arg(long)]
    semantic: bool,
    /// Drop records of this kind on the base side only. Repeat to retire more than one kind.
    #[arg(long, value_name = "KIND", requires = "semantic")]
    retired: Vec<String>,
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
        smoke: args.smoke,
        semantic: args.comparison.semantic.then_some(args.comparison.retired),
        keep_going: args.keep_going,
        no_cache: args.no_cache,
        keep: args.keep,
        jobs: args
            .jobs
            .or_else(|| thread::available_parallelism().ok())
            .unwrap_or(NonZeroUsize::MIN),
        timeout: Duration::from_secs(args.timeout.get()),
        cache,
        simulators: [args.base_sim, args.head_sim],
    };
    let mut stderr = io::stderr();
    let waiting = |lock: &Path| {
        let _ = writeln!(
            io::stderr(),
            "togi-tool-bench: waiting for another comparison's lock on {}",
            lock.display()
        );
    };
    let report = match execute(&options, &fleet, &waiting) {
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
    for warning in &report.warnings {
        let _ = writeln!(stderr, "togi-tool-bench: {warning}");
    }
    let timings = report.timings;
    let _ = writeln!(
        stderr,
        "togi-tool-bench: harness overhead: contracts {:.0?}, simulators {:.0?}, keys {:.0?}; sessions took {:.0?}",
        timings.contracts, timings.builds, timings.keys, timings.sessions
    );
    let outcome = &report.outcome;
    let _ = writeln!(
        stderr,
        "togi-tool-bench: base {} cached, {} run; head {} cached, {} run",
        outcome.cached[0], outcome.scheduled[0], outcome.cached[1], outcome.scheduled[1]
    );
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
