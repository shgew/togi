use std::collections::BTreeSet;
use std::io::Read as _;
use std::path::{Component, Path};

use flate2::read::MultiGzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use togi_machine::object_only;

use super::{Workspace, WorkspaceError};

/// What a campaign may change and what it ends in ([Research campaigns](../../../docs/benchmarking.md)).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Mode {
    ModelDevelopment,
    FixedPolicyObservation,
    CounterfactualAccounting,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Self::ModelDevelopment => "model development",
            Self::FixedPolicyObservation => "fixed-policy observation",
            Self::CounterfactualAccounting => "counterfactual accounting",
        }
    }
}

/// An issue with an approved brief: its mode, the real sessions it may read and its confirmation allowance.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Issue {
    StructuredSingleton,
    SharedVoltageDiagnosis,
    IntermittentHazard,
    Rederivation,
    OnlineRisk,
}

/// The original seven-session development artifact's first six sessions, which the model briefs develop on.
const DEVELOPMENT_SESSIONS: [&str; 6] = [
    "20260924T204352Z",
    "20260926T151414Z",
    "20260927T221954Z",
    "20260929T180308Z",
    "20261002T004254Z",
    "20261002T235548Z",
];

/// The original artifact's newest session: a historical, non-blind confirmation.
const HISTORICAL_CONFIRMATION: &str = "20261004T000701Z";

impl Issue {
    pub fn from_number(number: u32) -> Option<Self> {
        match number {
            427 => Some(Self::StructuredSingleton),
            480 => Some(Self::SharedVoltageDiagnosis),
            490 => Some(Self::IntermittentHazard),
            896 => Some(Self::Rederivation),
            503 => Some(Self::OnlineRisk),
            _ => None,
        }
    }

    pub fn number(self) -> u32 {
        match self {
            Self::StructuredSingleton => 427,
            Self::SharedVoltageDiagnosis => 480,
            Self::IntermittentHazard => 490,
            Self::Rederivation => 896,
            Self::OnlineRisk => 503,
        }
    }

    pub fn mode(self) -> Mode {
        match self {
            Self::StructuredSingleton | Self::SharedVoltageDiagnosis => Mode::ModelDevelopment,
            Self::IntermittentHazard | Self::OnlineRisk => Mode::FixedPolicyObservation,
            Self::Rederivation => Mode::CounterfactualAccounting,
        }
    }

    pub fn brief(self) -> &'static str {
        match self {
            Self::StructuredSingleton => "tools/fit/research/427.md",
            Self::SharedVoltageDiagnosis => "tools/fit/research/480.md",
            Self::IntermittentHazard => "tools/bench/research/490.md",
            Self::Rederivation => "tools/bench/research/896.md",
            Self::OnlineRisk => "tools/bench/research/503.md",
        }
    }

    fn development_sessions(self) -> &'static [&'static str] {
        match self {
            Self::StructuredSingleton | Self::SharedVoltageDiagnosis => &DEVELOPMENT_SESSIONS,
            Self::IntermittentHazard | Self::Rederivation | Self::OnlineRisk => &[],
        }
    }

    fn confirmation_sessions(self) -> &'static [&'static str] {
        match self {
            Self::StructuredSingleton => &[HISTORICAL_CONFIRMATION],
            Self::SharedVoltageDiagnosis
            | Self::IntermittentHazard
            | Self::Rederivation
            | Self::OnlineRisk => &[],
        }
    }

    pub fn max_confirmations(self) -> u32 {
        match self {
            Self::StructuredSingleton => 1,
            Self::SharedVoltageDiagnosis
            | Self::IntermittentHazard
            | Self::Rederivation
            | Self::OnlineRisk => 0,
        }
    }
}

/// Paths no campaign writes in any mode: evidence, fitted machines, baselines, the score, the run loop, the
/// hardware path, the tuner, the dispatcher, the programs and briefs, and the build environment.
const NEVER_WRITABLE: &[&str] = &[
    ".github",
    "Cargo.lock",
    "crates/togi-config",
    "crates/togi-detect",
    "crates/togi-hardware",
    "crates/togi-journal",
    "crates/togi-session",
    "crates/togi-smu",
    "crates/togi-tool-bench",
    "crates/togi-tool-fit/src/forward.rs",
    "crates/togi-tool-modelcheck",
    "crates/togi-tool-trialfacts",
    "crates/togi-trial",
    "crates/togi-tuner",
    "crates/togi-tuningboot",
    "flake.lock",
    "flake.nix",
    "justfile",
    "nix",
    "rust-toolchain.toml",
    "tools/bench",
    "tools/fit",
];

