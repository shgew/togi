//! The track record of merged pull requests: robotogi's review check on each head, findings by priority and
//! outcome from the review records, and the time from opening to the first record.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write;

use jiff::Timestamp;
use serde::Deserialize;

use crate::record::{Finding, OUTCOMES, PRIORITIES, RecordError, parse_record};
use crate::text::nullable;
use crate::{Error, Programs, ROBOTOGI_LOGIN};

/// The time an absent `createdAt` reads as: midnight UTC on 1 January of year 1.
const ZERO_TIME: Timestamp = Timestamp::constant(-62_135_596_800, 0);

const NANOS_PER_MINUTE: i64 = 60_000_000_000;
const NANOS_PER_HOUR: i64 = 60 * NANOS_PER_MINUTE;

/// The merged pull requests, with check suites filtered by robotogi's App ID.
pub const PULLS_QUERY: &str = r#"query($owner: String!, $name: String!, $endCursor: String) {
  repository(owner: $owner, name: $name) {
    pullRequests(states: MERGED, first: 50, after: $endCursor) {
      pageInfo { hasNextPage endCursor }
      nodes {
        number title createdAt
        commits(last: 1) { nodes { commit {
          checkSuites(first: 10, filterBy: {appId: 5162510}) { nodes {
            checkRuns(first: 10, filterBy: {checkName: "review"}) { nodes { conclusion } }
          } }
        } } }
        comments(first: 100) {
          pageInfo { hasNextPage }
          nodes { author { login } createdAt url body }
        }
      }
    }
  }
}"#;

/// A merged pull request as fetched: its head's review check and robotogi's comments.
#[derive(Clone, Debug)]
pub struct Pull {
    pub number: i64,
    pub title: String,
    pub opened: Timestamp,
    /// The conclusion of robotogi's review check on the head commit, empty when the head has none.
    pub check: String,
    pub comments: Vec<Comment>,
}

#[derive(Clone, Debug)]
pub struct Comment {
    pub url: String,
    pub created: Timestamp,
    pub body: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub priority: [i64; PRIORITIES.len()],
    pub outcome: [i64; OUTCOMES.len()],
}

impl Counts {
    fn add(&mut self, other: &Self) {
        for (total, n) in self.priority.iter_mut().zip(other.priority) {
            *total += n;
        }
        for (total, n) in self.outcome.iter_mut().zip(other.outcome) {
            *total += n;
        }
    }
}

/// Where a finding is: the package or directory its file is in, or unknown when its location names no
/// file.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Place {
    /// The file's first two directories, such as `crates/togi-tuner`, `docs/spec` or `docs`; `.` for a
    /// file at the repository root.
    Directory(String),
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    pub number: i64,
    pub title: String,
    pub check: String,
    pub records: i64,
    /// Nanoseconds from opening to the first review record; meaningful only when `records > 0`.
    pub first_record: i64,
    pub findings: Counts,
    /// The findings counted in `findings`, by place.
    pub places: BTreeMap<Place, i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Totals {
    pub pulls: i64,
    pub checked: i64,
    pub successful: i64,
    pub recorded: i64,
    pub findings: Counts,
    /// Each recorded pull request's time to its first record, ascending.
    pub first_records: Vec<i64>,
    pub places: BTreeMap<Place, i64>,
}

pub fn run(programs: &mut dyn Programs, w: &mut dyn Write) -> Result<(), Error> {
    let pulls = fetch_pulls(programs)?;
    let rows = summarize(&pulls)?;
    let totals = total(&rows);
    render(w, &rows, &totals)
}

#[derive(Default, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
struct Nodes<T> {
    #[serde(default, deserialize_with = "nullable")]
    nodes: Vec<T>,
}

#[derive(Default, Deserialize)]
struct Page {
    #[serde(default, deserialize_with = "nullable")]
    data: PageData,
}

#[derive(Default, Deserialize)]
struct PageData {
    #[serde(default, deserialize_with = "nullable")]
    repository: Repository,
}

#[derive(Default, Deserialize)]
struct Repository {
    #[serde(default, deserialize_with = "nullable", rename = "pullRequests")]
    pull_requests: Nodes<PullNode>,
}

#[derive(Default, Deserialize)]
struct PullNode {
    #[serde(default, deserialize_with = "nullable")]
    number: i64,
    #[serde(default, deserialize_with = "nullable")]
    title: String,
    #[serde(default, rename = "createdAt")]
    created_at: Option<Timestamp>,
    #[serde(default, deserialize_with = "nullable")]
    commits: Nodes<CommitNode>,
    #[serde(default, deserialize_with = "nullable")]
    comments: Comments,
}

#[derive(Default, Deserialize)]
struct CommitNode {
    #[serde(default, deserialize_with = "nullable")]
    commit: Commit,
}

