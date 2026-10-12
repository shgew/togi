use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use jiff::Timestamp;
use togi_tool_stats::{Error, Journal, report, score};

/// Summarize a current or archived journal for review, read-only: the journal and its unfinished final line
/// are left as they are.
#[derive(Debug, Parser)]
#[command(name = "togi-tool-stats")]
struct Cli {
    /// The state directory holding events.jsonl.
    #[arg(long, default_value = "/var/lib/togi")]
    state_dir: PathBuf,
    /// One current or archived journal file, instead of the state directory's.
    #[arg(long, conflicts_with = "forecast")]
    journal: Option<PathBuf>,
    /// Include what starts at or after this RFC 3339 time.
    #[arg(long, value_parser = parse_since, conflicts_with = "forecast")]
    since: Option<Timestamp>,
    /// Score the run in --state-dir against this forecast file from `bench --forecast`, instead of
    /// reporting. Refuses a state directory whose journal through the forecast's anchor changed.
    #[arg(long)]
    forecast: Option<PathBuf>,
}

fn parse_since(text: &str) -> Result<Timestamp, String> {
    text.parse::<Timestamp>().map_err(|error| error.to_string())
}

/// The report and what reading the journal skipped, one line each, for stderr.
fn run(cli: &Cli) -> Result<(String, Vec<String>), Error> {
    if let Some(forecast) = &cli.forecast {
        let scored = score(&cli.state_dir, forecast)?;
        let skipped = scored
            .skipped
            .iter()
            .map(|(session, skipped)| format!("session {session}: {skipped}"))
            .collect();
        return Ok((scored.text, skipped));
    }
    let path = cli
        .journal
        .clone()
        .unwrap_or_else(|| cli.state_dir.join("events.jsonl"));
    let journal = Journal::read(path)?;
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
            let _ = writeln!(std::io::stderr(), "togi-tool-stats: {error}");
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
