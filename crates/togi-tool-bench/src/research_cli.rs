use std::io::{self, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Subcommand};
use togi_tool_bench::research::{self, End, Outcome, ResearchError, Role, StopReason};

#[derive(Debug, Args)]
pub(crate) struct ResearchArgs {
    /// The issue whose approved brief the campaign follows.
    issue: u32,
    #[command(subcommand)]
    action: Action,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// Validate a launch manifest against the tree; create nothing.
    Check {
        #[arg(long, value_name = "FILE")]
        manifest: PathBuf,
    },
    /// Validate a launch manifest, create its run directory and start the ledger.
    Start {
        #[arg(long, value_name = "FILE")]
        manifest: PathBuf,
    },
    /// Record runs a killed dispatcher left unfinished as incomplete, then continue the campaign.
    Resume {
        #[arg(long, value_name = "DIR")]
        run: PathBuf,
    },
    /// Check the campaign's limits, pinned files and writable paths, then run one allowed evaluator recipe and record it.
    Exec {
        #[arg(long, value_name = "DIR")]
        run: PathBuf,
        /// The hypothesis id the run serves.
        #[arg(long)]
        candidate: String,
        #[arg(long, value_enum)]
        role: Role,
        /// The `just` recipe and its arguments, after `--`.
        #[arg(last = true, required = true, value_name = "RECIPE ARGS")]
        command: Vec<String>,
    },
    /// Record a candidate's scientific outcome, decided by the brief's keep rule from the runs' metrics.
    Outcome {
        #[arg(long, value_name = "DIR")]
        run: PathBuf,
        #[arg(long)]
        candidate: String,
        #[arg(long, value_enum)]
        outcome: Outcome,
        #[arg(long)]
        reason: String,
        /// A JSON object of metric names to full-precision numbers; required unless the outcome is invalid.
        #[arg(long, value_name = "FILE")]
        metrics: Option<PathBuf>,
    },
    /// End the campaign with a reason.
    Stop {
        #[arg(long, value_name = "DIR")]
        run: PathBuf,
        #[arg(long, value_enum)]
        reason: StopReason,
        #[arg(long, default_value = "")]
        detail: String,
    },
    /// Write report.md from the ledger, with the researcher's notes on what remains uncertain.
    Report {
        #[arg(long, value_name = "DIR")]
        run: PathBuf,
        #[arg(long, value_name = "FILE")]
        notes: Option<PathBuf>,
    },
}

pub(crate) fn run(args: ResearchArgs) -> ExitCode {
    let issue = args.issue;
    let result = match args.action {
        Action::Check { manifest } => research::check(issue, &manifest).map(|manifest| {
            format!(
                "manifest valid: issue {}, {}",
                manifest.issue, manifest.output
            )
        }),
        Action::Start { manifest } => {
            research::start_run(issue, &manifest).map(|path| format!("started {}", path.display()))
        }
        Action::Resume { run } => research::resume(issue, &run).map(|incomplete| {
            if incomplete.is_empty() {
                "resumed".to_owned()
            } else {
                format!("resumed; runs {incomplete:?} recorded incomplete")
            }
        }),
        Action::Exec {
            run,
            candidate,
            role,
            command,
        } => {
            let Some((recipe, rest)) = command.split_first() else {
                return fail(&"exec needs a recipe after --");
            };
            match research::exec(issue, &run, &candidate, role, recipe, rest) {
                Ok(End::Exited { code: 0 }) => Ok("run complete".to_owned()),
                Ok(end) => {
                    let _ = writeln!(io::stderr(), "research: run {}", end.describe());
                    return ExitCode::FAILURE;
                }
                Err(error) => Err(error),
            }
        }
        Action::Outcome {
            run,
            candidate,
            outcome,
            reason,
            metrics,
        } => research::outcome(
            issue,
            &run,
            &candidate,
            outcome,
            &reason,
            metrics.as_deref(),
        )
        .map(|()| format!("recorded {candidate}: {}", outcome.name())),
        Action::Stop {
            run,
            reason,
            detail,
        } => research::stop(issue, &run, reason, &detail)
            .map(|()| format!("stopped: {}", reason.name())),
        Action::Report { run, notes } => research::write_report(issue, &run, notes.as_deref())
            .map(|path| format!("wrote {}", path.display())),
    };
    match result {
        Ok(message) => {
            let _ = writeln!(io::stdout(), "{message}");
            ExitCode::SUCCESS
        }
        Err(error) => fail(&chain(&error)),
    }
}

fn chain(error: &ResearchError) -> String {
    let mut text = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        text.push_str(": ");
        text.push_str(&cause.to_string());
        source = cause.source();
    }
    text
}

fn fail(error: &dyn std::fmt::Display) -> ExitCode {
    let _ = writeln!(io::stderr(), "togi-tool-bench research: {error}");
    ExitCode::FAILURE
}
