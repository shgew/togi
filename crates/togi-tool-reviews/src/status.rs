//! The next action for open pull requests, using the checks on each exact head.

use std::fmt;
use std::io::Write;

use serde::Deserialize;

use crate::parts::{join_records, robotogi_comments};
use crate::record::{RecordError, parse_record};
use crate::{Error, Outcome, Programs, ROBOTOGI_APP_ID};

const PULL_FIELDS: &str = r#"
  number title state isDraft headRefOid baseRefName mergeable
  baseRef { target { oid } }
  commits(last: 1) { nodes { commit {
    checks: checkSuites(first: 20) {
      pageInfo { hasNextPage }
      nodes {
        runs: checkRuns(first: 20, filterBy: {checkName: "check", checkType: LATEST}) {
          pageInfo { hasNextPage }
          nodes { name status conclusion }
        }
      }
    }
    reviews: checkSuites(first: 20, filterBy: {appId: $appId}) {
      pageInfo { hasNextPage }
      nodes {
        runs: checkRuns(first: 20, filterBy: {checkName: "review", checkType: LATEST}) {
          pageInfo { hasNextPage }
          nodes { name status conclusion }
        }
      }
    }
  } } }
"#;

const THREADS_QUERY: &str = r"query($owner: String!, $name: String!, $number: Int!, $endCursor: String) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      reviewThreads(first: 100, after: $endCursor) {
        pageInfo { hasNextPage endCursor }
        nodes { isResolved }
      }
    }
  }
}";

/// Where review of a head stands; robotogi's passing review check on the head means done.
#[derive(Debug, PartialEq, Eq)]
enum Review {
    Done,
    Pending(Pending),
}

/// Why review of a head is not done yet.
#[derive(Debug, PartialEq, Eq)]
enum Pending {
    NeedsReview,
    RecordWithoutCheck,
    CarryForward { url: String },
}

/// The first matching merge gate. Draft, conflicts and a failed check come before review in that order,
/// so they carry the review state that `/review-pr` still needs.
#[derive(Debug, PartialEq, Eq)]
enum State<'a> {
    Draft { review: Review },
    Conflicts { base: &'a str, review: Review },
    CheckFailed { head: &'a str, review: Review },
    Review(Pending),
    UnresolvedThreads { count: usize },
    Behind { base: &'a str },
    CheckRunning,
    Ready,
}

impl State<'_> {
    fn outcome(&self) -> Outcome {
        match self {
            Self::Ready => Outcome::Success,
            Self::CheckRunning => Outcome::Waiting,
            Self::Draft { .. }
            | Self::Conflicts { .. }
            | Self::CheckFailed { .. }
            | Self::Review(_)
            | Self::UnresolvedThreads { .. }
            | Self::Behind { .. } => Outcome::NotReady,
        }
    }

    /// The review state an earlier gate hides from `next:`.
    fn masked_review(&self) -> Option<&Review> {
        match self {
            Self::Draft { review }
            | Self::Conflicts { review, .. }
            | Self::CheckFailed { review, .. } => Some(review),
            Self::Review(_)
            | Self::UnresolvedThreads { .. }
            | Self::Behind { .. }
            | Self::CheckRunning
            | Self::Ready => None,
        }
    }
}

impl fmt::Display for Pending {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NeedsReview => f.write_str("needs review"),
            Self::RecordWithoutCheck => {
                f.write_str("needs review: the record on this head has no passing review check")
            }
            Self::CarryForward { url } => {
                write!(f, "carry forward or review the delta since {url}")
            }
        }
    }
}

impl fmt::Display for Review {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Done => f.write_str("done"),
            Self::Pending(pending) => pending.fmt(f),
        }
    }
}

impl fmt::Display for State<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Draft { .. } => f.write_str("draft"),
            Self::Conflicts { base, .. } => write!(f, "conflicts with {base}"),
            Self::CheckFailed { head, .. } => write!(f, "check failed on {head}"),
            Self::Review(pending) => pending.fmt(f),
            Self::UnresolvedThreads { count } => write!(f, "{count} unresolved review threads"),
            Self::Behind { base } => {
                write!(
                    f,
                    "behind {base}: just push-fix, then carry the review forward"
                )
            }
            Self::CheckRunning => f.write_str("check running"),
            Self::Ready => f.write_str("ready to merge"),
        }
    }
}

