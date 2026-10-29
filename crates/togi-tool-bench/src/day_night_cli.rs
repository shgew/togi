mod successor;

use std::fs::File;
use std::io::{self, Write};
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::Instant;

use clap::{Args, ValueEnum};
use togi_tool_bench::Selection;
use togi_tool_bench::day_night::Registration;
use togi_tool_bench::day_night_gate::{
    SelectError, Verdict, any_fails_gate, render_summary, render_verdict, select_policies,
};
use togi_tool_bench::day_night_report::{ComparatorError, ScoreError, report, run_policies};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Split {
    Dev,
    Holdout,
    All,
}

#[derive(Debug, Args)]
pub(crate) struct DayNightArgs {
    /// Preregistered scenarios, seed ranges, calendar and thresholds.
    #[arg(
        long,
        value_name = "FILE",
        default_value = "tools/bench/day-night.json"
    )]
    registration: PathBuf,
    /// Develop on dev; successor refuses holdout/all outside its named once-only procedure.
    #[arg(long, value_enum, default_value = "dev")]
    split: Split,
    /// Preserve JSON Lines; required for successor runs, with FILE.manifest.json beside it.
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
    /// Frozen registration: repeat, or default to co-zero and fixed-margin.
    /// Successor runs: exactly one co-zero or ruleset13; --score-from selects a pinned policy.
    #[arg(long = "policy", value_name = "NAME")]
    policies: Vec<String>,
    /// Registered evaluation slope (also Ruleset 13's fallback); co-zero uses it only as a cell label.
    #[arg(long, value_name = "SLOPE")]
    slope: Option<f64>,
    /// Concurrent runs. Frozen default: available parallelism; successor: fixed registered count.
    #[arg(long, value_name = "N")]
    jobs: Option<NonZeroUsize>,
    /// Successor only, required: reduced-dev is exploratory; registered requires every slope.
    #[arg(long, value_enum)]
    run_profile: Option<Profile>,
    /// Successor tuning-unit mode; only the registered CCD-uniform comparator mode is enabled.
    #[arg(long, value_enum)]
    mode: Option<Mode>,
    /// Compilation elapsed seconds supplied by the launcher; direct prebuilt runs use zero.
    #[arg(long, hide = true, default_value_t = 0.0)]
    compilation_seconds: f64,
    /// Successor only: score retained JSONL instead of running; repeat to combine slope files.
    /// Omit --slope to select every registered slope. Does not execute a policy or unseal holdout.
    #[arg(long, value_name = "FILE")]
    score_from: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Profile {
    ReducedDev,
    Registered,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum Mode {
    CcdUniform,
    PerCore,
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("load day/night experiment")]
    Registration(#[from] togi_tool_bench::day_night::Error),
    #[error("select day/night policies")]
    Select(#[from] SelectError),
    #[error("run day/night policies")]
    Comparators(#[from] ComparatorError),
    #[error("score day/night policies")]
    Score(#[from] ScoreError),
    #[error("write day/night results")]
    Write(#[from] io::Error),
    #[error("execute successor day/night diagnostics")]
    Successor(#[from] successor::Error),
}

pub(crate) fn run(args: DayNightArgs) -> ExitCode {
    match execute(args) {
        Ok(false) => ExitCode::SUCCESS,
        Ok(true) => ExitCode::FAILURE,
        Err(error) => crate::fail(&togi_machine::error_chain(&error)),
    }
}

/// Returns whether a gating policy failed the gate.
fn execute(args: DayNightArgs) -> Result<bool, Error> {
    let bytes = std::fs::read(&args.registration)?;
    let identity: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(successor::Error::Decode)?;
    if identity.get("id").and_then(serde_json::Value::as_str) != Some("ruleset-13-day-night") {
        return successor::execute(&args, &bytes).map_err(Error::from);
    }
    if args.run_profile.is_some() || args.mode.is_some() || args.compilation_seconds != 0.0
        || !args.score_from.is_empty()
    {
        return Err(successor::Error::Invalid(
            "successor-only flags require a successor id/version registration",
        ).into());
    }
    let registration = Registration::load(&args.registration)?;
    let policies = select_policies(&registration, &args.policies, args.slope)?;
    let selection = match args.split {
        Split::Dev => Selection::Dev,
        Split::Holdout => Selection::Holdout,
        Split::All => Selection::All,
    };
    let jobs = args
        .jobs
        .or_else(|| thread::available_parallelism().ok())
        .unwrap_or(NonZeroUsize::MIN);
    let mut output = io::BufWriter::new(match args.out {
        Some(path) => Box::new(File::create(path)?) as Box<dyn Write>,
        None => Box::new(io::sink()),
    });
    let started = Instant::now();
    let records = run_policies(
        &registration,
        selection,
        &policies,
        jobs,
        &togi_simrun::day_night::run,
        &mut output,
    )?;
    let total = started.elapsed();
    output.flush()?;
    let mut stdout = io::stdout().lock();
    report(&mut stdout, &registration, &policies, &records)?;
    writeln!(stdout)?;
    let mut verdicts = Vec::with_capacity(policies.len());
    for policy in &policies {
        let verdict = Verdict::compute(&registration, selection, *policy, &records)?;
        render_verdict(&mut stdout, &registration, &verdict)?;
        writeln!(stdout)?;
        verdicts.push(verdict);
    }
    render_summary(&mut stdout, &registration, &policies, &records, total)?;
    Ok(any_fails_gate(&verdicts))
}