#[derive(Default, Deserialize)]
struct Commit {
    #[serde(default, deserialize_with = "nullable", rename = "checkSuites")]
    check_suites: Nodes<CheckSuite>,
}

#[derive(Default, Deserialize)]
struct CheckSuite {
    #[serde(default, deserialize_with = "nullable", rename = "checkRuns")]
    check_runs: Nodes<CheckRun>,
}

#[derive(Default, Deserialize)]
struct CheckRun {
    #[serde(default, deserialize_with = "nullable")]
    conclusion: String,
}

#[derive(Default, Deserialize)]
struct Comments {
    #[serde(default, deserialize_with = "nullable", rename = "pageInfo")]
    page_info: PageInfo,
    #[serde(default, deserialize_with = "nullable")]
    nodes: Vec<CommentNode>,
}

#[derive(Default, Deserialize)]
struct PageInfo {
    #[serde(default, deserialize_with = "nullable", rename = "hasNextPage")]
    has_next_page: bool,
}

#[derive(Deserialize)]
struct CommentNode {
    #[serde(default)]
    author: Option<Author>,
    #[serde(default, rename = "createdAt")]
    created_at: Option<Timestamp>,
    #[serde(default, deserialize_with = "nullable")]
    url: String,
    #[serde(default, deserialize_with = "nullable")]
    body: String,
}

#[derive(Default, Deserialize)]
struct Author {
    #[serde(default, deserialize_with = "nullable")]
    login: String,
}

fn fetch_pulls(programs: &mut dyn Programs) -> Result<Vec<Pull>, Error> {
    let query = format!("query={PULLS_QUERY}");
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
            "-f",
            &query,
        ])
        .map_err(|error| Error::wrap("fetch merged pull requests", error))?;
    let pages: Vec<Page> = serde_json::from_slice(&out)
        .map_err(|error| Error::wrap("decode merged pull requests", error))?;
    let mut pulls = Vec::new();
    for page in pages {
        for node in page.data.repository.pull_requests.nodes {
            if node.comments.page_info.has_next_page {
                return Err(Error::new(format!(
                    "pull request #{} has more than 100 comments",
                    node.number
                )));
            }
            let check = head_check(&node);
            pulls.push(Pull {
                number: node.number,
                title: node.title,
                opened: node.created_at.unwrap_or(ZERO_TIME),
                check,
                comments: node
                    .comments
                    .nodes
                    .into_iter()
                    .filter(|c| c.author.as_ref().is_some_and(|a| a.login == ROBOTOGI_LOGIN))
                    .map(|c| Comment {
                        url: c.url,
                        created: c.created_at.unwrap_or(ZERO_TIME),
                        body: c.body,
                    })
                    .collect(),
            });
        }
    }
    Ok(pulls)
}

/// `success` when any robotogi review check on the head succeeded, else the first one's conclusion,
/// lower-cased.
fn head_check(node: &PullNode) -> String {
    let mut first = String::new();
    let runs = node.commits.nodes.iter().flat_map(|c| {
        c.commit
            .check_suites
            .nodes
            .iter()
            .flat_map(|s| s.check_runs.nodes.iter())
    });
    for run in runs {
        let mut conclusion = run.conclusion.to_lowercase();
        if conclusion.is_empty() {
            "pending".clone_into(&mut conclusion);
        }
        if conclusion == "success" {
            return conclusion;
        }
        if first.is_empty() {
            first = conclusion;
        }
    }
    first
}

/// One row per pull request, ascending by number. Each finding entry of a record counts once, except that
/// an entry of a later record updates the outcome of one not-yet-updated entry of earlier records with the
/// same source, priority, text and location.
pub fn summarize(pulls: &[Pull]) -> Result<Vec<Row>, Error> {
    let mut rows = Vec::with_capacity(pulls.len());
    for pull in pulls {
        let mut row = Row {
            number: pull.number,
            title: pull.title.clone(),
            check: pull.check.clone(),
            ..Row::default()
        };
        let mut comments: Vec<&Comment> = pull.comments.iter().collect();
        comments.sort_by_key(|c| c.created);
        let mut found: Vec<Finding> = Vec::new();
        for comment in comments {
            let record = match parse_record(&comment.body) {
                Ok(record) => record,
                Err(RecordError::NoBlock) => continue,
                Err(error) => {
                    return Err(Error::wrap(format!("parse record {}", comment.url), error));
                }
            };
            if row.records == 0 {
                row.first_record = nanos_between(pull.opened, comment.created);
            }
            row.records += 1;
            let earlier = found.len();
            let mut updated = vec![false; earlier];
            for finding in record.findings {
                if let Some(i) = earlier_match(&found[..earlier], &updated, &finding) {
                    found[i].outcome = finding.outcome;
                    updated[i] = true;
                    continue;
                }
                found.push(finding);
            }
        }
        for finding in &found {
            *row.places.entry(place(finding)).or_default() += 1;
            if let Some(i) = PRIORITIES.iter().position(|p| *p == finding.priority) {
                row.findings.priority[i] += 1;
            }
            if let Some(i) = OUTCOMES.iter().position(|o| *o == finding.outcome.status) {
                row.findings.outcome[i] += 1;
            }
        }
        rows.push(row);
    }
    rows.sort_by_key(|r| r.number);
    Ok(rows)
}

