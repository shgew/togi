//! togi's review tool. With no arguments, or `report`, it prints the track record over the current
//! repository's merged pull requests: robotogi's review check on each head, findings by priority and
//! outcome from the review records, and the time from opening to the first record.
//!
//! `snapshot` freezes a pull request's head, base and diff into a directory of hunk files and a manifest;
//! `render` writes the review record from that manifest and the coordinator's findings; `publish` posts the
//! record and the `review` check as robotogi. docs/review-record.md describes them.

pub mod cover;
pub mod diff;
pub mod error;
pub mod files;
pub mod manifest;
pub mod process;
pub mod publish;
pub mod rangediff;
pub mod record;
pub mod render;
pub mod report;
pub mod snapshot;
pub mod text;

#[cfg(test)]
#[path = "tests/fakes.rs"]
mod fakes;

use std::io::{Read, Write};

pub use error::Error;
pub use process::{CommandError, Programs, System};

pub const ROBOTOGI_LOGIN: &str = "robotogi";
/// The exact REST login of the robotogi App's comments; a prefix would also match ordinary users such as
/// robotogi-helper.
pub const ROBOTOGI_BOT_LOGIN: &str = "robotogi[bot]";

/// A length as the manifest's and record's integer counts hold it, saturating at `i64::MAX`, which no
/// list in memory reaches.
pub fn count(len: usize) -> i64 {
    i64::try_from(len).unwrap_or(i64::MAX)
}

/// What the subcommands reach outside the process through, so tests use fakes.
pub struct Tools<'a> {
    pub programs: &'a mut dyn Programs,
    /// `GH_TOKEN`, empty when unset.
    pub gh_token: String,
    /// The directory holding `togi-review/`.
    pub tmp: String,
    pub stdin: &'a mut dyn Read,
    pub stdout: &'a mut dyn Write,
    pub stderr: &'a mut dyn Write,
}

/// Runs the subcommand named first; no subcommand reports.
pub fn dispatch(t: &mut Tools<'_>, args: &[String]) -> Result<(), Error> {
    let Some(command) = args.first() else {
        return report::run(t.programs, t.stdout);
    };
    match command.as_str() {
        "report" => {
            if args.len() > 1 {
                return Err(Error::new("usage: reviews [report]"));
            }
            report::run(t.programs, t.stdout)
        }
        "snapshot" => snapshot::run_snapshot(t, &args[1..]),
        "render" => render::run_render(t.stdout, &args[1..]),
        "publish" => publish::run_publish(t, &args[1..]),
        _ => Err(Error::new(format!(
            "unknown subcommand {}: want report, snapshot, render or publish",
            text::quote(command)
        ))),
    }
}
