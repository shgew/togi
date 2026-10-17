//! togi's coverage tool (`just cover BASE`): from the LCOV trace of a `cargo llvm-cov` run and
//! `git diff` against the merge base, it prints the changed Rust lines no test reached as
//! `path:first-last` ranges, then names as `path: not built for <host triple>` each changed file
//! the coverage build leaves out in part or whole, by its dep-info files and the host's `cfg`
//! (ADRs 0030 and 0032). `just reviews snapshot --cover` reads that text.

pub mod cfg;
pub mod cli;
pub mod diff;
pub mod host;
pub mod lcov;
pub mod process;
pub mod report;
pub mod scan;
pub mod tree;
pub mod unbuilt;

#[cfg(test)]
#[path = "tests/fakes.rs"]
mod fakes;

use std::error::Error as StdError;
use std::io;
use std::path::PathBuf;

use crate::process::ProcessError;

/// A failure of the tool: each variant names the operation and keeps its cause as the source.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("supply --lcov, --build-dir and --base, and no positional arguments")]
    Usage,
    #[error("flag provided but not defined: {0}")]
    UnknownFlag(String),
    #[error("flag needs an argument: {0}")]
    MissingValue(String),
    #[error("read the lcov trace {}", path.display())]
    ReadTrace {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read the working directory")]
    WorkingDirectory(#[source] io::Error),
    #[error("diff against {base}")]
    Diff {
        base: String,
        #[source]
        source: ProcessError,
    },
    #[error("malformed hunk header {0:?}")]
    MalformedHunk(String),
    #[error("malformed lcov line {0:?}")]
    MalformedLcov(String),
    #[error("rustc {args}")]
    Rustc {
        args: &'static str,
        #[source]
        source: ProcessError,
    },
    #[error("rustc -vV names no host")]
    NoHost,
    #[error("{} holds no dep-info files in deps or build/*", .0.display())]
    NoDepInfo(PathBuf),
    #[error("read dep-info {}", path.display())]
    ReadDepInfo {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("read {path}")]
    ReadSource {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("compile the module declaration pattern")]
    Pattern(#[source] regex::Error),
    #[error("write the report")]
    Write(#[source] io::Error),
}

/// The error and every cause below it, joined as `outer: inner`.
pub fn chain(error: &dyn StdError) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(next) = cause {
        text.push_str(": ");
        text.push_str(&next.to_string());
        cause = next.source();
    }
    text
}