#[derive(Deserialize)]
struct Page {
    data: Data,
}

#[derive(Deserialize)]
struct Data {
    repository: Repository,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repository {
    #[serde(default)]
    pull_requests: Option<Connection<Pull>>,
    #[serde(default)]
    pull_request: Option<Pull>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connection<T> {
    nodes: Vec<T>,
    #[serde(default)]
    page_info: PageInfo,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    #[serde(default)]
    has_next_page: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pull {
    number: i64,
    title: String,
    state: String,
    is_draft: bool,
    head_ref_oid: String,
    base_ref_name: String,
    base_ref: Option<BaseRef>,
    mergeable: String,
    commits: Connection<CommitNode>,
}

#[derive(Deserialize)]
struct BaseRef {
    target: Target,
}

#[derive(Deserialize)]
struct Target {
    oid: String,
}

#[derive(Deserialize)]
struct CommitNode {
    commit: Commit,
}

#[derive(Deserialize)]
struct Commit {
    checks: Connection<Suite>,
    reviews: Connection<Suite>,
}

#[derive(Deserialize)]
struct Suite {
    runs: Connection<Run>,
}

#[derive(Deserialize)]
struct App {
    #[serde(alias = "databaseId")]
    id: i64,
}

#[derive(Deserialize)]
struct Run {
    name: String,
    status: String,
    conclusion: Option<String>,
}

impl Run {
    fn passed(&self) -> bool {
        self.status.eq_ignore_ascii_case("completed")
            && self
                .conclusion
                .as_deref()
                .is_some_and(|c| c.eq_ignore_ascii_case("success"))
    }

    fn failed(&self) -> bool {
        self.status.eq_ignore_ascii_case("completed") && !self.passed()
    }
}

/// Prints every open pull request as a section, or one pull request and its merge-gate outcome.
pub fn run(
    programs: &mut dyn Programs,
    w: &mut dyn Write,
    args: &[String],
) -> Result<Outcome, Error> {
    let number = match args {
        [] => None,
        [number] => Some(
            number
                .parse::<i64>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| Error::new("usage: reviews status [N]"))?,
        ),
        _ => return Err(Error::new("usage: reviews status [N]")),
    };
    let mut pulls = fetch_pulls(programs, number)?;
    pulls.sort_by_key(|p| p.number);
    let mut rows = Vec::with_capacity(pulls.len());
    for pull in &pulls {
        if pull.state != "OPEN" {
            return Err(Error::new(format!(
                "classify open pull request #{}",
                pull.number
            )));
        }
        let state = classify(programs, pull)?;
        rows.push((pull, state));
    }
    if number.is_none() {
        writeln!(w, "Pull requests").map_err(|error| Error::wrap("write status", error))?;
        if rows.is_empty() {
            writeln!(w, "  none").map_err(|error| Error::wrap("write status", error))?;
        }
    }
    for (pull, state) in &rows {
        render(w, pull, state)?;
    }
    Ok(if number.is_some() {
        rows.first()
            .map_or(Outcome::NotReady, |(_, state)| state.outcome())
    } else {
        Outcome::Success
    })
}

fn fetch_pulls(programs: &mut dyn Programs, number: Option<i64>) -> Result<Vec<Pull>, Error> {
    let selection = number.map_or_else(
        || format!("pullRequests(states: OPEN, first: 50, after: $endCursor) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ {PULL_FIELDS} }} }}"),
        |_| format!("pullRequest(number: $number) {{ {PULL_FIELDS} }}"),
    );
    let number_variable = if number.is_some() {
        ", $number: Int!"
    } else {
        ""
    };
    // A single-pull query has no outer connection; declaring an unused cursor is invalid GraphQL.
    let cursor_variable = if number.is_none() {
        ", $endCursor: String"
    } else {
        ""
    };
    let query = format!(
        "query=query($owner: String!, $name: String!, $appId: Int!{number_variable}{cursor_variable}) {{ repository(owner: $owner, name: $name) {{ {selection} }} }}"
    );
    let app = format!("appId={ROBOTOGI_APP_ID}");
    let number_field = number.map(|n| format!("number={n}"));
    let mut args = vec![
        "api",
        "graphql",
        "--paginate",
        "--slurp",
        "-F",
        "owner={owner}",
        "-F",
        "name={repo}",
        "-F",
        &app,
        "-f",
        &query,
    ];
    if let Some(field) = &number_field {
        args.extend(["-F", field]);
    }
    let out = programs
        .gh(&args)
        .map_err(|error| Error::wrap("fetch open pull requests", error))?;
    let pages: Vec<Page> = serde_json::from_slice(&out)
        .map_err(|error| Error::wrap("decode open pull requests", error))?;
    let mut pulls = Vec::new();
    for page in pages {
        let repository = page.data.repository;
        if number.is_some() {
            pulls.push(
                repository
                    .pull_request
                    .ok_or_else(|| Error::new("find open pull request"))?,
            );
        } else {
            pulls.extend(
                repository
                    .pull_requests
                    .ok_or_else(|| Error::new("read open pull requests"))?
                    .nodes,
            );
        }
    }
    if number.is_some() && pulls.len() != 1 {
        return Err(Error::new("find open pull request"));
    }
    Ok(pulls)
}

fn classify<'a>(programs: &mut dyn Programs, pull: &'a Pull) -> Result<State<'a>, Error> {
    let commit = &pull
        .commits
        .nodes
        .first()
        .ok_or_else(|| Error::new("read head commit checks"))?
        .commit;
    let truncated = [&commit.checks, &commit.reviews].iter().any(|suites| {
        suites.page_info.has_next_page
            || suites.nodes.iter().any(|s| s.runs.page_info.has_next_page)
    });
    // Nested connections cannot share gh's outer pull-request cursor. REST completes unusually large
    // check histories instead of silently classifying only their first page.
    let complete = if truncated {
        Some(fetch_all_checks(programs, &pull.head_ref_oid)?)
    } else {
        None
    };
    let mut checks = commit
        .checks
        .nodes
        .iter()
        .flat_map(|s| &s.runs.nodes)
        .peekable();
    let check_failed = complete.as_ref().map_or_else(
        || checks.clone().any(Run::failed),
        |runs| runs.iter().any(|r| r.run.name == "check" && r.run.failed()),
    );
    // The GraphQL `reviews` connection is filtered to robotogi's App by the server, which does not expose
    // that App on its suites to every token; REST does, so the fallback checks it.
    let reviewed = complete.as_ref().map_or_else(
        || {
            commit
                .reviews
                .nodes
                .iter()
                .flat_map(|s| &s.runs.nodes)
                .any(Run::passed)
        },
        |runs| {
            runs.iter().any(|r| {
                r.run.name == "review"
                    && r.app.as_ref().is_some_and(|a| a.id == ROBOTOGI_APP_ID)
                    && r.run.passed()
            })
        },
    );
    let review = if reviewed {
        Review::Done
    } else {
        Review::Pending(pending_review(programs, pull)?)
    };
    if pull.is_draft {
        return Ok(State::Draft { review });
    }
    if pull.mergeable == "CONFLICTING" {
        return Ok(State::Conflicts {
            base: &pull.base_ref_name,
            review,
        });
    }
    if check_failed {
        return Ok(State::CheckFailed {
            head: pull.head_ref_oid.get(..7).unwrap_or(&pull.head_ref_oid),
            review,
        });
    }
    if let Review::Pending(pending) = review {
        return Ok(State::Review(pending));
    }
    let count = unresolved_threads(programs, pull.number)?;
    if count > 0 {
        return Ok(State::UnresolvedThreads { count });
    }
    let base = pull
        .base_ref
        .as_ref()
        .ok_or_else(|| Error::new("read base branch tip"))?;
    let out = programs
        .gh(&[
            "api",
            &format!(
                "repos/{{owner}}/{{repo}}/compare/{}...{}",
                base.target.oid, pull.head_ref_oid
            ),
        ])
        .map_err(|error| Error::wrap("compare base branch with head", error))?;
    let comparison: Comparison = serde_json::from_slice(&out)
        .map_err(|error| Error::wrap("decode base branch comparison", error))?;
    if comparison.behind_by > 0 {
        return Ok(State::Behind {
            base: &pull.base_ref_name,
        });
    }
    let check_running = complete.as_ref().map_or_else(
        || checks.peek().is_none() || checks.any(|run| !run.passed()),
        |runs| {
            let mut checks = runs.iter().filter(|r| r.run.name == "check").peekable();
            checks.peek().is_none() || checks.any(|run| !run.run.passed())
        },
    );
    if check_running {
        return Ok(State::CheckRunning);
    }
    Ok(State::Ready)
}

