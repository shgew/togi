//! togi's release tool: validates the changelog fragments in `changes/` (`-check DIR`, the `changes` flake
//! check), previews the release `origin/main` would make (no flags, `just release-preview`), and, from the
//! release workflow, commits the release (`-commit`) and publishes it (`-publish`). See `docs/releasing.md`.

pub mod changelog;
pub mod cli;
pub mod fragments;
pub mod gates;
pub mod git;
pub mod github;
pub mod release;

#[cfg(test)]
#[path = "tests/fakes.rs"]
mod fakes;

use std::error::Error;
use std::sync::LazyLock;

use regex::Regex;

/// The error and every cause below it, joined as `outer: inner`.
pub fn chain(error: &dyn Error) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(next) = cause {
        text.push_str(": ");
        text.push_str(&next.to_string());
        cause = next.source();
    }
    text
}

/// A constant pattern that does not compile.
#[derive(Clone, Debug, thiserror::Error)]
#[error("compile the pattern {pattern}")]
pub struct PatternError {
    pattern: &'static str,
    #[source]
    source: regex::Error,
}

/// A pattern compiled on first use.
type Pattern = LazyLock<Result<Regex, PatternError>>;

fn pattern(source: &'static str) -> Result<Regex, PatternError> {
    Regex::new(source).map_err(|error| PatternError {
        pattern: source,
        source: error,
    })
}

fn compiled(pattern: &'static Pattern) -> Result<&'static Regex, PatternError> {
    pattern.as_ref().map_err(Clone::clone)
}