/// Paths observation and accounting campaigns also leave alone: the simulator and the fitter.
const MODEL_PATHS: &[&str] = &[
    "crates/togi-sim",
    "crates/togi-simrun",
    "crates/togi-tool-fit",
    "crates/togi-tool-sim",
];

/// Files every manifest pins: the build environment.
const ENVIRONMENT: &[&str] = &["Cargo.lock", "flake.lock", "rust-toolchain.toml"];

object_only! {
    /// The launch manifest of one campaign, as the owner writes it.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Manifest {
        pub issue: u32,
        pub mode: Mode,
        pub hypotheses: Vec<Hypothesis>,
        /// The full commit the campaign starts from.
        pub checkout: String,
        /// The full commit of the unchanged reference.
        pub reference: String,
        /// Real trial-fact inputs, by canonical hash: a gzip file's decompressed bytes, any other file's bytes.
        #[serde(default)]
        pub inputs: Vec<Input>,
        pub suite: Option<SuiteSelection>,
        pub frozen: Vec<Pinned>,
        pub writable: Vec<String>,
        #[serde(default)]
        pub limits: Limits,
        /// The run directory: an absolute path outside the checkout, which the campaign creates.
        pub output: String,
        pub confirmations: u32,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Hypothesis {
        pub id: String,
        pub statement: String,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Input {
        pub path: String,
        pub sha256: String,
        pub development: Vec<String>,
        #[serde(default)]
        pub confirmation: Vec<String>,
    }

    /// The suite whose hashed file fixes the scenarios and seeds, and the split a bench run selects.
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct SuiteSelection {
        pub path: String,
        pub sha256: String,
        pub split: Split,
    }

    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Pinned {
        pub path: String,
        pub sha256: String,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Limits {
        pub wall_minutes: u32,
        pub candidates: u32,
        pub jobs: u32,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Split {
    Dev,
    Holdout,
    All,
}

impl Split {
    pub fn name(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Holdout => "holdout",
            Self::All => "all",
        }
    }
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            wall_minutes: 8 * 60,
            candidates: 12,
            jobs: 4,
        }
    }
}

/// Why a manifest cannot start or continue a campaign.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("decode the manifest")]
    Decode(#[source] serde_json::Error),
    #[error("the manifest names issue {manifest}, the command issue {command}")]
    IssueMismatch { manifest: u32, command: u32 },
    #[error("issue {0} has no research brief")]
    NoBrief(u32),
    #[error("issue {issue} runs in {expected} mode, not {named}", expected = .expected.name(), named = .named.name())]
    WrongMode {
        issue: u32,
        expected: Mode,
        named: Mode,
    },
    #[error("the manifest is invalid: {0}")]
    Invalid(String),
    #[error("{path} is not approved for issue {issue}: {why}")]
    Session {
        issue: u32,
        path: String,
        why: String,
    },
    #[error("{path} changed: SHA-256 {actual}, the manifest pins {expected}")]
    Changed {
        path: String,
        expected: String,
        actual: String,
    },
    #[error("read {path}")]
    Read {
        path: String,
        #[source]
        source: WorkspaceError,
    },
    #[error("decompress {path}")]
    Decompress {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

impl Manifest {
    pub fn parse(bytes: &[u8]) -> Result<Self, ManifestError> {
        serde_json::from_slice(bytes).map_err(ManifestError::Decode)
    }

    /// Checks everything that does not read the tree: the issue, mode, queue, paths, limits and allowances.
    pub fn validate(&self, command_issue: u32) -> Result<Issue, ManifestError> {
        if self.issue != command_issue {
            return Err(ManifestError::IssueMismatch {
                manifest: self.issue,
                command: command_issue,
            });
        }
        let issue = Issue::from_number(self.issue).ok_or(ManifestError::NoBrief(self.issue))?;
        if self.mode != issue.mode() {
            return Err(ManifestError::WrongMode {
                issue: self.issue,
                expected: issue.mode(),
                named: self.mode,
            });
        }
        let invalid = |message: String| Err(ManifestError::Invalid(message));
        if self.hypotheses.is_empty() {
            return invalid("the hypothesis queue is empty".to_owned());
        }
        let mut ids = BTreeSet::new();
        for hypothesis in &self.hypotheses {
            if !is_name(&hypothesis.id) {
                return invalid(format!(
                    "hypothesis id {:?} is not a plain name",
                    hypothesis.id
                ));
            }
            if hypothesis.statement.trim().is_empty() {
                return invalid(format!("hypothesis {} states nothing", hypothesis.id));
            }
            if !ids.insert(hypothesis.id.as_str()) {
                return invalid(format!("hypothesis id {} appears twice", hypothesis.id));
            }
        }
        for (what, commit) in [("checkout", &self.checkout), ("reference", &self.reference)] {
            if commit.len() != 40
                || !commit
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            {
                return invalid(format!(
                    "{what} {commit:?} is not a full lowercase commit hash"
                ));
            }
        }
        if self.limits.wall_minutes == 0 || self.limits.candidates == 0 || self.limits.jobs == 0 {
            return invalid("every limit must be positive".to_owned());
        }
        if self.confirmations > issue.max_confirmations() {
            return invalid(format!(
                "issue {} allows {} confirmations, the manifest asks for {}",
                self.issue,
                issue.max_confirmations(),
                self.confirmations
            ));
        }
        let output = Path::new(&self.output);
        let absolute = output.is_absolute()
            && !self.output.ends_with('/')
            && output
                .components()
                .all(|component| matches!(component, Component::RootDir | Component::Normal(_)));
        if !absolute || !is_name(self.run_id()) {
            return invalid(format!(
                "output {:?} is not an absolute path ending in a plain run id",
                self.output
            ));
        }
        self.validate_data(issue)?;
        self.validate_paths(issue)?;
        Ok(issue)
    }

    fn validate_data(&self, issue: Issue) -> Result<(), ManifestError> {
        let invalid = |message: String| Err(ManifestError::Invalid(message));
        match issue.mode() {
            Mode::ModelDevelopment => {
                if self.inputs.is_empty() {
                    return invalid(
                        "model development reads at least one approved facts input".to_owned(),
                    );
                }
                if self.suite.is_some() {
                    return invalid("model development names no bench suite".to_owned());
                }
            }
            Mode::FixedPolicyObservation | Mode::CounterfactualAccounting => {
                if !self.inputs.is_empty() {
                    return invalid(format!("{} reads no real sessions", issue.mode().name()));
                }
                if self.suite.is_none() {
                    return invalid(format!(
                        "{} names its frozen research suite",
                        issue.mode().name()
                    ));
                }
            }
        }
        for input in &self.inputs {
            check_relative(&input.path)?;
            let session = |why: String| {
                Err(ManifestError::Session {
                    issue: self.issue,
                    path: input.path.clone(),
                    why,
                })
            };
            if input.development.is_empty() {
                return session("it names no development session".to_owned());
            }
            for id in &input.development {
                if !issue.development_sessions().contains(&id.as_str()) {
                    return session(format!("{id} is not an approved development session"));
                }
            }
            for id in &input.confirmation {
                if !issue.confirmation_sessions().contains(&id.as_str()) {
                    return session(format!("{id} is not an approved confirmation session"));
                }
            }
            let all: BTreeSet<_> = input
                .development
                .iter()
                .chain(&input.confirmation)
                .collect();
            if all.len() != input.development.len() + input.confirmation.len() {
                return session("a session is named twice".to_owned());
            }
        }
        if let Some(suite) = &self.suite {
            check_relative(&suite.path)?;
        }
        Ok(())
    }

    fn validate_paths(&self, issue: Issue) -> Result<(), ManifestError> {
        let invalid = |message: String| Err(ManifestError::Invalid(message));
        for pinned in &self.frozen {
            check_relative(&pinned.path)?;
        }
        for required in ENVIRONMENT.iter().copied().chain([issue.brief()]) {
            if !self.frozen.iter().any(|pinned| pinned.path == required) {
                return invalid(format!("frozen files must pin {required}"));
            }
        }
        let model_paths: &[&str] = match issue.mode() {
            Mode::ModelDevelopment => &[],
            Mode::FixedPolicyObservation | Mode::CounterfactualAccounting => MODEL_PATHS,
        };
        let pinned = self
            .inputs
            .iter()
            .map(|input| input.path.as_str())
            .chain(self.suite.iter().map(|suite| suite.path.as_str()))
            .chain(self.frozen.iter().map(|pinned| pinned.path.as_str()));
        let closed: Vec<&str> = NEVER_WRITABLE
            .iter()
            .chain(model_paths)
            .copied()
            .chain(pinned)
            .collect();
        for writable in &self.writable {
            check_relative(writable)?;
            if let Some(closed) = closed.iter().find(|closed| overlaps(writable, closed)) {
                return invalid(format!(
                    "writable path {writable} overlaps {closed}, which this campaign may not change"
                ));
            }
        }
        Ok(())
    }

    /// The run directory's name.
    pub fn run_id(&self) -> &str {
        self.output.rsplit('/').next().unwrap_or_default()
    }

    /// Whether `path`, relative to the checkout's root, is one the campaign may change.
    pub fn may_write(&self, path: &str) -> bool {
        self.writable.iter().any(|writable| within(path, writable))
    }

    /// Hashes every input, the suite and every frozen file, refusing the first that differs from the manifest,
    /// and checks that each facts input holds exactly its named sessions.
    pub fn check_tree(&self, workspace: &dyn Workspace) -> Result<(), ManifestError> {
        for input in &self.inputs {
            let bytes = canonical(workspace, &input.path)?;
            compare(&input.path, &input.sha256, &bytes)?;
            let sessions = sessions(&bytes).map_err(|why| ManifestError::Session {
                issue: self.issue,
                path: input.path.clone(),
                why,
            })?;
            let named: BTreeSet<&str> = input
                .development
                .iter()
                .chain(&input.confirmation)
                .map(String::as_str)
                .collect();
            let found: BTreeSet<&str> = sessions.iter().map(String::as_str).collect();
            if named != found {
                return Err(ManifestError::Session {
                    issue: self.issue,
                    path: input.path.clone(),
                    why: format!("it holds sessions {found:?}, the manifest names {named:?}"),
                });
            }
        }
        if let Some(suite) = &self.suite {
            compare(&suite.path, &suite.sha256, &read(workspace, &suite.path)?)?;
        }
        for pinned in &self.frozen {
            compare(
                &pinned.path,
                &pinned.sha256,
                &read(workspace, &pinned.path)?,
            )?;
        }
        Ok(())
    }

    /// The pinned hashes a run rests on, for its ledger record.
    pub fn pins(&self) -> Vec<Pinned> {
        self.inputs
            .iter()
            .map(|input| Pinned {
                path: input.path.clone(),
                sha256: input.sha256.clone(),
            })
            .chain(self.suite.iter().map(|suite| Pinned {
                path: suite.path.clone(),
                sha256: suite.sha256.clone(),
            }))
            .chain(self.frozen.iter().cloned())
            .collect()
    }
}

pub fn sha256(bytes: &[u8]) -> String {
    crate::suite::hex(&Sha256::digest(bytes))
}

fn is_name(name: &str) -> bool {
    name.bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn check_relative(path: &str) -> Result<(), ManifestError> {
    let normal = !path.is_empty()
        && !path.ends_with('/')
        && Path::new(path)
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
    if normal {
        Ok(())
    } else {
        Err(ManifestError::Invalid(format!(
            "{path:?} is not a plain relative path without a trailing slash"
        )))
    }
}

/// Whether `path` is `root` or lies under it, component by component.
pub(super) fn within(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn overlaps(left: &str, right: &str) -> bool {
    within(left, right) || within(right, left)
}

fn read(workspace: &dyn Workspace, path: &str) -> Result<Vec<u8>, ManifestError> {
    workspace.read(path).map_err(|source| ManifestError::Read {
        path: path.to_owned(),
        source,
    })
}

fn canonical(workspace: &dyn Workspace, path: &str) -> Result<Vec<u8>, ManifestError> {
    let bytes = read(workspace, path)?;
    if !Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gz"))
    {
        return Ok(bytes);
    }
    let mut rows = Vec::new();
    MultiGzDecoder::new(bytes.as_slice())
        .read_to_end(&mut rows)
        .map_err(|source| ManifestError::Decompress {
            path: path.to_owned(),
            source,
        })?;
    Ok(rows)
}

fn compare(path: &str, expected: &str, bytes: &[u8]) -> Result<(), ManifestError> {
    let actual = sha256(bytes);
    if actual == expected {
        Ok(())
    } else {
        Err(ManifestError::Changed {
            path: path.to_owned(),
            expected: expected.to_owned(),
            actual,
        })
    }
}

/// The distinct `session` fields of a facts extract's rows.
fn sessions(rows: &[u8]) -> Result<BTreeSet<String>, String> {
    #[derive(Deserialize)]
    struct Row {
        session: String,
    }
    let mut sessions = BTreeSet::new();
    for (index, line) in rows.split(|&byte| byte == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let row: Row = serde_json::from_slice(line)
            .map_err(|error| format!("row {} has no readable session: {error}", index + 1))?;
        sessions.insert(row.session);
    }
    Ok(sessions)
}
