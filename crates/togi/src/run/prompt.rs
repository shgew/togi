//! The question `run` asks at a terminal about a defect finding, and how its answer is read.

use std::io::{self, BufRead, Write};
use std::rc::Rc;
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use togi_defect::FindingView;
use togi_machine::CancellationToken;
use togi_session::{Prompt, PromptError};

use crate::dashboard::{Dashboard, discard_typed_ahead};

const TICK: Duration = Duration::from_millis(100);

/// Where answers come from.
pub(super) trait Answers {
    /// The next line typed, up to and including its newline, or `None` when the input ended or `cancel` did
    /// first.
    fn line(&mut self, cancel: &CancellationToken) -> io::Result<Option<String>>;

    /// Drops what was typed before now.
    fn discard(&mut self);
}

/// Standard input.
pub(super) struct Stdin;

impl Answers for Stdin {
    fn line(&mut self, cancel: &CancellationToken) -> io::Result<Option<String>> {
        let stdin = rustix::stdio::stdin();
        loop {
            if cancel.is_cancelled() {
                return Ok(None);
            }
            let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
            let tick = Timespec {
                tv_sec: 0,
                tv_nsec: i64::try_from(TICK.as_nanos()).unwrap_or(0),
            };
            if poll(&mut fds, Some(&tick))? == 0 {
                continue;
            }
            let mut line = String::new();
            io::stdin().lock().read_line(&mut line)?;
            return Ok(Some(line));
        }
    }

    fn discard(&mut self) {
        discard_typed_ahead();
    }
}

/// Whether `line` is a complete line that says yes: `y` or `yes`, in any case, after trimming.
#[must_use]
pub(super) fn parse_answer(line: &str) -> bool {
    line.ends_with('\n') && {
        let answer = line.trim();
        answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
    }
}

/// Writes the finding and asks whether to reset the cores it affected. Only input typed after the question
/// counts. Ending `cancel` before an answer arrives is [`PromptError::Cancelled`].
///
/// # Errors
///
/// The question or the answer could not be written or read, or the run was cancelled.
pub(super) fn ask(
    out: &mut dyn Write,
    answers: &mut dyn Answers,
    cancel: &CancellationToken,
    finding: &FindingView<'_>,
) -> Result<bool, PromptError> {
    answers.discard();
    let cores = finding
        .cores
        .iter()
        .map(|core| core.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let decisions = finding
        .decisions
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(
        out,
        "togi: defect {}: {} (fixed by pull request #{}); {} decisions [{decisions}] affected cores [{cores}]",
        finding.id.0, finding.title, finding.pr, finding.direction
    )?;
    if !finding.detail.is_empty() {
        writeln!(out, "{}", finding.detail)?;
    }
    write!(out, "Reset cores {cores}? [y/N] ")?;
    out.flush()?;
    match answers.line(cancel)? {
        Some(line) => Ok(parse_answer(&line)),
        None if cancel.is_cancelled() => Err(PromptError::Cancelled),
        None => Ok(false),
    }
}

/// Asks at the terminal, with the dashboard cleared while the question is open.
pub(super) struct Terminal {
    cancel: CancellationToken,
    dashboard: Option<Rc<Dashboard>>,
}

impl Terminal {
    #[must_use]
    pub(super) const fn new(cancel: CancellationToken, dashboard: Option<Rc<Dashboard>>) -> Self {
        Self { cancel, dashboard }
    }
}

impl Prompt for Terminal {
    fn reset_cores(&mut self, finding: &FindingView<'_>) -> Result<bool, PromptError> {
        if let Some(dashboard) = &self.dashboard {
            dashboard.hide();
        }
        let answer = ask(&mut io::stderr(), &mut Stdin, &self.cancel, finding);
        if let Some(dashboard) = &self.dashboard {
            dashboard.show();
        }
        answer
    }
}

#[cfg(test)]
mod tests;