fn nanos_between(from: Timestamp, to: Timestamp) -> i64 {
    let nanos = to.as_nanosecond() - from.as_nanosecond();
    i64::try_from(nanos).unwrap_or(if nanos < 0 { i64::MIN } else { i64::MAX })
}

fn earlier_match(earlier: &[Finding], updated: &[bool], f: &Finding) -> Option<usize> {
    earlier.iter().zip(updated).position(|(e, updated)| {
        !updated
            && e.source == f.source
            && e.priority == f.priority
            && e.finding == f.finding
            && e.location() == f.location()
    })
}

/// A finding's location as it was recorded: a string from version 2, an object with a `path` in version 1.
#[derive(Deserialize)]
#[serde(untagged)]
enum Location {
    Text(String),
    Object { path: String },
}

/// The place of a finding's file: a location string starts with the path, ended by `:`, a space, `,` or
/// `;`. Text that does not start with a path, such as `pull request body`, names no file.
pub fn place(f: &Finding) -> Place {
    let path = match f
        .location
        .as_deref()
        .map(|raw| serde_json::from_str::<Location>(raw.get()))
    {
        Some(Ok(Location::Text(text))) => {
            let end = text
                .find(|c: char| c == ':' || c == ',' || c == ';' || c.is_whitespace())
                .unwrap_or(text.len());
            let path = &text[..end];
            let is_path = path.contains(['/', '.']) || text[end..].starts_with(':');
            if path.is_empty() || !is_path {
                return Place::Unknown;
            }
            path.to_owned()
        }
        Some(Ok(Location::Object { path })) if !path.is_empty() => path,
        Some(Ok(Location::Object { .. }) | Err(_)) | None => return Place::Unknown,
    };
    let mut dirs: Vec<&str> = path
        .split('/')
        .filter(|d| !d.is_empty() && *d != ".")
        .collect();
    dirs.pop();
    if dirs.is_empty() {
        return Place::Directory(".".to_owned());
    }
    dirs.truncate(2);
    Place::Directory(dirs.join("/"))
}

pub fn total(rows: &[Row]) -> Totals {
    let mut totals = Totals {
        pulls: crate::count(rows.len()),
        ..Totals::default()
    };
    for row in rows {
        if !row.check.is_empty() {
            totals.checked += 1;
        }
        if row.check == "success" {
            totals.successful += 1;
        }
        if row.records > 0 {
            totals.recorded += 1;
            totals.first_records.push(row.first_record);
        }
        totals.findings.add(&row.findings);
        for (place, n) in &row.places {
            *totals.places.entry(place.clone()).or_default() += n;
        }
    }
    totals.first_records.sort_unstable();
    totals
}

pub fn render(w: &mut dyn Write, rows: &[Row], totals: &Totals) -> Result<(), Error> {
    let mut table = String::from(
        "PR\tCHECK\tRECORDS\tP0\tP1\tP2\tP3\tFIXED\tREJECTED\tDEFERRED\tFIRST RECORD\tTITLE\n",
    );
    for row in rows {
        let check = if row.check.is_empty() {
            "none"
        } else {
            row.check.as_str()
        };
        let first = if row.records > 0 {
            format_duration(row.first_record)
        } else {
            "-".to_owned()
        };
        let _ = writeln!(
            table,
            "#{}\t{check}\t{}\t{}\t{first}\t{}",
            row.number,
            row.records,
            count_cells(&row.findings),
            row.title
        );
    }
    let _ = writeln!(
        table,
        "total\t{}/{}\t{}/{}\t{}",
        totals.checked,
        totals.pulls,
        totals.recorded,
        totals.pulls,
        count_cells(&totals.findings)
    );
    w.write_all(tabwrite(&table, 2).as_bytes())
        .map_err(|error| Error::wrap("write table", error))?;
    let f = &totals.findings;
    let mut sentences = vec![
        format!(
            "\n{} merged pull requests: {} with robotogi's review check on the head ({} successful), {} with a review record.\n",
            totals.pulls, totals.checked, totals.successful, totals.recorded
        ),
        format!(
            "Findings: P0 {}, P1 {}, P2 {}, P3 {}; fixed {}, rejected {}, deferred {}.\n",
            f.priority[0],
            f.priority[1],
            f.priority[2],
            f.priority[3],
            f.outcome[0],
            f.outcome[1],
            f.outcome[2]
        ),
    ];
    if let Some(longest) = totals.first_records.last() {
        sentences.push(format!(
            "Time from opening to the first record: median {}, longest {}.\n",
            format_duration(median(&totals.first_records)),
            format_duration(*longest)
        ));
    }
    for sentence in sentences {
        w.write_all(sentence.as_bytes())
            .map_err(|error| Error::wrap("write totals", error))?;
    }
    if totals.places.is_empty() {
        return Ok(());
    }
    let mut places: Vec<(&Place, &i64)> = totals.places.iter().collect();
    places.sort_by(|(a, m), (b, n)| {
        (**a == Place::Unknown)
            .cmp(&(**b == Place::Unknown))
            .then(n.cmp(m))
            .then(a.cmp(b))
    });
    let mut table = String::from("\nFindings by directory, each counted once:\n");
    for (place, n) in places {
        let name = match place {
            Place::Directory(dir) => dir.as_str(),
            Place::Unknown => "unknown",
        };
        let _ = writeln!(table, "{name}\t{n}");
    }
    w.write_all(tabwrite(&table, 2).as_bytes())
        .map_err(|error| Error::wrap("write directories", error))
}

