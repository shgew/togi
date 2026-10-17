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

#[expect(
    clippy::expect_used,
    reason = "the patterns are constants, each compiled by the tests that exercise it"
)]
fn pattern(source: &'static str) -> Regex {
    Regex::new(source).expect("a constant pattern compiles")
}
