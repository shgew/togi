use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use jiff::Timestamp;
use togi_tool_stats::{Error, Journal, report};

/// Summarize a current or archived journal for review, read-only: the journal and its unfinished final line
/// are left as they are.
#[derive(Debug, Parser)]
#[command(name = "togi-tool-stats")]
struct Cli {
    /// The state directory holding events.jsonl.
    #[arg(long, default_value = "/var/lib/togi")]
    state_dir: PathBuf,
    /// One current or archived journal file, instead of the state directory's.
    #[arg(long)]
    journal: Option<PathBuf>,
    /// Include what starts at or after this RFC 3339 time.
    #[arg(long, value_parser = parse_since)]
    since: Option<Timestamp>,
}

fn parse_since(text: &str) -> Result<Timestamp, String> {
    text.parse::<Timestamp>().map_err(|error| error.to_string())
}

/// The report and what reading the journal skipped, one line each, for stderr.
fn run(cli: &Cli) -> Result<(String, Vec<String>), Error> {
    let journal = match &cli.journal {
        Some(path) => Journal::read(path)?,
        None => Journal::read(cli.state_dir.join("events.jsonl"))?,
    };
    let skipped = Some(journal.skipped())
        .filter(|skipped| !skipped.is_empty())
        .map(ToString::to_string)
        .into_iter()
        .collect();
    Ok((report(&journal, cli.since), skipped))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let (text, skipped) = match run(&cli) {
        Ok(ran) => ran,
        Err(error) => {
            let _ = writeln!(
                std::io::stderr(),
                "togi-tool-stats: {}",
                togi_machine::error_chain(&error)
            );
            return ExitCode::FAILURE;
        }
    };
    for line in skipped {
        let _ = writeln!(std::io::stderr(), "togi-tool-stats: {line}");
    }
    match std::io::stdout().lock().write_all(text.as_bytes()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(
                std::io::stderr(),
                "togi-tool-stats: write the report: {error}"
            );
            ExitCode::FAILURE
        }
    }
}
