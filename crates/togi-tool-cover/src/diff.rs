//! The changed lines of a `git diff -U0`, and their intersection with the lines no test reached.

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use crate::Error;

/// Lines `first` through `last` of a file, both included and `first <= last`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineRange {
    pub first: u32,
    pub last: u32,
}

impl LineRange {
    pub fn new(first: u32, last: u32) -> Self {
        Self { first, last }
    }

    pub fn contains(self, line: u32) -> bool {
        self.first <= line && line <= self.last
    }

    pub fn overlaps(self, other: Self) -> bool {
        self.first.max(other.first) <= self.last.min(other.last)
    }
}

/// The new-side lines each file gained or changed, by repository-relative path in path order.
pub type Changed = BTreeMap<String, Vec<LineRange>>;

/// Changed lines no test reached, printed `path:line` or `path:first-last`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Gap {
    pub path: String,
    pub lines: LineRange,
}

impl fmt::Display for Gap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let LineRange { first, last } = self.lines;
        if first == last {
            write!(f, "{}:{first}", self.path)
        } else {
            write!(f, "{}:{first}-{last}", self.path)
        }
    }
}

/// The new-side lines each file gained or changed in a `git diff -U0 --no-prefix`.
pub fn parse_diff(diff: &[u8]) -> Result<Changed, Error> {
    let mut changed = Changed::new();
    let mut path: Option<String> = None;
    let mut header = false;
    for line in diff.split(|&b| b == b'\n') {
        let line = String::from_utf8_lossy(line.strip_suffix(b"\r").unwrap_or(line));
        if line.starts_with("diff --git ") {
            header = true;
            path = None;
        } else if let (true, Some(new)) = (header, line.strip_prefix("+++ ")) {
            path = (new != "/dev/null").then(|| new.to_owned());
        } else if line.starts_with("@@ ") {
            header = false;
            let Some(path) = &path else {
                continue;
            };
            if let Some(lines) = parse_hunk(&line)? {
                changed.entry(path.clone()).or_default().push(lines);
            }
        }
    }
    Ok(changed)
}

/// The new-side lines of a hunk header, or `None` when the hunk only deletes.
fn parse_hunk(line: &str) -> Result<Option<LineRange>, Error> {
    let malformed = || Error::MalformedHunk(line.to_owned());
    let new = line
        .split_whitespace()
        .nth(2)
        .and_then(|field| field.strip_prefix('+'))
        .ok_or_else(malformed)?;
    let (start, count) = new.split_once(',').unwrap_or((new, "1"));
    let first: u32 = start.parse().map_err(|_| malformed())?;
    let count: u32 = count.parse().map_err(|_| malformed())?;
    if count == 0 {
        return Ok(None);
    }
    let last = first.checked_add(count - 1).ok_or_else(malformed)?;
    Ok(Some(LineRange::new(first, last)))
}

/// The changed lines inside unreached ranges, merged into ranges and sorted by path and line.
pub fn intersect(changed: &Changed, unreached: &HashMap<String, Vec<LineRange>>) -> Vec<Gap> {
    let mut gaps: Vec<Gap> = Vec::new();
    for (path, changes) in changed {
        let Some(blocks) = unreached.get(path) else {
            continue;
        };
        let mut hits: Vec<LineRange> = blocks
            .iter()
            .flat_map(|block| {
                changes.iter().filter_map(|c| {
                    let (first, last) = (block.first.max(c.first), block.last.min(c.last));
                    (first <= last).then_some(LineRange::new(first, last))
                })
            })
            .collect();
        hits.sort_by_key(|hit| hit.first);
        for hit in hits {
            if let Some(gap) = gaps.last_mut()
                && gap.path == *path
                && hit.first <= gap.lines.last.saturating_add(1)
            {
                gap.lines.last = gap.lines.last.max(hit.last);
                continue;
            }
            gaps.push(Gap {
                path: path.clone(),
                lines: hit,
            });
        }
    }
    gaps
}

#[cfg(test)]
#[path = "tests/diff.rs"]
mod tests;
