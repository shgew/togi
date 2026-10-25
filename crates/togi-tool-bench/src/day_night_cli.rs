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
    /// Develop on dev; holdout is reserved for once-only confirmation.
    #[arg(long, value_enum, default_value = "dev")]
    split: Split,
    /// Preserve every comparator seed's metrics as JSON Lines.
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
    /// A policy to run, by name; repeat to run several. Default: every diagnostic comparator
    /// (co-zero, fixed-margin).
    #[arg(long = "policy", value_name = "NAME")]
    policies: Vec<String>,
    /// The fallback slope of a policy that takes one; must be a registered slope.
    #[arg(long, value_name = "SLOPE")]
    slope: Option<f64>,
    /// Run N scenario/seed/policy runs at once; default the available parallelism.
    #[arg(long, value_name = "N")]
    jobs: Option<NonZeroUsize>,
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
