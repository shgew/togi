//! togi's review tool. With no arguments, or `report`, it prints the track record over the current
//! repository's merged pull requests: robotogi's review check on each head, findings by priority and
//! outcome from the review records, and the time from opening to the first record.
//! `status [N]` prints the next action for every open pull request, or the requested pull request.
//!
//! `snapshot` freezes a pull request's head, base and diff into a directory of hunk files and a manifest;
//! `render` writes the review record from that manifest and the coordinator's findings; `publish` posts the
//! record and the `review` check as robotogi. docs/review-record.md describes them.

pub mod cover;
pub mod diff;
pub mod error;
pub mod files;
pub mod headlines;
pub mod manifest;
pub mod parts;
pub mod process;
pub mod publish;
pub mod rangediff;
pub mod record;
pub mod render;
pub mod report;
pub mod snapshot;
pub mod status;
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
/// The GitHub App whose successful review check completes robotogi's review gate.
pub const ROBOTOGI_APP_ID: i64 = 5_162_510;

/// A successful command, a pending CI check, or a pull request that needs another action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Waiting,
    NotReady,
}

impl Outcome {
    /// The process exit status: 0 for success, 3 while only a check runs, 4 for any other next action.
    /// An error exits 1 instead.
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Success => 0,
            Self::Waiting => 3,
            Self::NotReady => 4,
        }
    }
}

/// A length as the manifest's and record's integer counts hold it, saturating at `i64::MAX`, which no
/// list in memory reaches.
pub fn saturating_record_count(len: usize) -> i64 {
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
pub fn dispatch(t: &mut Tools<'_>, args: &[String]) -> Result<Outcome, Error> {
    let Some(command) = args.first() else {
        return report::run(t.programs, t.stdout).map(|()| Outcome::Success);
    };
    match command.as_str() {
        "status" => status::run(t.programs, t.stdout, &args[1..]),
        "report" => {
            if args.len() > 1 {
                return Err(Error::new("usage: reviews [report]"));
            }
            report::run(t.programs, t.stdout).map(|()| Outcome::Success)
        }
        "snapshot" => snapshot::run_snapshot(t, &args[1..]).map(|()| Outcome::Success),
        "render" => render::run_render(t.stdout, &args[1..]).map(|()| Outcome::Success),
        "publish" => publish::run_publish(t, &args[1..]).map(|()| Outcome::Success),
        _ => Err(Error::new(format!(
            "unknown subcommand {}: want report, status, snapshot, render or publish",
            text::quote(command)
        ))),
    }
}

#[cfg(test)]
mod numeric_tests {
    use super::saturating_record_count;
    use test_case::test_case;

    #[cfg(target_pointer_width = "64")]
    #[test_case(0, 0; "zero")]
    #[test_case(usize::try_from(i64::MAX).unwrap(), i64::MAX; "largest exact count")]
    #[test_case(usize::try_from(i64::MAX).unwrap() + 1, i64::MAX; "first saturated count")]
    #[test_case(usize::MAX, i64::MAX; "largest platform count")]
    fn record_counts_saturate(len: usize, expected: i64) {
        assert_eq!(saturating_record_count(len), expected);
    }
}
