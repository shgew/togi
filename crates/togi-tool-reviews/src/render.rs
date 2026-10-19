//! The review record, from the snapshot's manifest and the coordinator's findings: `record.md` for people
//! and its version 3 block, and `record.json` with the same block.

use std::fmt::Write as _;
use std::io::Write;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize as _, Serialize};
use togi_machine::object_only;

use crate::manifest::{Manifest, read_manifest};
use crate::record::{PRIORITIES, RECORD_CLOSE, RECORD_OPEN};
use crate::text::{base, clean, join, json, nullable, quote};
use crate::{Error, files};

pub const RECORD_FILE: &str = "record.md";
pub const RECORD_JSON_FILE: &str = "record.json";
pub const RENDER_USAGE: &str = "usage: reviews render <snapshot directory> <findings file>";

static FULL_SHA: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"^[0-9a-f]{40}$"));

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde passes the field by reference"
)]
fn is_zero(value: &i64) -> bool {
    *value == 0
}

object_only! {
    /// One review record. Its JSON form is the version 3 block of docs/review-record.md; `markdown` writes
    /// the same data for people. The skipped fields are the few things only the Markdown carries.
    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct RecordModel {
        pub version: i64,
        pub repository: String,
        pub pull_request: i64,
        pub head_sha: String,
        pub base_sha: String,
        pub previous: Option<RecordPrevious>,
        pub excluded_files: Vec<RecordExcluded>,
        pub files: Vec<String>,
        pub reviewers: Vec<RecordReviewer>,
        pub findings: Vec<RecordFinding>,
        pub coverage: Option<RecordCoverage>,
        pub verdict: String,
        #[serde(skip)]
        pub blocker: String,
        #[serde(skip)]
        pub fallback: String,
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct RecordPrevious {
        pub url: String,
        pub head_sha: String,
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct RecordExcluded {
        pub path: String,
        pub reason: String,
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct RecordReviewer {
        #[serde(default, deserialize_with = "nullable")]
        pub name: String,
        #[serde(default)]
        pub model: Option<String>,
        #[serde(default, deserialize_with = "nullable")]
        pub files: Vec<String>,
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct RecordFinding {
        #[serde(default, deserialize_with = "nullable")]
        pub source: String,
        #[serde(default, deserialize_with = "nullable")]
        pub priority: String,
        #[serde(default, deserialize_with = "nullable")]
        pub finding: String,
        #[serde(default)]
        pub location: Option<String>,
        #[serde(default)]
        pub url: Option<String>,
        #[serde(default, deserialize_with = "nullable")]
        pub outcome: RecordOutcome,
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct RecordOutcome {
        #[serde(default, deserialize_with = "nullable")]
        pub status: String,
        #[serde(default, deserialize_with = "nullable", skip_serializing_if = "String::is_empty")]
        pub sha: String,
        /// What a fix made only in the pull request body changed, for a fix with no commit.
        #[serde(default, deserialize_with = "nullable", skip_serializing_if = "String::is_empty")]
        pub body: String,
        #[serde(default, deserialize_with = "nullable", skip_serializing_if = "String::is_empty")]
        pub reason: String,
        #[serde(default, deserialize_with = "nullable", skip_serializing_if = "is_zero")]
        pub issue: i64,
    }

    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct RecordCoverage {
        pub uncovered: i64,
        pub judgment: String,
    }

    /// The coordinator's judgment, the one input render takes besides the manifest.
    #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct FindingsInput {
        /// Cover every included file; a coordinator that reviewed a small pull request lists itself.
        #[serde(default, deserialize_with = "nullable")]
        pub reviewers: Vec<RecordReviewer>,
        #[serde(default, deserialize_with = "nullable")]
        pub findings: Vec<RecordFinding>,
        /// How the uncovered ranges were judged; required when the diff changes Go or Rust code.
        #[serde(default, deserialize_with = "nullable")]
        pub coverage_judgment: String,
        /// A blocker the findings do not show, such as an unresolved review thread or an open finding from
        /// an earlier record.
        #[serde(default, deserialize_with = "nullable")]
        pub blocked: String,
    }
}

pub fn run_render(stdout: &mut dyn Write, args: &[String]) -> Result<(), Error> {
    let [dir, findings_file] = args else {
        return Err(Error::new(RENDER_USAGE));
    };
    let m = read_manifest(dir)?;
    let raw = files::read(findings_file).map_err(|error| Error::wrap("read findings", error))?;
    let input = decode_findings(&raw)
        .map_err(|error| Error::wrap(format!("decode findings {findings_file}"), error))?;
    let model = build_record(&m, input)?;
    let block = model.json_block()?;
    let md = format!("{}\n{RECORD_OPEN}{block}{RECORD_CLOSE}\n", model.markdown());
    let record_path = join(dir, RECORD_FILE);
    let json_path = join(dir, RECORD_JSON_FILE);
    files::write(&record_path, md.as_bytes())
        .map_err(|error| Error::wrap(format!("write {RECORD_FILE}"), error))?;
    files::write(&json_path, format!("{block}\n").as_bytes())
        .map_err(|error| Error::wrap(format!("write {RECORD_JSON_FILE}"), error))?;
    write!(
        stdout,
        "{}: {} on {}\nwrote {record_path} and {json_path}\n",
        base(&clean(dir)),
        model.verdict,
        model.head_sha
    )
    .map_err(Error::cause)
}

/// Exactly one JSON object with known fields. Anything after it but whitespace, such as a second object,
/// is refused rather than silently dropped.
pub fn decode_findings(raw: &[u8]) -> Result<FindingsInput, Error> {
    let mut de = serde_json::Deserializer::from_slice(raw);
    let input = FindingsInput::deserialize(&mut de).map_err(Error::cause)?;
    if de.end().is_err() {
        return Err(Error::new(
            "data follows the findings object; the file must hold exactly one JSON object",
        ));
    }
    Ok(input)
}

/// Checks the coordinator's findings against the manifest and joins them into one record.
pub fn build_record(m: &Manifest, input: FindingsInput) -> Result<RecordModel, Error> {
    if m.carry_forward {
        return Err(Error::new(
            "every patch is unchanged: carry the record forward with publish; there is no record to render",
        ));
    }
    let mut r = RecordModel {
        version: 3,
        repository: m.repository.clone(),
        pull_request: m.pull_request,
        head_sha: m.head_sha.clone(),
        base_sha: m.base_sha.clone(),
        previous: m.previous.as_ref().map(|p| RecordPrevious {
            url: p.url.clone(),
            head_sha: p.head_sha.clone(),
        }),
        fallback: m.fallback.clone(),
        ..RecordModel::default()
    };
    for f in &m.files {
        if f.exclusion.is_empty() {
            r.files.push(f.path.clone());
        } else {
            r.excluded_files.push(RecordExcluded {
                path: f.path.clone(),
                reason: f.exclusion.clone(),
            });
        }
    }
    check_reviewers(&r.files, &input.reviewers)?;
    r.reviewers = input.reviewers;
    let mut open: Vec<&str> = Vec::new();
    for f in &input.findings {
        check_finding(f)?;
        if f.outcome.status == "deferred"
            && (f.priority == "P0" || f.priority == "P1")
            && !open.contains(&f.priority.as_str())
        {
            open.push(&f.priority);
        }
    }
    if m.changes_code() {
        let Some(coverage) = &m.coverage else {
            return Err(Error::new(
                "the diff changes Go or Rust code but the snapshot has no coverage: run `just cover <base>` and pass the output to `snapshot --cover`",
            ));
        };
        if input.coverage_judgment.trim().is_empty() {
            return Err(Error::new(
                "coverage_judgment is required: the diff changes Go or Rust code",
            ));
        }
        r.coverage = Some(RecordCoverage {
            uncovered: crate::saturating_record_count(coverage.ranges.len()),
            judgment: input.coverage_judgment.clone(),
        });
    } else if !input.coverage_judgment.is_empty() {
        return Err(Error::new(
            "coverage_judgment given, but the diff changes no Go or Rust code",
        ));
    }
    open.sort_unstable();
    let mut reasons = Vec::new();
    if !open.is_empty() {
        reasons.push(format!("{} open", open.join(", ")));
    }
    let blocked = input.blocked.trim();
    let blocked = blocked.strip_suffix('.').unwrap_or(blocked);
    if !blocked.is_empty() {
        reasons.push(blocked.to_owned());
    }
    r.blocker = reasons.join("; ");
    if r.blocker.is_empty() {
        "success"
    } else {
        "blocked"
    }
    .clone_into(&mut r.verdict);
    r.findings = input.findings;
    Ok(r)
}

fn check_reviewers(included: &[String], reviewers: &[RecordReviewer]) -> Result<(), Error> {
    if included.is_empty() {
        if !reviewers.is_empty() {
            return Err(Error::new(
                "no file is included, so the record has no reviewers",
            ));
        }
        return Ok(());
    }
    if reviewers.is_empty() {
        return Err(Error::new(
            "reviewers are required: the record covers files",
        ));
    }
    let mut covered = std::collections::HashSet::new();
    for rv in reviewers {
        if rv.name.is_empty() {
            return Err(Error::new("a reviewer has no name"));
        }
        for f in &rv.files {
            if !included.contains(f) {
                return Err(Error::new(format!(
                    "reviewer {} covers {f}, which the snapshot does not include",
                    rv.name
                )));
            }
            covered.insert(f.as_str());
        }
    }
    if let Some(f) = included.iter().find(|f| !covered.contains(f.as_str())) {
        return Err(Error::new(format!("no reviewer covers {f}")));
    }
    Ok(())
}

fn check_finding(f: &RecordFinding) -> Result<(), Error> {
    if f.source.is_empty() || f.finding.is_empty() {
        return Err(Error::new("a finding needs a source and text"));
    }
    if !PRIORITIES.contains(&f.priority.as_str()) {
        return Err(Error::new(format!(
            "finding from {}: unknown priority {}",
            f.source,
            quote(&f.priority)
        )));
    }
    let full_sha = FULL_SHA
        .as_ref()
        .map_err(|error| Error::wrap("compile a pattern", error.clone()))?;
    let o = &f.outcome;
    let valid = match o.status.as_str() {
        "fixed" => {
            let commit = full_sha.is_match(&o.sha) && o.body.is_empty();
            let body = o.sha.is_empty() && !o.body.is_empty();
            (commit || body) && o.reason.is_empty() && o.issue == 0
        }
        "rejected" => !o.reason.is_empty() && o.sha.is_empty() && o.body.is_empty() && o.issue == 0,
        "deferred" => o.issue > 0 && o.sha.is_empty() && o.body.is_empty() && o.reason.is_empty(),
        _ => false,
    };
    if !valid {
        return Err(Error::new(format!(
            "finding from {}: outcome must be fixed with a full sha or a description of the pull request body edit, rejected with a reason, or deferred with an issue number",
            f.source
        )));
    }
    Ok(())
}

impl RecordModel {
    /// The record's compact one-line JSON; `<`, `>` and `&` are escaped so finding text cannot end the
    /// HTML comment around it.
    pub fn json_block(&self) -> Result<String, Error> {
        json(self).map_err(|error| Error::wrap("encode record", error))
    }

    pub fn markdown(&self) -> String {
        let mut b = String::from("## Review record\n\n");
        if self.blocker.is_empty() {
            let _ = write!(b, "**{}** on {}.\n\n", self.verdict, self.head_sha);
        } else {
            let _ = write!(
                b,
                "**{}** on {}: {}.\n\n",
                self.verdict,
                self.head_sha,
                text(&self.blocker)
            );
        }
        if self.findings.is_empty() {
            b.push_str("No findings.\n");
        } else {
            b.push_str("| Source | Priority | Finding | Outcome |\n|---|---|---|---|\n");
            for f in &self.findings {
                let mut source = cell(&f.source);
                if let Some(url) = &f.url {
                    source = format!("[{source}]({})", entities(url));
                }
                let mut finding = cell(&f.finding);
                if let Some(location) = &f.location {
                    let _ = write!(finding, " ({})", code(&fields(location)));
                }
                let _ = writeln!(
                    b,
                    "| {source} | {} | {finding} | {} |",
                    f.priority,
                    cell(&f.outcome.text())
                );
            }
        }
        b.push_str("\n<details>\n<summary>Review data</summary>\n\n");
        let _ = writeln!(b, "- Base: {}", self.base_sha);
        if let Some(previous) = &self.previous {
            let _ = writeln!(
                b,
                "- Previous: {}, range {}..{}",
                previous.url, previous.head_sha, self.head_sha
            );
        }
        if !self.fallback.is_empty() {
            let _ = writeln!(
                b,
                "- Fallback: the whole layer is covered because {}.",
                self.fallback.strip_suffix('.').unwrap_or(&self.fallback)
            );
        }
        let _ = writeln!(b, "- Files: {}", code_list(&self.files));
        if !self.excluded_files.is_empty() {
            let parts: Vec<String> = self
                .excluded_files
                .iter()
                .map(|e| format!("{} ({})", code(&e.path), e.reason))
                .collect();
            let _ = writeln!(b, "- Excluded: {}", parts.join(", "));
        }
        let parts: Vec<String> = self
            .reviewers
            .iter()
            .map(|rv| {
                let mut part = text(&rv.name);
                if let Some(model) = &rv.model {
                    let _ = write!(part, " ({})", text(model));
                }
                format!("{part}: {}", code_list(&rv.files))
            })
            .collect();
        let _ = writeln!(b, "- Reviewers: {}", or_none(parts.join("; ")));
        if let Some(c) = &self.coverage {
            let noun = if c.uncovered == 1 { "range" } else { "ranges" };
            let _ = writeln!(
                b,
                "- Coverage: {} uncovered {noun}. {}",
                c.uncovered,
                text(&c.judgment)
            );
        }
        b.push_str("\n</details>\n");
        b
    }
}

impl RecordOutcome {
    fn text(&self) -> String {
        match self.status.as_str() {
            "fixed" if self.sha.is_empty() => {
                format!("fixed in the pull request body: {}", self.body)
            }
            "fixed" => format!("fixed in {}", self.sha),
            "rejected" => format!("rejected: {}", self.reason),
            _ => format!("deferred: #{}", self.issue),
        }
    }
}

fn fields(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Text safe for one table cell: joined onto one line and written as `text` does.
fn cell(s: &str) -> String {
    text(&fields(s))
}

/// `&`, `<` and `>` as HTML entities, which Markdown shows as the characters themselves.
fn entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

fn push_entity(b: &mut Vec<u8>, c: u8) {
    match c {
        b'&' => b.extend_from_slice(b"&amp;"),
        b'<' => b.extend_from_slice(b"&lt;"),
        b'>' => b.extend_from_slice(b"&gt;"),
        c => b.push(c),
    }
}

fn into_text(b: Vec<u8>) -> String {
    String::from_utf8(b)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

/// Human text written so Markdown shows it as written and it holds no HTML comment or tag, leaving the
/// record's block the only hidden one. Outside code spans it writes `&`, `<` and `>` as entities and escapes
/// `|`; a backslash before one of them becomes an entity so it cannot undo that. A code span, a run of
/// backticks closed by the next run of exactly as many, keeps its meaning but is written as `<code>` with
/// literal content, since a backtick span would show entities verbatim and would carry markers as raw bytes.
/// An unmatched run is literal backticks.
pub fn text(s: &str) -> String {
    let s = s.as_bytes();
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let byte = s[i];
        let next = s.get(i + 1).copied();
        if byte == b'\\' && next.is_some_and(|n| b"&<>|".contains(&n)) {
            out.extend_from_slice(b"&#92;");
            i += 1;
        } else if byte == b'\\' && matches!(next, Some(b'`' | b'\\')) {
            out.extend_from_slice(&s[i..i + 2]);
            i += 2;
        } else if byte == b'`' {
            let run = backtick_run(s, i);
            if let Some((content, end)) = code_span(s, i + run, run) {
                out.extend_from_slice(b"<code>");
                out.extend_from_slice(literal(&content).as_bytes());
                out.extend_from_slice(b"</code>");
                i = end;
            } else {
                out.extend_from_slice(&s[i..i + run]);
                i += run;
            }
        } else if byte == b'|' {
            out.extend_from_slice(b"\\|");
            i += 1;
        } else {
            push_entity(&mut out, byte);
            i += 1;
        }
    }
    into_text(out)
}

/// The length of the run of backticks at `s[i]`.
fn backtick_run(s: &[u8], i: usize) -> usize {
    s[i..].iter().take_while(|c| **c == b'`').count()
}

/// The content of the code span whose content starts at `s[start]`, closed by the next run of exactly `n`
/// backticks, as Markdown shows it, and the end of the closing run: line endings as spaces, one space
/// stripped from each side when both sides have one and it is not all spaces. A blank line ends the
/// paragraph, so no span crosses one.
fn code_span(s: &[u8], start: usize, n: usize) -> Option<(Vec<u8>, usize)> {
    let mut j = start;
    while j < s.len() {
        if s[j] != b'`' {
            j += 1;
            continue;
        }
        let m = backtick_run(s, j);
        if m != n {
            j += m;
            continue;
        }
        let content = &s[start..j];
        if content.windows(2).any(|w| w == b"\n\n") {
            return None;
        }
        let mut content: Vec<u8> = content
            .iter()
            .map(|c| if *c == b'\n' { b' ' } else { *c })
            .collect();
        if content.len() > 1
            && content[0] == b' '
            && content[content.len() - 1] == b' '
            && content.iter().any(|c| *c != b' ')
        {
            content = content[1..content.len() - 1].to_vec();
        }
        return Some((content, j + m));
    }
    None
}

/// `s` for inside `<code>`: Markdown still parses there, so every ASCII punctuation character is an entity
/// and shows as itself.
fn literal(s: &[u8]) -> String {
    let mut b = Vec::with_capacity(s.len());
    for &c in s {
        if c == b'&' || c == b'<' || c == b'>' {
            push_entity(&mut b, c);
        } else if b"!\"#$%'()*+,-./:;=?@[\\]^_`{|}~".contains(&c) {
            b.extend_from_slice(format!("&#{c};").as_bytes());
        } else {
            b.push(c);
        }
    }
    into_text(b)
}

/// `s`, such as a path or location, as code: a backtick span when that shows it verbatim, otherwise
/// `<code>` with literal content.
fn code(s: &str) -> String {
    if !s.is_empty()
        && !s.contains(['`', '&', '<', '>', '|'])
        && !s.starts_with(' ')
        && !s.ends_with(' ')
    {
        return format!("`{s}`");
    }
    format!("<code>{}</code>", literal(s.as_bytes()))
}

fn code_list(items: &[String]) -> String {
    or_none(items.iter().map(|s| code(s)).collect::<Vec<_>>().join(", "))
}

fn or_none(s: String) -> String {
    if s.is_empty() { "none".to_owned() } else { s }
}

#[cfg(test)]
#[path = "tests/render.rs"]
pub(crate) mod tests;
