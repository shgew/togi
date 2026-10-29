//! Changed lines numbered in the frozen head, distinct from commit-local patch spans.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::LazyLock;

use regex::bytes::Regex;
use serde::{Deserialize, Serialize};

use crate::diff::{FileDiff, Span, line_number, lines, number, parse_diff, pattern};
use crate::{Error, Programs};

/// An inclusive range in the frozen head's version of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadSpan {
    first: i64,
    last: i64,
}

/// Head paths in stable order for the frozen manifest.
pub type HeadLines = BTreeMap<String, Vec<HeadSpan>>;

impl HeadSpan {
    /// Takes the new side of a diff whose destination is the frozen head.
    fn from_new_side(span: Span) -> Self {
        Self {
            first: span.first,
            last: span.last,
        }
    }

    pub fn first(self) -> i64 {
        self.first
    }

    pub fn last(self) -> i64 {
        self.last
    }
}

/// Included files of a pull request diff, whose new side is already the frozen head.
pub fn from_head_diff<'a>(files: impl Iterator<Item = &'a FileDiff>) -> HeadLines {
    let mut out = HeadLines::new();
    for file in files {
        out.entry(file.path.clone())
            .or_default()
            .extend(file.changed.iter().copied().map(HeadSpan::from_new_side));
    }
    out
}

/// A delta commit's changed lines before its file sections are merged for presentation.
pub struct CommitLines {
    pub commit: String,
    pub files: Vec<FileLines>,
}

pub struct FileLines {
    pub path: String,
    pub spans: Vec<Span>,
}

static HUNK_HEADER: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"(?-u)^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@"));

struct Shift {
    old: i64,
    removed: i64,
    added: i64,
}

fn shifts(diff: &[u8]) -> Result<Vec<Shift>, Error> {
    let header = pattern(&HUNK_HEADER)?;
    lines(diff)
        .filter(|line| line.starts_with(b"@@ "))
        .map(|line| {
            let m = header
                .captures(line)
                .ok_or_else(|| Error::new("parse head-coordinate hunk header"))?;
            Ok(Shift {
                old: number(&m[1])?,
                removed: m.get(2).map_or(Ok(1), |n| number(n.as_bytes()))?,
                added: m.get(4).map_or(Ok(1), |n| number(n.as_bytes()))?,
            })
        })
        .collect()
}

/// Splits intervals around later replacements, carrying only the surviving lines forward.
fn shifted(spans: &[Span], shifts: &[Shift]) -> Result<Vec<HeadSpan>, Error> {
    let mut out = Vec::new();
    for span in spans {
        let (mut next, last) = (i128::from(span.first), i128::from(span.last));
        let mut offset = 0_i128;
        for shift in shifts {
            let old = i128::from(shift.old);
            // A zero-width old range inserts after `old`, not before that line.
            let boundary = old + i128::from(shift.removed == 0);
            let end = last.min(boundary - 1);
            if next <= end {
                out.push(HeadSpan {
                    first: line_number(next + offset)?,
                    last: line_number(end + offset)?,
                });
            }
            next = next.max(boundary.max(old + i128::from(shift.removed)));
            offset += i128::from(shift.added) - i128::from(shift.removed);
        }
        if next <= last {
            out.push(HeadSpan {
                first: line_number(next + offset)?,
                last: line_number(last + offset)?,
            });
        }
    }
    Ok(out)
}

/// Name-status output uses NUL delimiters even for paths containing tabs or newlines.
fn destinations(raw: &[u8]) -> Result<HashMap<&str, Option<&str>>, Error> {
    let text = std::str::from_utf8(raw)
        .map_err(|error| Error::wrap("decode head-coordinate path changes", error))?;
    let mut fields = text.split_terminator('\0');
    let mut paths = HashMap::new();
    while let Some(status) = fields.next() {
        let old = fields
            .next()
            .ok_or_else(|| Error::new("parse head-coordinate path changes: missing path"))?;
        let destination = if status.starts_with('R') || status.starts_with('C') {
            let new = fields.next().ok_or_else(|| {
                Error::new("parse head-coordinate path changes: missing destination")
            })?;
            if status.starts_with('C') {
                continue;
            }
            Some(new)
        } else if status == "D" {
            None
        } else {
            Some(old)
        };
        paths.insert(old, destination);
    }
    Ok(paths)
}

fn source_shifts(diff: &[u8], source: &str) -> Result<Vec<Shift>, Error> {
    let sections = parse_diff(diff)?;
    let section = sections
        .iter()
        .find(|section| {
            let old = if section.old_path.is_empty() {
                &section.path
            } else {
                &section.old_path
            };
            old == source
        })
        .ok_or_else(|| {
            Error::new(format!(
                "find source section for {source} in head-coordinate diff"
            ))
        })?;
    // A mode-only change or pure rename has a section but no hunks, so maps identically.
    // A missing section contradicts name-status; refusing it avoids silently losing later edits.
    shifts(&section.text)
}

/// Maps delta lines from their own commits to the frozen head, following renames and dropping later edits.
pub fn translate(
    git: &mut dyn Programs,
    entries: &[CommitLines],
    head: &str,
    included: &HashSet<&str>,
) -> Result<HeadLines, Error> {
    let mut out = HeadLines::new();
    for entry in entries {
        let commit = &entry.commit;
        if !entry
            .files
            .iter()
            .any(|f| included.contains(f.path.as_str()))
        {
            continue;
        }
        let operation = format!("map delta lines from {commit} to frozen head {head}");
        let raw = git
            .git(&[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "-M",
                "--name-status",
                "-z",
                commit,
                head,
            ])
            .map_err(|error| Error::wrap(&operation, error))?;
        let paths = destinations(&raw).map_err(|error| Error::wrap(&operation, error))?;
        for file in &entry.files {
            if !included.contains(file.path.as_str()) {
                continue;
            }
            let destination = match paths.get(file.path.as_str()) {
                Some(None) => continue,
                Some(Some(path)) => *path,
                None => &file.path,
            };
            let spans = if paths.contains_key(file.path.as_str()) {
                let old = format!(":(literal){}", file.path);
                let new = (destination != file.path).then(|| format!(":(literal){destination}"));
                let args = [
                    "diff",
                    "--no-color",
                    "--no-ext-diff",
                    "--no-textconv",
                    "-U0",
                    "--inter-hunk-context=0",
                    "-M",
                    "--src-prefix=a/",
                    "--dst-prefix=b/",
                    commit,
                    head,
                    "--",
                    &old,
                    new.as_deref().unwrap_or(&old),
                ];
                let count = if new.is_some() {
                    args.len()
                } else {
                    args.len() - 1
                };
                let diff = git
                    .git(&args[..count])
                    .map_err(|error| Error::wrap(&operation, error))?;
                let shifts = source_shifts(&diff, &file.path)
                    .map_err(|error| Error::wrap(&operation, error))?;
                shifted(&file.spans, &shifts).map_err(|error| Error::wrap(&operation, error))?
            } else {
                file.spans
                    .iter()
                    .copied()
                    .map(HeadSpan::from_new_side)
                    .collect()
            };
            out.entry(destination.to_owned()).or_default().extend(spans);
        }
    }
    Ok(out)
}

#[cfg(test)]
#[path = "tests/headlines.rs"]
mod tests;
