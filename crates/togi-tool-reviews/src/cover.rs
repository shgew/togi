//! `just cover` output, kept where it meets the snapshot's hunks.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::diff::{Span, number, parse_diff};
use crate::manifest::{CoverRange, Coverage, Manifest, NotBuilt, read_manifest, write_manifest};
use crate::text::{join, quote};
use crate::{Error, files};

static COVER_RANGE_LINE: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"^(.+):([0-9]+)(?:-([0-9]+))?$"));
static COVER_NOT_BUILT_LINE: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"^(.+): not built for (.+)$"));

fn compiled(
    pattern: &'static LazyLock<Result<Regex, regex::Error>>,
) -> Result<&'static Regex, Error> {
    pattern
        .as_ref()
        .map_err(|error| Error::wrap("compile a pattern", error.clone()))
}

/// `just cover` output: `path:first-last` and `path:line` ranges, then `path: not built for <platform>`
/// lines. Its summary sentences are ignored.
pub fn parse_cover(text: &str) -> Result<(Vec<CoverRange>, Vec<NotBuilt>), Error> {
    let range_line = compiled(&COVER_RANGE_LINE)?;
    let not_built_line = compiled(&COVER_NOT_BUILT_LINE)?;
    let (mut ranges, mut unbuilt) = (Vec::new(), Vec::new());
    for line in text.split_inclusive('\n') {
        let line = line.trim();
        if let Some(m) = not_built_line.captures(line) {
            unbuilt.push(NotBuilt {
                path: m[1].to_owned(),
                target: m[2].to_owned(),
            });
            continue;
        }
        let Some(m) = range_line.captures(line) else {
            continue;
        };
        let parse = |digits: &str| {
            number(digits.as_bytes())
                .map_err(|error| Error::wrap(format!("parse cover range {}", quote(line)), error))
        };
        let first = parse(&m[2])?;
        let last = m
            .get(3)
            .map(|d| parse(d.as_str()))
            .transpose()?
            .unwrap_or(first);
        if last < first {
            return Err(Error::new(format!(
                "cover range {} ends before it starts",
                quote(line)
            )));
        }
        ranges.push(CoverRange {
            path: m[1].to_owned(),
            first,
            last,
        });
    }
    Ok((ranges, unbuilt))
}

/// The parts of the reported ranges that lie inside the hunks of their file, and the not-built files the
/// review covers. A range outside every hunk belongs to a patch the record does not cover and is dropped. A
/// delta's hunk file concatenates the patches of several commits, so their hunks can overlap; overlapping
/// parts of a file are merged so one uncovered interval counts once. Adjacent parts stay separate.
pub fn intersect_cover(
    ranges: &[CoverRange],
    unbuilt: &[NotBuilt],
    hunks: &HashMap<String, Vec<Span>>,
) -> Coverage {
    let mut parts = Vec::new();
    for r in ranges {
        for h in hunks.get(&r.path).into_iter().flatten() {
            let (first, last) = (r.first.max(h.first), r.last.min(h.last));
            if first <= last {
                parts.push(CoverRange {
                    path: r.path.clone(),
                    first,
                    last,
                });
            }
        }
    }
    parts.sort_by(|a, b| (&a.path, a.first, a.last).cmp(&(&b.path, b.first, b.last)));
    let mut merged: Vec<CoverRange> = Vec::with_capacity(parts.len());
    for r in parts {
        if let Some(prev) = merged.last_mut()
            && prev.path == r.path
            && r.first <= prev.last
        {
            prev.last = prev.last.max(r.last);
            continue;
        }
        merged.push(r);
    }
    Coverage {
        reported: crate::count(ranges.len()),
        ranges: merged,
        not_built: unbuilt
            .iter()
            .filter(|u| hunks.contains_key(&u.path))
            .cloned()
            .collect(),
    }
}

/// The changed new-side lines of every included file, from the snapshot's hunk files.
pub fn hunk_spans(dir: &str, m: &Manifest) -> Result<HashMap<String, Vec<Span>>, Error> {
    let mut hunks = HashMap::new();
    for f in m.included() {
        let bytes = files::read(&join(dir, &f.hunk_file))
            .map_err(|error| Error::wrap(format!("read hunk file for {}", f.path), error))?;
        let sections = parse_diff(&bytes)
            .map_err(|error| Error::wrap(format!("parse hunk file for {}", f.path), error))?;
        hunks.insert(
            f.path.clone(),
            sections.into_iter().flat_map(|s| s.changed).collect(),
        );
    }
    Ok(hunks)
}

/// Records the uncovered ranges inside the snapshot's hunks in its manifest, replacing earlier coverage.
pub fn add_cover(dir: &str, text: &str) -> Result<Manifest, Error> {
    let mut m = read_manifest(dir)?;
    let (ranges, unbuilt) = parse_cover(text)?;
    let hunks = hunk_spans(dir, &m)?;
    m.coverage = Some(intersect_cover(&ranges, &unbuilt, &hunks));
    write_manifest(dir, &m)?;
    Ok(m)
}

#[cfg(test)]
#[path = "tests/cover.rs"]
mod tests;

#[cfg(test)]
mod numeric_tests {
    use test_case::test_case;

    use super::*;

    #[test_case("f:9223372036854775808" ; "first overflow")]
    #[test_case("f:1-9223372036854775808" ; "last overflow")]
    fn coverage_line_overflow_is_refused(text: &str) {
        let error = parse_cover(text).unwrap_err();
        assert!(error.find::<std::num::ParseIntError>().is_some());
        assert_eq!(
            error.to_string(),
            format!("parse cover range {}", quote(text))
        );
    }

    #[test_case("f:9223372036854775807", i64::MAX ; "single")]
    #[test_case("f:1-9223372036854775807", 1 ; "range")]
    fn coverage_limit_is_exact(text: &str, first: i64) {
        let (ranges, _) = parse_cover(text).unwrap();
        assert_eq!(ranges[0].first, first);
        assert_eq!(ranges[0].last, i64::MAX);
    }
}
