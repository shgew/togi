//! What a new release commit waits for: an empty target-machine queue (ADR 0043) and a passing `check` run on
//! the commit it builds on (ADR 0014).

use std::fmt;
use std::io::{self, Write};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde::de::IgnoredAny;

use crate::github::{ApiError, GitHub, Method, Repository, escape, query};
use crate::release::DEFAULT_BRANCH;

/// The workflow whose push run on `main` must pass before a release.
pub const CHECK_WORKFLOW: &str = "check.yml";
/// Several times the ten minutes a check run takes, leaving room for runs waiting on a runner.
pub const CHECK_TIMEOUT: Duration = Duration::from_hours(1);
/// How often the check run is looked up while it has not finished.
pub const CHECK_POLL: Duration = Duration::from_secs(30);
/// The label of an issue waiting on a run on the target machine, open or closed.
pub const HARDWARE_LABEL: &str = "needs-hardware";
/// The most records GitHub returns per page; a shorter page is the last one.
const ISSUES_PER_PAGE: usize = 100;

/// The time a wait measures and sleeps on.
pub trait Clock {
    fn now(&mut self) -> Instant;
    fn sleep(&mut self, duration: Duration);
}

/// The system's monotonic clock.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&mut self) -> Instant {
        Instant::now()
    }

    fn sleep(&mut self, duration: Duration) {
        thread::sleep(duration);
    }
}

/// The checks a new release commit must pass before it is made.
pub trait Gates {
    /// Fails while any issue, open or closed, waits on the target machine.
    fn require_no_hardware_wait(&mut self) -> Result<(), QueueError>;

    /// Waits for the check run on `commit` and fails unless it succeeds, reporting progress to `out`.
    fn require_green(&mut self, commit: &str, out: &mut dyn Write) -> Result<(), CheckError>;
}

/// The gates, read from GitHub.
pub struct GitHubGates<'a, C> {
    pub api: &'a mut GitHub,
    pub repository: &'a Repository,
    pub clock: C,
}

impl<C: Clock> Gates for GitHubGates<'_, C> {
    fn require_no_hardware_wait(&mut self) -> Result<(), QueueError> {
        require_no_hardware_wait(self.api, self.repository)
    }

    fn require_green(&mut self, commit: &str, out: &mut dyn Write) -> Result<(), CheckError> {
        wait_for_green_check(self.api, self.repository, commit, &mut self.clock, out)
    }
}

/// An issue waiting on a run on the target machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Waiting {
    pub number: u64,
    pub title: String,
    pub closed: bool,
}

impl fmt::Display for Waiting {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{} {}", self.number, self.title)?;
        if self.closed {
            f.write_str(" (closed)")?;
        }
        Ok(())
    }
}

/// Why the target-machine queue refuses a release.
#[derive(Debug, thiserror::Error)]
pub enum QueueError {
    #[error("list {HARDWARE_LABEL} issues")]
    List(#[source] ApiError),
    #[error("issues wait on a run on the target machine ({HARDWARE_LABEL}): {}", list(.0))]
    Waiting(Vec<Waiting>),
}

fn list(waiting: &[Waiting]) -> String {
    waiting
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

#[derive(Deserialize)]
struct Issue {
    number: u64,
    title: String,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    pull_request: Option<IgnoredAny>,
}

/// Fails while any issue carries [`HARDWARE_LABEL`], open or closed, naming each by number and title. The label,
/// not the open state, marks the wait: GitHub closes an issue when a pull request linked to it merges, before
/// the run on the target machine. The issues endpoint also lists pull requests, which are skipped.
pub fn require_no_hardware_wait(
    api: &mut GitHub,
    repository: &Repository,
) -> Result<(), QueueError> {
    let path = format!("{}/issues", repository.path());
    let per_page = ISSUES_PER_PAGE.to_string();
    let mut waiting = Vec::new();
    for page in 1.. {
        let page = page.to_string();
        let query = query(&[
            ("labels", HARDWARE_LABEL),
            ("state", "all"),
            ("per_page", &per_page),
            ("page", &page),
        ]);
        let issues: Vec<Issue> = api
            .decoded(Method::Get, &format!("{path}?{query}"), None)
            .map_err(QueueError::List)?;
        let last = issues.len() < ISSUES_PER_PAGE;
        waiting.extend(
            issues
                .into_iter()
                .filter(|issue| issue.pull_request.is_none())
                .map(|issue| Waiting {
                    number: issue.number,
                    title: issue.title,
                    closed: issue.state.as_deref() == Some("closed"),
                }),
        );
        if last {
            break;
        }
    }
    if waiting.is_empty() {
        Ok(())
    } else {
        Err(QueueError::Waiting(waiting))
    }
}

#[derive(Debug, Deserialize)]
struct CheckRun {
    html_url: String,
    status: String,
    #[serde(default)]
    conclusion: Option<String>,
}

#[derive(Deserialize)]
struct CheckRuns {
    workflow_runs: Vec<CheckRun>,
}

/// Where an unfinished check run stands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Pending {
    /// No push run of [`CHECK_WORKFLOW`] on `main` for the commit has started.
    Missing { commit: String },
    /// The run has started but not completed.
    Running {
        url: String,
        commit: String,
        status: String,
    },
}

impl fmt::Display for Pending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { commit } => {
                write!(
                    f,
                    "no {CHECK_WORKFLOW} run on {DEFAULT_BRANCH} for {commit}"
                )
            }
            Self::Running {
                url,
                commit,
                status,
            } => write!(f, "check run {url} for {commit} is {status}"),
        }
    }
}

