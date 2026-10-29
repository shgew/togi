use std::path::{Component, Path};

use super::ledger::{Recipe, Role};
use super::manifest::{Manifest, Mode};
use super::run::RECORD;

/// The committed baseline model development's F5 check pairs the committed machines' runs against.
const F5_BASELINE: &str = "tools/bench/baseline.jsonl";

/// Why the dispatcher refuses to run an evaluator.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("{recipe} is not an evaluator of {mode} campaigns")]
    Recipe { recipe: String, mode: &'static str },
    #[error("just {recipe} in a {role} run: {why}")]
    Argument {
        recipe: Recipe,
        role: &'static str,
        why: String,
    },
}

/// Which evaluator a campaign of `mode` may run. Model development runs `bench` only as its F5
/// finalization check, which [`arguments`] holds it to.
pub(super) fn recipe(mode: Mode, name: &str) -> Result<Recipe, CommandError> {
    let recipe = match (mode, name) {
        (Mode::ModelDevelopment, "forward") => Recipe::Forward,
        (Mode::ModelDevelopment, "fit") => Recipe::Fit,
        (_, "bench") => Recipe::Bench,
        _ => {
            return Err(CommandError::Recipe {
                recipe: name.to_owned(),
                mode: mode.name(),
            });
        }
    };
    Ok(recipe)
}

/// Whether an evaluator's output `path` lies inside the run directory `run`, below none of the campaign's own
/// record (`manifest.json`, `experiments.jsonl`, `lock`, `report.md`, `raw`) and without `..`.
fn inside_run(path: &str, run: &str) -> bool {
    let Some(rest) = path
        .strip_prefix(run)
        .and_then(|rest| rest.strip_prefix('/'))
    else {
        return false;
    };
    let mut components = Path::new(rest).components();
    let first = components.next();
    matches!(first, Some(Component::Normal(name)) if !RECORD.iter().any(|record| name == *record))
        && components.all(|component| matches!(component, Component::Normal(_)))
}

