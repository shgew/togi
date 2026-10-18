use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;
use togi_machine::object_only;

use crate::{Run, RunId, Split};

#[derive(Debug, thiserror::Error)]
pub enum SuiteError {
    #[error("read suite {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("decode suite")]
    Decode(#[from] serde_json::Error),
    #[error(
        "invalid scenario name {0:?}: it must be one non-empty path component of letters, digits, '-', '_' and '.'"
    )]
    InvalidName(String),
    #[error("duplicate scenario {0:?}")]
    DuplicateName(String),
    #[error("scenario {0}: machine and machines are mutually exclusive")]
    MachineAndMachines(String),
    #[error("scenario {0}: machines lists an empty path")]
    EmptyMachine(String),
    #[error("scenario {scenario} repeats seed {seed}")]
    RepeatedSeed { scenario: String, seed: u64 },
    #[error("scenario {scenario} smoke seed {seed} is in neither dev nor holdout")]
    SmokeOutsideSplits { scenario: String, seed: u64 },
    #[error("suite has no runs")]
    NoRuns,
}

object_only! {
    #[derive(Debug, Serialize)]
    #[serde(deny_unknown_fields)]
    struct SuiteFile {
        /// The bench's gate (docs/benchmarking.md §The gate); the simulator does not judge it.
        #[serde(default)]
        gate: Option<serde_json::Map<String, serde_json::Value>>,
        scenarios: Vec<Scenario>,
    }

    #[derive(Debug, Serialize)]
    #[serde(deny_unknown_fields)]
    struct Scenario {
        name: String,
        #[serde(default)]
        machine: String,
        #[serde(default)]
        machines: Vec<String>,
        #[serde(default)]
        replay: bool,
        #[serde(default)]
        dev: Vec<u64>,
        #[serde(default)]
        holdout: Vec<u64>,
        #[serde(default)]
        smoke: Vec<u64>,
    }
}

/// A validated suite: every scenario name is a safe unique component, no seed repeats within a scenario, every smoke
/// seed belongs to a split, and at least one run exists.
#[derive(Debug)]
pub struct Suite {
    scenarios: Vec<Checked>,
}

#[derive(Debug)]
struct Checked {
    name: String,
    machines: Vec<PathBuf>,
    replay: bool,
    dev: Vec<u64>,
    holdout: Vec<u64>,
    smoke: Vec<u64>,
}

impl Suite {
    pub fn load(path: &Path) -> Result<Self, SuiteError> {
        Self::load_with(path, |path| std::fs::read_to_string(path))
    }

    /// Loads a suite through the supplied text reader, resolving machines beside `path`.
    pub fn load_with(
        path: &Path,
        read: impl FnOnce(&Path) -> std::io::Result<String>,
    ) -> Result<Self, SuiteError> {
        let text = read(path).map_err(|source| SuiteError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&text, path.parent().unwrap_or_else(|| Path::new("")))
    }

    /// Decodes a suite whose machine paths are relative to `base`, the directory of the suite file.
    pub fn parse(text: &str, base: &Path) -> Result<Self, SuiteError> {
        let file: SuiteFile = serde_json::from_str(text)?;
        let mut names = BTreeSet::new();
        let mut scenarios = Vec::with_capacity(file.scenarios.len());
        for scenario in file.scenarios {
            if !valid_name(&scenario.name) {
                return Err(SuiteError::InvalidName(scenario.name));
            }
            if !names.insert(scenario.name.clone()) {
                return Err(SuiteError::DuplicateName(scenario.name));
            }
            scenarios.push(check(scenario, base)?);
        }
        if scenarios
            .iter()
            .all(|s| s.dev.is_empty() && s.holdout.is_empty())
        {
            return Err(SuiteError::NoRuns);
        }
        Ok(Self { scenarios })
    }

    /// Every run, scenario by scenario in suite order, dev seeds before holdout seeds. An ensemble scenario's member
    /// is chosen by the seed's index modulo the member count, counted separately within each split.
    pub fn runs(&self) -> Vec<Run> {
        let mut runs = Vec::new();
        for scenario in &self.scenarios {
            for (split, seeds) in [
                (Split::Dev, &scenario.dev),
                (Split::Holdout, &scenario.holdout),
            ] {
                for (index, &seed) in seeds.iter().enumerate() {
                    let machine = (!scenario.machines.is_empty())
                        .then(|| scenario.machines[index % scenario.machines.len()].clone());
                    runs.push(Run {
                        id: RunId::new(&scenario.name, split, seed),
                        scenario: scenario.name.clone(),
                        split,
                        seed,
                        machine,
                        replay: scenario.replay,
                        smoke: scenario.smoke.contains(&seed),
                    });
                }
            }
        }
        runs
    }
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn check(scenario: Scenario, base: &Path) -> Result<Checked, SuiteError> {
    if !scenario.machine.is_empty() && !scenario.machines.is_empty() {
        return Err(SuiteError::MachineAndMachines(scenario.name));
    }
    if scenario.machines.iter().any(String::is_empty) {
        return Err(SuiteError::EmptyMachine(scenario.name));
    }
    let listed = if scenario.machine.is_empty() {
        scenario.machines
    } else {
        vec![scenario.machine]
    };
    let mut seeds = BTreeSet::new();
    for &seed in scenario.dev.iter().chain(&scenario.holdout) {
        if !seeds.insert(seed) {
            return Err(SuiteError::RepeatedSeed {
                scenario: scenario.name,
                seed,
            });
        }
    }
    if let Some(&seed) = scenario.smoke.iter().find(|seed| !seeds.contains(seed)) {
        return Err(SuiteError::SmokeOutsideSplits {
            scenario: scenario.name,
            seed,
        });
    }
    Ok(Checked {
        name: scenario.name,
        machines: listed.into_iter().map(|path| base.join(path)).collect(),
        replay: scenario.replay,
        dev: scenario.dev,
        holdout: scenario.holdout,
        smoke: scenario.smoke,
    })
}