/// Why the check run does not let the release proceed.
#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error("look up the {CHECK_WORKFLOW} run for {commit}")]
    Lookup {
        commit: String,
        #[source]
        source: ApiError,
    },
    #[error("check run {url} for {commit} concluded {conclusion}")]
    Concluded {
        url: String,
        commit: String,
        conclusion: String,
    },
    #[error("{pending} after waiting {}", go_duration(CHECK_TIMEOUT))]
    TimedOut { pending: Pending },
    #[error("report the wait")]
    Report(#[source] io::Error),
}

fn latest_check_run(
    api: &mut GitHub,
    repository: &Repository,
    commit: &str,
) -> Result<Option<CheckRun>, CheckError> {
    let path = format!(
        "{}/actions/workflows/{}/runs?{}",
        repository.path(),
        escape(CHECK_WORKFLOW),
        query(&[
            ("branch", DEFAULT_BRANCH),
            ("event", "push"),
            ("head_sha", commit),
            ("per_page", "1"),
        ])
    );
    let runs: CheckRuns =
        api.decoded(Method::Get, &path, None)
            .map_err(|source| CheckError::Lookup {
                commit: commit.to_owned(),
                source,
            })?;
    Ok(runs.workflow_runs.into_iter().next())
}

/// Looks up the check run for `commit` every [`CHECK_POLL`] until it completes or [`CHECK_TIMEOUT`] passes, and
/// fails unless it succeeded; each change of state is reported to `out`.
pub fn wait_for_green_check(
    api: &mut GitHub,
    repository: &Repository,
    commit: &str,
    clock: &mut impl Clock,
    out: &mut dyn Write,
) -> Result<(), CheckError> {
    let deadline = clock.now() + CHECK_TIMEOUT;
    let mut reported = None;
    loop {
        let pending = match latest_check_run(api, repository, commit)? {
            Some(run) if run.status == "completed" => {
                let conclusion = run.conclusion.unwrap_or_default();
                if conclusion == "success" {
                    return Ok(());
                }
                return Err(CheckError::Concluded {
                    url: run.html_url,
                    commit: commit.to_owned(),
                    conclusion,
                });
            }
            Some(run) => Pending::Running {
                url: run.html_url,
                commit: commit.to_owned(),
                status: run.status,
            },
            None => Pending::Missing {
                commit: commit.to_owned(),
            },
        };
        let left = deadline.saturating_duration_since(clock.now());
        if left.is_zero() {
            return Err(CheckError::TimedOut { pending });
        }
        if reported.as_ref() != Some(&pending) {
            writeln!(
                out,
                "{pending}; waiting up to {} for it to pass",
                go_duration(round_to_second(left))
            )
            .map_err(CheckError::Report)?;
            reported = Some(pending);
        }
        clock.sleep(CHECK_POLL.min(left));
    }
}

fn round_to_second(duration: Duration) -> Duration {
    let seconds = duration.as_secs();
    if duration.subsec_nanos() >= 500_000_000 {
        Duration::from_secs(seconds + 1)
    } else {
        Duration::from_secs(seconds)
    }
}

/// A whole number of seconds as Go prints a duration: `1h0m0s`, `59m30s`, `4s`.
fn go_duration(duration: Duration) -> String {
    let total = duration.as_secs();
    let (hours, minutes, seconds) = (total / 3600, total / 60 % 60, total % 60);
    if hours > 0 {
        format!("{hours}h{minutes}m{seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m{seconds}s")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
#[path = "tests/gates.rs"]
mod tests;
