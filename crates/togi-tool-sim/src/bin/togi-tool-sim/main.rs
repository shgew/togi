//! `togi-tool-sim`: one seeded simulated tuning session without hardware or root, or the suite check that runs
//! every session of `tools/bench/suite.json` by shard and proves each ran once and ended clean.

mod log;
mod machine;
mod session;
mod signals;
mod suite;

#[cfg(test)]
mod tests;

use std::io::Write as _;
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use togi_journal::{Build, FixStamp, SCHEMA};

use crate::log::Log;

const USAGE: u8 = 2;

#[derive(Debug, Parser)]
#[command(
    name = "togi-tool-sim",
    version = togi_version::version(),
    about = "Run a seeded simulated tuning session, or check the simulator suite",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    session: SessionArgs,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run every session of one shard of the suite and write the shard's manifest.
    Suite(SuiteArgs),
    /// Check that the shards' manifests cover every session of the suite once and that each ended clean.
    Check(CheckArgs),
}

#[derive(Debug, Args)]
struct SessionArgs {
    /// Draw the simulated machine's limits and failures from this seed.
    #[arg(long, default_value_t = 1)]
    seed: u64,
    /// Load the simulated machine from this JSON file; default the seeded default machine.
    #[arg(long, value_name = "FILE")]
    machine: Option<PathBuf>,
    /// Answer exact class and profile matches from the machine's same-BIOS facts extract.
    #[arg(long)]
    replay_facts: bool,
    /// Stop after N clean cycles, each a passed full cycle ending at or after phase 2's confirmation cycle.
    #[arg(long, value_name = "N", default_value = "1")]
    cycles: NonZeroU32,
    /// Use this state directory, resuming the journal it holds under the configuration that journal recorded;
    /// default a new temporary directory, which is kept and announced.
    #[arg(long, value_name = "DIR")]
    state_dir: Option<PathBuf>,
    /// Write trials/<trial-id>/samples.jsonl in the state directory, gzip-compressed on trial end.
    #[arg(long)]
    samples: bool,
    /// Exit with status 3 if the session is still running after N simulated boots.
    #[arg(long, value_name = "N", default_value = "1000")]
    max_boots: NonZeroU32,
    /// After every Nth simulated crash, fail unless the state the next boot resumes from equals a fresh fold of the
    /// whole journal; 0 checks only the last boot.
    #[arg(long, value_name = "N", default_value_t = 0)]
    verify_every: u32,
    /// Fold the whole journal at every boot, as togi run does, instead of resuming from the state the crashed boot
    /// folded.
    #[arg(long)]
    cold_boots: bool,
}

#[derive(Debug, Args)]
struct SuiteArgs {
    #[arg(long, value_name = "FILE")]
    suite: PathBuf,
    /// The number of shards the suite is split into.
    #[arg(long, value_name = "N", default_value = "1")]
    shards: NonZeroUsize,
    /// The shard to run, counted from 0.
    #[arg(long, value_name = "I", default_value_t = 0)]
    shard: usize,
    /// The directory the shard's manifest, shard-I.jsonl, is written to.
    #[arg(long, value_name = "DIR")]
    out: PathBuf,
    /// Fail a session still running after N simulated boots.
    #[arg(long, value_name = "N", default_value = "1000")]
    max_boots: NonZeroU32,
    /// Run N sessions at once; default the available parallelism. The manifest does not depend on it.
    #[arg(long, value_name = "N")]
    jobs: Option<NonZeroUsize>,
}

#[derive(Debug, Args)]
struct CheckArgs {
    #[arg(long, value_name = "FILE")]
    suite: PathBuf,
    /// The number of shards the suite was split into; every one must have a manifest.
    #[arg(long, value_name = "N")]
    shards: NonZeroUsize,
    /// Check only this shard's manifest, against exactly the runs the partition gives it.
    #[arg(long, value_name = "I")]
    shard: Option<usize>,
    /// The directory holding the shards' shard-I.jsonl and nothing else.
    manifests: PathBuf,
}

/// The build every simulated session records.
fn build() -> Build {
    let rev = togi_version::rev();
    Build {
        version: togi_version::version().to_owned(),
        rev: (!rev.is_empty()).then(|| rev.to_owned()),
        ruleset: togi_tuner::RULESET,
        schema: SCHEMA,
        fixes: FixStamp::none(),
        evidence: togi_tuner::EVIDENCE_EPOCH,
    }
}

fn main() -> ExitCode {
    let mut log = Log::stderr();
    let code = match parse(std::env::args_os(), &mut log) {
        Ok(cli) => match cli.command {
            None => session::run(cli.session, &mut log),
            Some(Command::Suite(args)) => suite::run(&args, &mut log),
            Some(Command::Check(args)) => suite::check(&args, &mut log),
        },
        Err(code) => code,
    };
    match log.finish() {
        Ok(()) => ExitCode::from(code),
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "sim: write the log: {error}");
            ExitCode::FAILURE
        }
    }
}

fn parse(
    args: impl IntoIterator<Item = impl Into<std::ffi::OsString> + Clone>,
    log: &mut Log,
) -> Result<Cli, u8> {
    Cli::try_parse_from(args).map_err(|error| {
        let _ = write!(log, "{}", error.render());
        if error.use_stderr() { USAGE } else { 0 }
    })
}
