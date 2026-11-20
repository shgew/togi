//! `togi-tool-sim`: run simulated tuning sessions without hardware or root, or verify the fitted research suite.

mod content;
mod log;
mod machine;
mod scripted;
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
use togi_tool_sim::scripted::{CONTRACT_VERSION, ScriptedCase};

use crate::log::Log;

const USAGE: u8 = 2;

#[derive(Debug, Parser)]
#[command(
    name = "togi-tool-sim",
    version = togi_version::version(),
    about = "Run a simulated tuning session, a named mechanics script, or the fitted research suite",
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
    /// Print the named scripts' input contract version.
    ScriptedContract,
    /// Run a named deterministic mechanics script.
    Scripted(ScriptedArgs),
    /// Run every session of one shard of the suite and write the shard's manifest.
    Suite(SuiteArgs),
    /// Check that the shards' manifests cover every session of the suite once and that each ended clean.
    Check(CheckArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ScriptedArgs {
    /// The mechanics case to run.
    #[arg(long, value_enum)]
    pub(crate) case: ScriptedCase,
    /// The input contract version the caller expects.
    #[arg(long, value_name = "VERSION")]
    pub(crate) contract: String,
    /// Write this case's state to this directory.
    #[arg(long, value_name = "DIR")]
    pub(crate) state_dir: PathBuf,
    /// Allow at most N simulated boots.
    #[arg(long, value_name = "N")]
    pub(crate) max_boots: NonZeroU32,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "independent command-line switches, not state-machine flags"
)]
#[derive(Debug, Args)]
struct SessionArgs {
    /// Seed simulated boot IDs and, for fitted machines, model draws.
    #[arg(long, default_value_t = 1)]
    seed: u64,
    /// Load a fitted research machine from this JSON file; otherwise use the developer mechanics script.
    #[arg(long, value_name = "FILE")]
    machine: Option<PathBuf>,
    /// Use the research suite's seeded default fitted machine, which its machine-free scenarios run, instead of the
    /// developer mechanics script.
    #[arg(long, conflicts_with = "machine")]
    default_fitted_machine: bool,
    /// Answer exact class and profile matches from the machine's same-BIOS facts extract.
    #[arg(long)]
    replay_facts: bool,
    /// Report the workload content identities the state directory's journal recorded, not the simulator's
    /// synthetic ones, so a recorded night can be continued without invalidating its evidence. The identities are
    /// assumed, not verified: nothing checks that the executables they name ran. Refused when the journal
    /// records none, records two for one workload, or lacks one for a workload the session runs.
    #[arg(long)]
    recorded_content: bool,
    /// The simulated operator ends the night after this many hours of simulated time, counted from the machine's
    /// clock when the command starts.
    #[arg(long, value_name = "HOURS", default_value = "8")]
    night_hours: NonZeroU32,
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
            Some(Command::ScriptedContract) => print_scripted_contract(&mut log),
            Some(Command::Scripted(args)) => scripted::run(args, &mut log),
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

fn print_scripted_contract(log: &mut Log) -> u8 {
    match writeln!(std::io::stdout().lock(), "{CONTRACT_VERSION}") {
        Ok(()) => 0,
        Err(error) => {
            let _ = writeln!(log, "sim: write the scripted contract version: {error}");
            1
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
