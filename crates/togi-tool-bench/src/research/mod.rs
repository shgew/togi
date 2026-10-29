//! `research`: the dispatcher of a bounded research campaign ([Research campaigns] in `docs/benchmarking.md`).
//!
//! It runs no agent and chooses no experiment. The researcher, an agent the owner started, calls it for each
//! step: `start` validates the launch manifest against the tree and opens the run directory; `exec` checks the
//! campaign's limits, pinned files, writable paths and the run's role, then runs one allowed evaluator and
//! keeps its raw output; `outcome` records the researcher's typed decision once the runs it rests on are
//! complete and paired; `stop` and `report` end the campaign. Every step appends to `experiments.jsonl`, which
//! is never rewritten. A check that fails after the campaign started stops it with a [`StopReason`].
//!
//! The manifest and diff checks catch mistakes; they are not a security sandbox.

mod campaign;
mod command;
mod ledger;
mod manifest;
mod report;
mod run;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub use campaign::{Campaign, Halt, Planned, Refusal, start};
pub use command::CommandError;
pub use ledger::{
    Decision, End, Ended, Ledger, LedgerError, Outcome, Recipe, Record, Role, Started, StopReason,
};
pub use manifest::{
    Hypothesis, Input, Issue, Limits, Manifest, ManifestError, Mode, Pinned, Split, SuiteSelection,
    sha256,
};
pub use report::render as render_report;
pub use run::{
    ResearchError, RunDir, check, exec, outcome, report as write_report, resume, start_run, stop,
};

pub trait Workspace {
    /// The checkout's root directory.
    fn root(&self) -> &Path;
    /// The full commit hash of `HEAD`.
    fn head(&self) -> Result<String, WorkspaceError>;
    /// Whether `ancestor` is `descendant` or one of its ancestors.
    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool, WorkspaceError>;
    /// Every path whose content differs from `base`: committed since, staged, unstaged, deleted or untracked
    /// and not ignored.
    fn changed(&self, base: &str) -> Result<BTreeSet<String>, WorkspaceError>;
    fn read(&self, path: &str) -> Result<Vec<u8>, WorkspaceError>;
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    #[error("{0} does not exist")]
    Missing(String),
    #[error("read {path}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("git {command}: {message}")]
    Git { command: String, message: String },
}

/// The checkout through git, rooted at the repository's top level.
#[derive(Debug)]
pub struct GitWorkspace {
    pub root: PathBuf,
}

impl GitWorkspace {
    /// The checkout containing the current directory.
    pub fn current() -> Result<Self, WorkspaceError> {
        let root = git_text(Path::new("."), &["rev-parse", "--show-toplevel"])?;
        Ok(Self {
            root: PathBuf::from(root),
        })
    }
}

impl Workspace for GitWorkspace {
    fn root(&self) -> &Path {
        &self.root
    }

    fn head(&self) -> Result<String, WorkspaceError> {
        git_text(&self.root, &["rev-parse", "HEAD"])
    }

    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool, WorkspaceError> {
        let output = git(
            &self.root,
            &["merge-base", "--is-ancestor", ancestor, descendant],
        )?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(WorkspaceError::Git {
                command: format!("merge-base --is-ancestor {ancestor} {descendant}"),
                message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            }),
        }
    }

    fn changed(&self, base: &str) -> Result<BTreeSet<String>, WorkspaceError> {
        let tracked = checked(
            &self.root,
            &["diff", "--name-only", "--no-renames", "-z", base],
        )?;
        let untracked = checked(
            &self.root,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )?;
        Ok(tracked
            .split(|&byte| byte == 0)
            .chain(untracked.split(|&byte| byte == 0))
            .filter(|path| !path.is_empty())
            .map(|path| String::from_utf8_lossy(path).into_owned())
            .collect())
    }

    fn read(&self, path: &str) -> Result<Vec<u8>, WorkspaceError> {
        fs::read(self.root.join(path)).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                WorkspaceError::Missing(path.to_owned())
            } else {
                WorkspaceError::Read {
                    path: path.to_owned(),
                    source,
                }
            }
        })
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the dispatcher reads the checkout's revision and changes from git, as the bench does"
)]
fn git(dir: &Path, args: &[&str]) -> Result<std::process::Output, WorkspaceError> {
    Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .map_err(|error| WorkspaceError::Git {
            command: args.join(" "),
            message: error.to_string(),
        })
}

fn checked(dir: &Path, args: &[&str]) -> Result<Vec<u8>, WorkspaceError> {
    let output = git(dir, args)?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(WorkspaceError::Git {
            command: args.join(" "),
            message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

fn git_text(dir: &Path, args: &[&str]) -> Result<String, WorkspaceError> {
    Ok(String::from_utf8_lossy(&checked(dir, args)?)
        .trim()
        .to_owned())
}
