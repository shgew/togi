use std::fmt;
use std::io;
use std::path::PathBuf;

use serde::Serialize;

use crate::Group;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Every eligible group's observed failures are inside its interval.
    Ok,
    /// At least one group's observed failures are outside its interval.
    Flagged,
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Ok => "ok",
            Self::Flagged => "flagged",
        })
    }
}

/// One machine judged against one extract: every eligible group with its prediction, and the idle failures
/// the extract holds, which have no trial exposure to check.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Check {
    /// The machine file as the caller named it; a caller that prints it may rewrite it first.
    pub machine: PathBuf,
    pub extract: PathBuf,
    pub status: Status,
    pub groups: Vec<Group>,
    pub idle_failures: usize,
}

impl Check {
    pub fn flagged(&self) -> impl Iterator<Item = &Group> {
        self.groups.iter().filter(|group| group.flagged)
    }
}

/// Writes the model-check section of a report: one line per check and one per flagged group. Nothing is
/// written for no checks.
pub fn report(out: &mut impl io::Write, checks: &[Check]) -> io::Result<()> {
    if checks.is_empty() {
        return Ok(());
    }
    writeln!(out, "\nModel check (99% binomial intervals)")?;
    for check in checks {
        writeln!(
            out,
            "{}: {} ({} eligible groups; {} idle failures without trial exposure)",
            check.machine.display(),
            check.status,
            check.groups.len(),
            check.idle_failures,
        )?;
        for group in check.flagged() {
            writeln!(
                out,
                "  {group} interval=[{},{}] mean_p={:.6e}",
                group.interval.low, group.interval.high, group.mean_p,
            )?;
        }
    }
    Ok(())
}
