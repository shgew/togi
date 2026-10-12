//! The question `run` asks at a terminal about a defect finding, and how its answer is read.

use std::io::{self, Write};
use std::os::fd::BorrowedFd;
use std::rc::Rc;
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::{Errno, read};
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
        read_line(rustix::stdio::stdin(), cancel, readable)
    }

    fn discard(&mut self) {
        discard_typed_ahead();
    }
}

/// Whether `fd` has input to read within a tick.
fn readable(fd: BorrowedFd<'_>) -> rustix::io::Result<bool> {
    let mut fds = [PollFd::new(&fd, PollFlags::IN)];
    let tick = Timespec {
        tv_sec: 0,
        tv_nsec: i64::try_from(TICK.as_nanos()).unwrap_or(0),
    };
    Ok(poll(&mut fds, Some(&tick))? > 0)
}

/// Reads the next line from `fd`, up to and including its newline, or what came before the input ended;
/// `None` when the input ended with nothing typed or `cancel` ended first. `readable` waits up to a tick for
/// input, so `cancel` is checked at least once a tick, and each wake-up reads only what has arrived: a part
/// of a line never blocks a stop. A signal that interrupts the wait or the read only wakes it.
fn read_line(
    fd: BorrowedFd<'_>,
    cancel: &CancellationToken,
    mut readable: impl FnMut(BorrowedFd<'_>) -> rustix::io::Result<bool>,
) -> io::Result<Option<String>> {
    let mut line = Vec::new();
    let mut chunk = [0_u8; 256];
    loop {
        if cancel.is_cancelled() {
            return Ok(None);
        }
        match readable(fd) {
            Ok(true) => {}
            Ok(false) | Err(Errno::INTR) => continue,
            Err(err) => return Err(err.into()),
        }
        let arrived = match read(fd, &mut chunk) {
            Ok(len) => chunk.get(..len).unwrap_or_default(),
            Err(Errno::INTR | Errno::AGAIN) => continue,
            Err(err) => return Err(err.into()),
        };
        if arrived.is_empty() {
            return Ok((!line.is_empty()).then(|| String::from_utf8_lossy(&line).into_owned()));
        }
        if let Some(end) = arrived.iter().position(|&byte| byte == b'\n') {
            line.extend_from_slice(arrived.get(..=end).unwrap_or_default());
            return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
        }
        line.extend_from_slice(arrived);
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
/// counts; the end of the input or an answer that could not be read counts as no. Ending `cancel` before an
/// answer arrives is [`PromptError::Cancelled`].
///
/// # Errors
///
/// The question could not be written, or the run was cancelled.
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
    match answers.line(cancel) {
        Ok(Some(line)) => Ok(parse_answer(&line)),
        Ok(None) | Err(_) if cancel.is_cancelled() => Err(PromptError::Cancelled),
        Ok(None) | Err(_) => Ok(false),
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
