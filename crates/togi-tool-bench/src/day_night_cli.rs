use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, ValueEnum};
use togi_tool_bench::Selection;
use togi_tool_bench::day_night::Registration;
use togi_tool_bench::day_night_report::{ComparatorError, report, run_comparators};

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
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("load day/night experiment")]
    Registration(#[from] togi_tool_bench::day_night::Error),
    #[error("run day/night comparators")]
    Comparators(#[from] ComparatorError),
    #[error("write day/night results")]
    Write(#[from] io::Error),
}

pub(crate) fn run(args: DayNightArgs) -> ExitCode {
    match execute(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => crate::fail(&togi_machine::error_chain(&error)),
    }
}

fn execute(args: DayNightArgs) -> Result<(), Error> {
    let registration = Registration::load(&args.registration)?;
    let selection = match args.split {
        Split::Dev => Selection::Dev,
        Split::Holdout => Selection::Holdout,
        Split::All => Selection::All,
    };
    let mut output = io::BufWriter::new(match args.out {
        Some(path) => Box::new(File::create(path)?) as Box<dyn Write>,
        None => Box::new(io::sink()),
    });
    let records = run_comparators(
        &registration,
        selection,
        &mut togi_simrun::day_night::run,
        &mut output,
    )?;
    output.flush()?;
    report(&mut io::stdout().lock(), &registration, &records)?;
    Ok(())
}