fn count_cells(c: &Counts) -> String {
    c.priority
        .iter()
        .chain(&c.outcome)
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join("\t")
}

/// The middle of `sorted` nanosecond durations; the mean of the two middle ones for an even count.
pub fn median(sorted: &[i64]) -> i64 {
    let n = sorted.len();
    if n % 2 == 1 {
        return sorted[n / 2];
    }
    sorted[n / 2 - 1].midpoint(sorted[n / 2])
}

/// `nanos` rounded to the minute, half away from zero, as `5m` or `2h05m`.
pub fn format_duration(nanos: i64) -> String {
    let d = round_minute(nanos);
    let (h, m) = (d / NANOS_PER_HOUR, d % NANOS_PER_HOUR / NANOS_PER_MINUTE);
    if h == 0 {
        format!("{m}m")
    } else {
        format!("{h}h{m:02}m")
    }
}

fn round_minute(d: i64) -> i64 {
    let r = (d % NANOS_PER_MINUTE).abs();
    let less_than_half = r + r < NANOS_PER_MINUTE;
    if d < 0 {
        if less_than_half {
            return d + r;
        }
        return d.checked_sub(NANOS_PER_MINUTE - r).unwrap_or(i64::MIN);
    }
    if less_than_half {
        return d - r;
    }
    d.checked_add(NANOS_PER_MINUTE - r).unwrap_or(i64::MAX)
}

/// `text` aligned in columns: cells end at tabs, a column is as wide as its widest cell in a run of
/// consecutive lines that have it, plus `padding` spaces, and a line's last cell is not aligned.
pub fn tabwrite(text: &str, padding: usize) -> String {
    let mut lines: Vec<Vec<&str>> = text
        .split('\n')
        .map(|line| line.split('\t').collect())
        .collect();
    let tail = lines
        .pop()
        .map(|cells| cells.join("\t"))
        .unwrap_or_default();
    let mut out = String::with_capacity(text.len() * 2);
    let mut widths = Vec::new();
    format_block(&lines, &mut out, &mut widths, 0, lines.len(), padding);
    out.push_str(&tail);
    out
}

fn format_block(
    lines: &[Vec<&str>],
    out: &mut String,
    widths: &mut Vec<usize>,
    mut line0: usize,
    line1: usize,
    padding: usize,
) {
    let column = widths.len();
    let mut this = line0;
    while this < line1 {
        if column + 1 >= lines[this].len() {
            this += 1;
            continue;
        }
        write_lines(lines, out, widths, line0, this);
        line0 = this;
        let mut width = 0;
        while this < line1 {
            let line = &lines[this];
            if column + 1 >= line.len() {
                break;
            }
            width = width.max(line[column].chars().count() + padding);
            this += 1;
        }
        widths.push(width);
        format_block(lines, out, widths, line0, this, padding);
        widths.pop();
        line0 = this;
        this += 1;
    }
    write_lines(lines, out, widths, line0, line1);
}

fn write_lines(
    lines: &[Vec<&str>],
    out: &mut String,
    widths: &[usize],
    line0: usize,
    line1: usize,
) {
    for line in &lines[line0..line1] {
        for (j, cell) in line.iter().enumerate() {
            out.push_str(cell);
            if let Some(width) = widths.get(j) {
                let pad = width.saturating_sub(cell.chars().count());
                out.extend(std::iter::repeat_n(' ', pad));
            }
        }
        out.push('\n');
    }
}

#[cfg(test)]
#[path = "tests/report.rs"]
mod tests;