/// Checks a run's arguments against the manifest and the run's role, and returns them unchanged.
pub(super) fn arguments(
    manifest: &Manifest,
    recipe: Recipe,
    role: Role,
    args: &[String],
) -> Result<Vec<String>, CommandError> {
    let refuse = |why: String| CommandError::Argument {
        recipe,
        role: role.name(),
        why,
    };
    let flags = parse(args).map_err(refuse)?;
    let allowed: &[&str] = match recipe {
        Recipe::Forward => &["--facts", "--seal", "--jobs", "--records"],
        Recipe::Fit => &["--facts", "--out", "--jobs", "--seed", "--bootstrap"],
        Recipe::Bench => &[
            "--suite",
            "--split",
            "--out",
            "--keep",
            "--jobs",
            "--timeout",
            "--max-boots",
            "--no-cache",
            "--cache",
            "--baseline",
        ],
    };
    for (flag, _) in &flags {
        if !allowed.contains(&flag.as_str()) {
            return Err(refuse(format!(
                "{flag} is not allowed; allowed: {}",
                allowed.join(" ")
            )));
        }
        if flags.iter().filter(|(other, _)| other == flag).count() > 1 {
            return Err(refuse(format!("{flag} is given twice")));
        }
    }
    let value = |flag: &str| {
        flags
            .iter()
            .find(|(name, _)| name == flag)
            .map(|(_, value)| value.as_deref())
    };
    let output = |flag: &str| -> Result<(), CommandError> {
        match value(flag) {
            None => Ok(()),
            Some(Some(path)) if inside_run(path, &manifest.output) => Ok(()),
            Some(_) => Err(refuse(format!(
                "{flag} must name a path inside {}, outside its record",
                manifest.output
            ))),
        }
    };
    match recipe {
        Recipe::Forward | Recipe::Fit => {
            let Some(Some(facts)) = value("--facts") else {
                return Err(refuse(
                    "--facts must name a manifest input; the committed extract holds sessions no campaign may read"
                        .to_owned(),
                ));
            };
            let Some(input) = manifest.inputs.iter().find(|input| input.path == facts) else {
                return Err(refuse(format!("--facts {facts} is not a manifest input")));
            };
            let confirmation = input.confirmation.len();
            match (recipe, role) {
                (Recipe::Forward, Role::Reference | Role::Candidate) => {
                    let seal = value("--seal").flatten().map(str::parse::<usize>);
                    match seal {
                        None if confirmation == 0 => {}
                        Some(Ok(seal)) if seal == confirmation && seal > 0 => {}
                        _ => {
                            return Err(refuse(format!(
                                "development runs seal exactly the input's {confirmation} confirmation sessions"
                            )));
                        }
                    }
                    output("--records")?;
                }
                (Recipe::Forward, Role::Confirmation | Role::ConfirmationReference) => {
                    if confirmation == 0 {
                        return Err(refuse(format!("{facts} names no confirmation session")));
                    }
                    if value("--seal").is_some() {
                        return Err(refuse(
                            "a confirmation scores its sessions unsealed".to_owned(),
                        ));
                    }
                    output("--records")?;
                }
                (Recipe::Fit, Role::Finalization) => {
                    if value("--out").is_none() {
                        return Err(refuse(format!(
                            "--out must name a directory inside {}",
                            manifest.output
                        )));
                    }
                    output("--out")?;
                }
                (Recipe::Forward, Role::Finalization)
                | (
                    Recipe::Fit,
                    Role::Reference
                    | Role::Candidate
                    | Role::Confirmation
                    | Role::ConfirmationReference,
                ) => {
                    return Err(refuse("this evaluator does not serve this role".to_owned()));
                }
                (Recipe::Bench, _) => {}
            }
        }
        Recipe::Bench => match manifest.mode {
            Mode::ModelDevelopment => {
                if role != Role::Finalization {
                    return Err(refuse(
                        "model development runs bench only as the F5 finalization check".to_owned(),
                    ));
                }
                if value("--suite").is_some()
                    || value("--split") != Some(Some("all"))
                    || value("--baseline") != Some(Some(F5_BASELINE))
                {
                    return Err(refuse(format!(
                        "F5 runs the committed suite with --split all --baseline {F5_BASELINE}"
                    )));
                }
                for flag in ["--out", "--keep", "--cache"] {
                    output(flag)?;
                }
            }
            Mode::FixedPolicyObservation => {
                if !matches!(role, Role::Reference | Role::Candidate) {
                    return Err(refuse("observation runs have no confirmation".to_owned()));
                }
                let Some(suite) = &manifest.suite else {
                    return Err(refuse("the manifest names no suite".to_owned()));
                };
                if value("--suite") != Some(Some(suite.path.as_str())) {
                    return Err(refuse(format!(
                        "--suite must name the manifest's suite {}",
                        suite.path
                    )));
                }
                if value("--split") != Some(Some(suite.split.name())) {
                    return Err(refuse(format!(
                        "--split must be the manifest's {}",
                        suite.split.name()
                    )));
                }
                for flag in ["--out", "--keep", "--cache"] {
                    output(flag)?;
                }
            }
        },
    }
    Ok(args.to_vec())
}

/// Splits `--flag value`, `--flag=value` and bare `--flag` arguments. Values never start with `--`.
fn parse(args: &[String]) -> Result<Vec<(String, Option<String>)>, String> {
    let mut flags: Vec<(String, Option<String>)> = Vec::new();
    let mut args = args.iter().peekable();
    while let Some(arg) = args.next() {
        let Some(name) = arg.strip_prefix("--") else {
            return Err(format!("{arg:?} is not a --flag"));
        };
        if let Some((name, value)) = name.split_once('=') {
            flags.push((format!("--{name}"), Some(value.to_owned())));
        } else {
            let value = args.next_if(|next| !next.starts_with("--")).cloned();
            flags.push((arg.clone(), value));
        }
    }
    Ok(flags)
}