/// Why review is not done: no complete record, a record on this head without a passing check, or the
/// latest record on an older head.
fn pending_review(programs: &mut dyn Programs, pull: &Pull) -> Result<Pending, Error> {
    let comments = join_records(robotogi_comments(programs, "{owner}/{repo}", pull.number)?)?;
    let mut latest = None;
    let mut on_head = false;
    for comment in comments {
        let record = match parse_record(&comment.body) {
            Ok(record) => record,
            Err(RecordError::NoBlock) => continue,
            Err(error) => return Err(Error::wrap("parse review record", error)),
        };
        on_head |= record.head_sha == pull.head_ref_oid;
        if latest
            .as_ref()
            .is_none_or(|previous: &crate::parts::Comment| previous.created <= comment.created)
        {
            latest = Some(comment);
        }
    }
    Ok(if on_head {
        Pending::RecordWithoutCheck
    } else if let Some(record) = latest {
        Pending::CarryForward { url: record.url }
    } else {
        Pending::NeedsReview
    })
}

#[derive(Deserialize)]
struct Comparison {
    behind_by: u64,
}

#[derive(Deserialize)]
struct RestRun {
    #[serde(flatten)]
    run: Run,
    app: Option<App>,
}

#[derive(Deserialize)]
struct CheckPage {
    check_runs: Vec<RestRun>,
}

