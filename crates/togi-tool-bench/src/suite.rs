use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use togi_machine::object_only;
use togi_sim::replay::Replay;
use togi_sim::{InvalidMachine, MachineDefinition, Model, file};
use togi_tool_trialfacts::Extracts;

use crate::error::Error;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Split {
    Dev,
    Holdout,
}

impl Split {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Holdout => "holdout",
        }
    }
}

impl fmt::Display for Split {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The seeds a run selects from the suite.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Selection {
    Dev,
    Holdout,
    All,
}

impl Selection {
    const fn contains(self, split: Split) -> bool {
        matches!(
            (self, split),
            (Self::All, _) | (Self::Dev, Split::Dev) | (Self::Holdout, Split::Holdout)
        )
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SuiteError {
    #[error("read suite {}: {source}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("decode suite: {0}")]
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
    #[error("scenario {0}: replay needs a machine file naming a facts extract")]
    ReplayWithoutMachine(String),
    #[error("scenario {scenario} repeats seed {seed}")]
    RepeatedSeed { scenario: String, seed: u64 },
    #[error("scenario {scenario} smoke seed {seed} is in neither dev nor holdout")]
    SmokeOutsideSplits { scenario: String, seed: u64 },
    #[error("suite has no selected runs")]
    NoRuns,
}

object_only! {
    #[derive(Debug, Serialize)]
    #[serde(deny_unknown_fields)]
    struct SuiteFile {
        #[serde(default)]
        gate: Option<serde_json::Map<String, serde_json::Value>>,
        scenarios: Vec<ScenarioDef>,
    }

    #[derive(Debug, Serialize)]
    #[serde(deny_unknown_fields)]
    struct ScenarioDef {
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

/// A scenario after validation: its members as the suite wrote them, in order.
#[derive(Debug)]
struct Scenario {
    name: String,
    members: Vec<String>,
    replay: bool,
    dev: Vec<u64>,
    holdout: Vec<u64>,
    smoke: Vec<u64>,
}

/// A validated suite file: scenario names are safe unique path components, no seed repeats within a scenario and
/// every smoke seed belongs to a split. The optional gate is kept as the object the file holds; [`crate::gate::load`]
/// types and checks it.
#[derive(Debug)]
pub struct Suite {
    gate: Option<serde_json::Map<String, serde_json::Value>>,
    scenarios: Vec<Scenario>,
    base: PathBuf,
    digest: String,
}

impl Suite {
    pub fn load(path: &Path) -> Result<Self, SuiteError> {
        let bytes = fs::read(path).map_err(|source| SuiteError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let base = path.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
        Self::parse(&bytes, base)
    }

    /// Decodes a suite whose machine paths are relative to `base`, the directory of the suite file.
    pub fn parse(bytes: &[u8], base: PathBuf) -> Result<Self, SuiteError> {
        let file: SuiteFile = serde_json::from_slice(bytes)?;
        let mut names = BTreeSet::new();
        let mut scenarios = Vec::with_capacity(file.scenarios.len());
        for def in file.scenarios {
            if !valid_name(&def.name) {
                return Err(SuiteError::InvalidName(def.name));
            }
            if !names.insert(def.name.clone()) {
                return Err(SuiteError::DuplicateName(def.name));
            }
            scenarios.push(check(def)?);
        }
        Ok(Self {
            gate: file.gate,
            scenarios,
            base,
            digest: hex(&Sha256::digest(bytes)),
        })
    }

    /// The SHA-256 of the suite file's bytes, in lower-case hex.
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// The registered gate as the object the file holds; [`gate::load`](crate::gate::load) types it.
    pub(crate) const fn gate_object(&self) -> Option<&serde_json::Map<String, serde_json::Value>> {
        self.gate.as_ref()
    }

    /// A scenario's dev and holdout seeds, as the suite wrote them; none for a scenario the suite lacks.
    pub(crate) fn seeds_of(&self, name: &str) -> Option<(&[u64], &[u64])> {
        self.scenarios
            .iter()
            .find(|scenario| scenario.name == name)
            .map(|scenario| (scenario.dev.as_slice(), scenario.holdout.as_slice()))
    }

    /// The notes of the registered gate, in order; none for a suite without a gate or a gate without notes.
    pub fn notes(&self) -> Vec<String> {
        self.gate
            .as_ref()
            .and_then(|gate| gate.get("notes"))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|note| note.as_str().map(str::to_owned))
            .collect()
    }

    /// Every selected run, scenario by scenario in suite order, dev seeds before holdout seeds, with each
    /// machine file read once. An ensemble scenario's member is its seed's position within the split, modulo the
    /// member count, not the seed's value; a replay scenario's oracle is built from its member's facts extract.
    /// Every member of every scenario is loaded, selected or not, so a mistake in a split not run now surfaces now.
    pub fn plan(&self, selection: Selection) -> Result<Plan, Error> {
        let mut machines = Machines::default();
        let mut runs = Vec::new();
        for scenario in &self.scenarios {
            let mut members = Vec::with_capacity(scenario.members.len());
            for name in &scenario.members {
                let machine = machines.load(&scenario.name, name, &self.base)?;
                let replay = scenario
                    .replay
                    .then(|| machines.replay(&scenario.name, &machine))
                    .transpose()?;
                members.push((machine, replay));
            }
            for (split, seeds) in [
                (Split::Dev, &scenario.dev),
                (Split::Holdout, &scenario.holdout),
            ] {
                if !selection.contains(split) {
                    continue;
                }
                for (index, &seed) in seeds.iter().enumerate() {
                    let member = (!members.is_empty()).then(|| &members[index % members.len()]);
                    runs.push(RunSpec {
                        scenario: scenario.name.clone(),
                        split,
                        seed,
                        machine: member.map(|(machine, _)| Arc::clone(machine)),
                        replay: member.and_then(|(_, replay)| replay.clone()),
                        smoke: scenario.smoke.contains(&seed),
                    });
                }
            }
        }
        if runs.is_empty() {
            return Err(SuiteError::NoRuns.into());
        }
        Ok(Plan {
            runs,
            machines: machines.loaded.into_values().collect(),
        })
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

fn check(def: ScenarioDef) -> Result<Scenario, SuiteError> {
    if !def.machine.is_empty() && !def.machines.is_empty() {
        return Err(SuiteError::MachineAndMachines(def.name));
    }
    if def.machines.iter().any(String::is_empty) {
        return Err(SuiteError::EmptyMachine(def.name));
    }
    let members = if def.machine.is_empty() {
        def.machines
    } else {
        vec![def.machine]
    };
    if def.replay && members.is_empty() {
        return Err(SuiteError::ReplayWithoutMachine(def.name));
    }
    let mut seeds = BTreeSet::new();
    for &seed in def.dev.iter().chain(&def.holdout) {
        if !seeds.insert(seed) {
            return Err(SuiteError::RepeatedSeed {
                scenario: def.name,
                seed,
            });
        }
    }
    if let Some(&seed) = def.smoke.iter().find(|seed| !seeds.contains(seed)) {
        return Err(SuiteError::SmokeOutsideSplits {
            scenario: def.name,
            seed,
        });
    }
    Ok(Scenario {
        name: def.name,
        members,
        replay: def.replay,
        dev: def.dev,
        holdout: def.holdout,
        smoke: def.smoke,
    })
}

/// A machine file read once and shared by every run that uses it, read-only.
#[derive(Debug)]
pub struct Machine {
    /// The path as the suite wrote it, relative to the suite file: how records name the machine.
    pub name: String,
    /// The path to read the machine from.
    pub path: PathBuf,
    pub definition: MachineDefinition,
    /// Every file the machine came from, the extract it declares included, each named relative to the suite file and
    /// by the SHA-256 of its content, in the order they were read.
    pub inputs: Vec<(PathBuf, Option<String>)>,
}

impl Machine {
    /// The model of this machine for `seed`.
    pub fn model(&self, seed: u64) -> Result<Model, InvalidMachine> {
        Model::new(&self.definition, seed)
    }

    /// The digest of every input of the machine: file names and contents, an absent file noted as absent.
    pub fn digest(&self) -> String {
        let mut hash = Sha256::new();
        for (path, sum) in &self.inputs {
            hash.update(format!(
                "file {} {}\n",
                path.display(),
                sum.as_deref().unwrap_or("absent")
            ));
        }
        hex(&hash.finalize())
    }
}

/// One session of the suite.
#[derive(Clone, Debug)]
pub struct RunSpec {
    pub scenario: String,
    pub split: Split,
    pub seed: u64,
    /// `None` runs the seeded default machine.
    pub machine: Option<Arc<Machine>>,
    pub replay: Option<Arc<Replay>>,
    pub smoke: bool,
}

impl RunSpec {
    /// `<scenario>/<split>-<seed>`: the run's name, and its directory below the run root.
    pub fn name(&self) -> String {
        format!("{}/{}-{}", self.scenario, self.split, self.seed)
    }

    /// The model the run's simulator draws from: its machine's, or the seeded default machine's.
    pub fn model(&self) -> Result<Model, InvalidMachine> {
        match &self.machine {
            Some(machine) => machine.model(self.seed),
            None => Model::new(&MachineDefinition::default(), self.seed),
        }
    }
}

#[derive(Debug)]
pub struct Plan {
    pub runs: Vec<RunSpec>,
    /// Every distinct machine of the suite, selected or not, ordered by path.
    pub machines: Vec<Arc<Machine>>,
}

#[derive(Default)]
struct Machines {
    loaded: BTreeMap<PathBuf, Arc<Machine>>,
    replays: BTreeMap<PathBuf, Arc<Replay>>,
    extracts: Extracts,
}

impl Machines {
    fn load(&mut self, scenario: &str, name: &str, base: &Path) -> Result<Arc<Machine>, Error> {
        let path = base.join(name);
        if let Some(machine) = self.loaded.get(&path) {
            return Ok(Arc::clone(machine));
        }
        let (definition, files) =
            file::load_with_files(&path).map_err(|source| Error::Machine {
                scenario: scenario.to_owned(),
                source,
            })?;
        let mut inputs = Vec::with_capacity(files.len() + 1);
        for file in files {
            inputs.push(sum(file, base)?);
        }
        if let Some(facts) = definition
            .facts
            .as_deref()
            .filter(|facts| !facts.is_empty())
        {
            let facts = path.parent().unwrap_or_else(|| Path::new("")).join(facts);
            inputs.push(sum(facts, base)?);
        }
        let machine = Arc::new(Machine {
            name: name.to_owned(),
            path: path.clone(),
            definition,
            inputs,
        });
        self.loaded.insert(path, Arc::clone(&machine));
        Ok(machine)
    }

    fn replay(&mut self, scenario: &str, machine: &Machine) -> Result<Arc<Replay>, Error> {
        if let Some(replay) = self.replays.get(&machine.path) {
            return Ok(Arc::clone(replay));
        }
        let replay = Arc::new(
            self.extracts
                .replay(&machine.path, &machine.definition)
                .map_err(|source| Error::Replay {
                    scenario: scenario.to_owned(),
                    source,
                })?,
        );
        self.replays
            .insert(machine.path.clone(), Arc::clone(&replay));
        Ok(replay)
    }
}

fn sum(path: PathBuf, base: &Path) -> Result<(PathBuf, Option<String>), Error> {
    let resolved = base.canonicalize().ok();
    let name = path
        .strip_prefix(base)
        .ok()
        .or_else(|| path.strip_prefix(resolved.as_deref()?).ok())
        .unwrap_or(&path)
        .to_path_buf();
    match fs::read(&path) {
        Ok(bytes) => Ok((name, Some(hex(&Sha256::digest(bytes))))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((name, None)),
        Err(source) => Err(Error::ReadFile {
            what: "hash input",
            path,
            source,
        }),
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}
