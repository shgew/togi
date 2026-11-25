//! `togi-tool-sim`: run simulated tuning sessions without hardware or root.

mod content;
mod log;
mod scripted;
mod session;
mod signals;

#[cfg(test)]
mod tests;

use std::io::Write as _;
use std::num::NonZeroU32;
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
    about = "Run a simulated tuning session or a named mechanics script",
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

#[derive(Debug, Args)]
struct SessionArgs {
    /// Seed simulated boot IDs.
    #[arg(long, default_value_t = 1)]
    seed: u64,
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