fn fetch_all_checks(programs: &mut dyn Programs, head: &str) -> Result<Vec<RestRun>, Error> {
    let out = programs
        .gh(&[
            "api",
            "--paginate",
            "--slurp",
            &format!(
                "repos/{{owner}}/{{repo}}/commits/{head}/check-runs?per_page=100&filter=latest"
            ),
        ])
        .map_err(|error| Error::wrap("fetch head check runs", error))?;
    let pages: Vec<CheckPage> = serde_json::from_slice(&out)
        .map_err(|error| Error::wrap("decode head check runs", error))?;
    Ok(pages.into_iter().flat_map(|p| p.check_runs).collect())
}

#[derive(Deserialize)]
struct ThreadsPage {
    data: ThreadsData,
}

#[derive(Deserialize)]
struct ThreadsData {
    repository: ThreadsRepository,
}

#[derive(Deserialize)]
struct ThreadsRepository {
    #[serde(rename = "pullRequest")]
    pull_request: ThreadsPull,
}

#[derive(Deserialize)]
struct ThreadsPull {
    #[serde(rename = "reviewThreads")]
    review_threads: Connection<Thread>,
}

#[derive(Deserialize)]
struct Thread {
    #[serde(rename = "isResolved")]
    is_resolved: bool,
}

fn unresolved_threads(programs: &mut dyn Programs, number: i64) -> Result<usize, Error> {
    let query = format!("query={THREADS_QUERY}");
    let number = format!("number={number}");
    let out = programs
        .gh(&[
            "api",
            "graphql",
            "--paginate",
            "--slurp",
            "-F",
            "owner={owner}",
            "-F",
            "name={repo}",
            "-F",
            &number,
            "-f",
            &query,
        ])
        .map_err(|error| Error::wrap("fetch review threads", error))?;
    let pages: Vec<ThreadsPage> = serde_json::from_slice(&out)
        .map_err(|error| Error::wrap("decode review threads", error))?;
    Ok(pages
        .into_iter()
        .flat_map(|p| p.data.repository.pull_request.review_threads.nodes)
        .filter(|t| !t.is_resolved)
        .count())
}

fn render(w: &mut dyn Write, pull: &Pull, state: &State<'_>) -> Result<(), Error> {
    writeln!(w, "  #{} {}", pull.number, pull.title)
        .map_err(|error| Error::wrap("write status", error))?;
    writeln!(w, "    next: {state}").map_err(|error| Error::wrap("write status", error))?;
    if let Some(review) = state.masked_review() {
        writeln!(w, "    review: {review}").map_err(|error| Error::wrap("write status", error))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests/status.rs"]
mod tests;
