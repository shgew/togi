use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, ValueEnum};
use togi_simrun::day_night::{CalendarConfig, Comparator};
use togi_tool_bench::day_night::Registration;
use togi_tool_bench::day_night_report::{Record, report};

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
    #[error("run day/night comparator")]
    Run(#[from] togi_simrun::day_night::RunError),
    #[error("write day/night results")]
    Write(#[from] io::Error),
    #[error("encode day/night results")]
    Encode(#[from] serde_json::Error),
}

pub(crate) fn run(args: DayNightArgs) -> ExitCode {
    match execute(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => crate::fail(&togi_machine::error_chain(&error)),
    }
}

fn execute(args: DayNightArgs) -> Result<(), Error> {
    let registration = Registration::load(&args.registration)?;
    let ranges = match args.split {
        Split::Dev => vec![&registration.seeds.dev],
        Split::Holdout => vec![&registration.seeds.holdout],
        Split::All => vec![&registration.seeds.dev, &registration.seeds.holdout],
    };
    let mut output = args
        .out
        .map(File::create)
        .transpose()?
        .map(io::BufWriter::new);
    let mut records = Vec::new();
    for scenario in &registration.scenarios {
        for range in &ranges {
            for seed in range.values()? {
                for (label, comparator) in [
                    ("co-zero", Comparator::CoZero),
                    ("fixed-margin", Comparator::FixedMargin),
                ] {
                    let calendar = CalendarConfig {
                        days: registration.calendar.days,
                        year_days: registration.calendar.year_days,
                        lambda_per_hour: registration.lambda_per_hour,
                        active_hours: f64::from(registration.calendar.daily_hours),
                        night_hours: f64::from(registration.calendar.night_hours),
                        night_weekdays: registration.calendar.night_weekdays.clone(),
                        firmware_change_day: (scenario.name == "firmware-change")
                            .then_some(registration.calendar.firmware_change_day),
                        ..CalendarConfig::default()
                    };
                    let metrics =
                        togi_simrun::day_night::run(&scenario.name, seed, comparator, calendar)?;
                    let record = Record {
                        scenario: scenario.name.clone(),
                        seed,
                        comparator: label.to_owned(),
                        metrics,
                    };
                    if let Some(output) = &mut output {
                        serde_json::to_writer(&mut *output, &record)?;
                        writeln!(output)?;
                    }
                    records.push(record);
                }
            }
        }
    }
    if let Some(output) = &mut output {
        output.flush()?;
    }
    report(&mut io::stdout().lock(), &registration, &records)?;
    Ok(())
}
