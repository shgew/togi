use std::io;
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args;
use togi_config::Config;
use togi_machine::{CoreId, CoreSet, InvalidCore, InvalidTarget, TargetOffset};
use togi_sim::{InvalidMachine, Model, file};
use togi_tool_bench::pass_cost::{
    Block, GRID_PER_HOUR, MachineError, MachineQuery, ObligationError, Settings, hazard_blocks,
    machine_blocks, report,
};

use crate::fail;

#[derive(Debug, Args)]
pub(crate) struct PassCostArgs {
    /// Constant hazards in failures per hour, comma-separated; default the #490 grid 0,0.1,0.5,1,2,4,8,16.
    #[arg(
        long = "hazard",
        value_name = "PER_HOUR",
        value_delimiter = ',',
        value_parser = hazard,
        conflicts_with = "machine"
    )]
    hazards: Vec<f64>,
    /// The CCDs of the machine the constant hazards describe, which set the long trials' durations.
    #[arg(
        long,
        value_name = "N",
        default_value = "2",
        conflicts_with = "machine"
    )]
    ccds: NonZeroUsize,
    /// A simulator machine file whose model gives each trial's failure probability instead of a constant hazard.
    #[arg(long, value_name = "FILE", requires = "profile")]
    machine: Option<PathBuf>,
    /// The fixed profile to evaluate the machine at, one offset per core, comma-separated.
    #[arg(
        long,
        value_name = "OFFSETS",
        value_delimiter = ',',
        allow_hyphen_values = true,
        requires = "machine"
    )]
    profile: Vec<i64>,
    /// The seed that draws the machine's hidden limits when its file sets none.
    #[arg(long, value_name = "N", default_value = "1", requires = "machine")]
    seed: u64,
    /// Evaluate only this R7 workload's step; default every R7 workload.
    #[arg(long, value_name = "ID", requires = "machine")]
    workload: Option<String>,
    /// Evaluate only the part that loads these cores, comma-separated, instead of each step's parts.
    #[arg(
        long,
        value_name = "CORES",
        value_delimiter = ',',
        requires = "machine"
    )]
    cores: Vec<i64>,
    /// The togi configuration whose durations set the requirements; default the built-in defaults.
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// The launch horizon.
    #[arg(long, value_name = "N", default_value = "40")]
    launches: u32,
    /// The time horizon in hours.
    #[arg(long, value_name = "HOURS", default_value = "4", value_parser = horizon_seconds)]
    hours: u32,
    /// Add a counterfactual row per part with N more short passes, comma-separated.
    #[arg(long = "extra-short", value_name = "N", value_delimiter = ',')]
    extra_short: Vec<NonZeroU32>,
}

#[derive(Debug, thiserror::Error)]
enum PassCostError {
    #[error("load the configuration")]
    Config(#[source] togi_config::LoadError),
    #[error("load the machine")]
    Machine(#[source] file::LoadError),
    #[error("build the machine model")]
    Model(#[source] InvalidMachine),
    #[error("read the profile")]
    Profile(#[source] InvalidTarget),
    #[error("read the cores")]
    Cores(#[source] InvalidCore),
    #[error("evaluate the constant hazards")]
    Hazards(#[source] ObligationError),
    #[error("evaluate the machine")]
    Evaluate(#[source] MachineError),
    #[error("write the report")]
    Write(#[source] io::Error),
}

fn hazard(text: &str) -> Result<f64, String> {
    match text.parse::<f64>() {
        Ok(value) if value.is_finite() && value >= 0.0 => Ok(value),
        _ => Err(format!(
            "{text} is not a nonnegative number of failures per hour"
        )),
    }
}

fn horizon_seconds(text: &str) -> Result<u32, String> {
    let seconds = text
        .parse::<f64>()
        .map(|hours| (hours * 3600.0).round())
        .ok()
        .filter(|seconds| (1.0..=f64::from(u32::MAX)).contains(seconds))
        .ok_or_else(|| format!("{text} is not a number of hours of at least one second"))?;
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "seconds is a whole number within 1..=u32::MAX here"
    )]
    Ok(seconds as u32)
}

pub(crate) fn run(args: PassCostArgs) -> ExitCode {
    match study(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(&togi_machine::error_chain(&error)),
    }
}

fn study(args: PassCostArgs) -> Result<(), PassCostError> {
    let config = match &args.config {
        Some(path) => Config::load(path).map_err(PassCostError::Config)?,
        None => Config::default(),
    };
    let mut header = vec![format!(
        "short_trial_s {}, checking_all_core_s {}",
        config.short_trial().get(),
        config.checking_all_core().get()
    )];
    let blocks = match &args.machine {
        None => {
            let hazards = if args.hazards.is_empty() {
                GRID_PER_HOUR.to_vec()
            } else {
                args.hazards.clone()
            };
            header.push(format!(
                "constant hazards on {} CCDs; a partial's requirement is its CCD part's",
                args.ccds
            ));
            hazard_blocks(&config, args.ccds.get(), &hazards).map_err(PassCostError::Hazards)
        }
        Some(path) => {
            header.push(format!(
                "machine {} seed {}, profile {}",
                path.display(),
                args.seed,
                args.profile
                    .iter()
                    .map(i64::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            ));
            machine(&config, path, &args)
        }
    }?;
    let settings = Settings {
        short_trial_s: config.short_trial().get(),
        launches: args.launches,
        seconds: args.hours,
        extra_short: args.extra_short,
    };
    report(&mut io::stdout().lock(), &header, &settings, &blocks).map_err(PassCostError::Write)
}

fn machine(config: &Config, path: &Path, args: &PassCostArgs) -> Result<Vec<Block>, PassCostError> {
    let definition = file::load(path).map_err(PassCostError::Machine)?;
    let model = Model::new(&definition, args.seed).map_err(PassCostError::Model)?;
    let profile = args
        .profile
        .iter()
        .map(|&offset| TargetOffset::new(offset))
        .collect::<Result<Vec<_>, _>>()
        .map_err(PassCostError::Profile)?;
    let cores = if args.cores.is_empty() {
        None
    } else {
        Some(
            args.cores
                .iter()
                .map(|&id| CoreId::new(id))
                .collect::<Result<CoreSet, _>>()
                .map_err(PassCostError::Cores)?,
        )
    };
    let query = MachineQuery {
        profile: &profile,
        workload: args.workload.as_deref(),
        cores,
    };
    machine_blocks(config, &model, &query).map_err(PassCostError::Evaluate)
}

#[cfg(test)]
mod tests {
    use super::horizon_seconds;

    #[test_case::test_case("0.0002777777777777778", Some(1); "one second")]
    #[test_case::test_case("1193046.4708333334", Some(u32::MAX); "largest whole second")]
    #[test_case::test_case("1193046.4711111111", None; "one second beyond the integer domain")]
    #[test_case::test_case("0", None; "zero")]
    #[test_case::test_case("-1", None; "negative")]
    #[test_case::test_case("NaN", None; "not a number")]
    #[test_case::test_case("inf", None; "infinite")]
    fn horizon_seconds_refuses_out_of_range_input(text: &str, expected: Option<u32>) {
        assert_eq!(horizon_seconds(text).ok(), expected);
    }
}
