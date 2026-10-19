//! Unified diffs, as git or gh print them, split into file sections.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::bytes::Regex;

use crate::Error;
use crate::text::{lossy, quote, unquote, unquote_prefix};

/// An inclusive range of line numbers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Span {
    pub first: i64,
    pub last: i64,
}

/// One hunk of a file's section.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hunk {
    /// The hunk verbatim, its `@@` line included.
    pub text: Vec<u8>,
    /// The old-side lines the hunk removes, merged into spans.
    pub removals: Vec<Span>,
    /// The text of the lines the hunk adds.
    pub added: Vec<Vec<u8>>,
}

/// One file's section of a unified diff.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    /// The source path of a rename or copy.
    pub old_path: String,
    /// `added`, `modified`, `deleted`, `renamed` or `copied`.
    pub status: String,
    pub binary: bool,
    pub added: i64,
    pub removed: i64,
    /// The section verbatim, header and hunks.
    pub text: Vec<u8>,
    /// The new-side lines the section adds, merged into spans.
    pub changed: Vec<Span>,
    /// The old-side lines the section removes, merged into spans.
    pub removals: Vec<Span>,
    /// The section's lines before its first hunk; the header and every hunk's text make the text.
    pub header: Vec<u8>,
    pub hunks: Vec<Hunk>,
    /// The section's mode lines (old mode, new mode, new file mode, deleted file mode), one per line.
    pub mode: Vec<u8>,
    /// The object a binary section's index line names as the file's new content.
    pub blob: Vec<u8>,
    /// The body of a binary section's GIT binary patch.
    pub payload: Vec<u8>,
}

const DIFF_HEADER: &[u8] = b"diff --git ";

static HUNK_HEADER: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"(?-u)^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@"));

/// A compiled constant pattern.
pub fn pattern(
    compiled: &'static LazyLock<Result<Regex, regex::Error>>,
) -> Result<&'static Regex, Error> {
    compiled
        .as_ref()
        .map_err(|error| Error::wrap("compile a pattern", error.clone()))
}

/// The lines of `text`, each with its newline.
pub fn lines(text: &[u8]) -> impl Iterator<Item = &[u8]> {
    text.split_inclusive(|b| *b == b'\n')
}

pub fn trim_newline(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n").unwrap_or(line)
}

pub fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A number the pattern matched as digits, represented exactly or refused.
pub fn number(digits: &[u8]) -> Result<i64, Error> {
    let digits = std::str::from_utf8(digits)
        .map_err(|error| Error::wrap("decode diff line number", error))?;
    digits
        .parse()
        .map_err(|error: std::num::ParseIntError| Error::wrap("parse diff line number", error))
}

#[derive(Debug, thiserror::Error)]
#[error("diff line number arithmetic overflows")]
pub struct LineOverflow;

pub fn advance_line(line: i64, by: i64) -> Result<i64, Error> {
    line.checked_add(by)
        .ok_or_else(|| Error::cause(LineOverflow))
}

pub fn line_number(line: i128) -> Result<i64, Error> {
    i64::try_from(line).map_err(|error| Error::wrap("advance diff line number", error))
}

/// The file sections of a unified diff. Empty input has none; text without a diff header is an error, so
/// a failure message is never taken for a diff.
pub fn parse_diff(raw: &[u8]) -> Result<Vec<FileDiff>, Error> {
    if raw.is_empty() {
        return Ok(Vec::new());
    }
    if !raw.starts_with(DIFF_HEADER) {
        return Err(Error::new(
            "not a unified diff: it does not start with a diff --git header",
        ));
    }
    let mut files = Vec::new();
    let mut rest = raw;
    while !rest.is_empty() {
        let tail = rest.get(DIFF_HEADER.len()..).unwrap_or_default();
        let end =
            find(tail, b"\ndiff --git ").map_or(rest.len(), |end| DIFF_HEADER.len() + end + 1);
        let (section, next) = rest.split_at(end);
        rest = next;
        files.push(parse_section(section)?);
    }
    Ok(files)
}

fn parse_section(section: &[u8]) -> Result<FileDiff, Error> {
    let hunk_header = pattern(&HUNK_HEADER)?;
    let mut f = FileDiff {
        status: "modified".to_owned(),
        text: section.to_vec(),
        ..FileDiff::default()
    };
    let lines: Vec<&[u8]> = lines(section).collect();
    let header_line = trim_newline(lines.first().copied().unwrap_or_default());
    let (mut old_name, mut new_name) = (Vec::new(), Vec::new());
    let (mut modes, mut payload): (Vec<&[u8]>, Vec<&[u8]>) = (Vec::new(), Vec::new());
    let mut in_payload = false;
    let mut i = 1;
    while i < lines.len() {
        let line = trim_newline(lines[i]);
        if line.starts_with(b"@@ ") {
            break;
        }
        i += 1;
        if in_payload {
            payload.push(line);
            continue;
        }
        if line.starts_with(b"old mode ") || line.starts_with(b"new mode ") {
            modes.push(line);
        } else if line.starts_with(b"new file mode") {
            "added".clone_into(&mut f.status);
            modes.push(line);
        } else if line.starts_with(b"deleted file mode") {
            "deleted".clone_into(&mut f.status);
            modes.push(line);
        } else if let Some(rest) = line.strip_prefix(b"index ") {
            let ids = rest.split(|b| *b == b' ').next().unwrap_or_default();
            f.blob = find(ids, b"..").map_or_else(Vec::new, |at| ids[at + 2..].to_vec());
        } else if let Some(name) = line.strip_prefix(b"rename from ") {
            "renamed".clone_into(&mut f.status);
            f.old_path = lossy(&unquote_name(name));
        } else if let Some(name) = line.strip_prefix(b"rename to ") {
            "renamed".clone_into(&mut f.status);
            f.path = lossy(&unquote_name(name));
        } else if let Some(name) = line.strip_prefix(b"copy from ") {
            "copied".clone_into(&mut f.status);
            f.old_path = lossy(&unquote_name(name));
        } else if let Some(name) = line.strip_prefix(b"copy to ") {
            "copied".clone_into(&mut f.status);
            f.path = lossy(&unquote_name(name));
        } else if let Some(name) = line.strip_prefix(b"--- ") {
            old_name = diff_name(name);
        } else if let Some(name) = line.strip_prefix(b"+++ ") {
            new_name = diff_name(name);
        } else if line == b"GIT binary patch" {
            f.binary = true;
            in_payload = true;
        } else if line.starts_with(b"Binary files ") {
            f.binary = true;
        }
    }
    f.mode = modes.join(&b'\n');
    f.payload = payload.join(&b'\n');
    if f.path.is_empty() {
        f.path = if !new_name.is_empty() {
            lossy(&new_name)
        } else if !old_name.is_empty() {
            lossy(&old_name)
        } else {
            let names = header_line.strip_prefix(DIFF_HEADER).unwrap_or(header_line);
            let Some(path) = header_path(names) else {
                return Err(Error::new(format!(
                    "cannot read the path in {}",
                    quote(&lossy(header_line))
                )));
            };
            lossy(&path)
        };
    }
    if f.status == "deleted" && !old_name.is_empty() {
        f.path = lossy(&old_name);
    }
    f.header = lines[..i].concat();
    let (mut old, mut next) = (0_i128, 0_i128);
    let (mut changed, mut removed, mut hunk_removed) = (Vec::new(), Vec::new(), Vec::new());
    let mut cur: Option<Hunk> = None;
    for line in &lines[i..] {
        if let Some(m) = hunk_header.captures(line) {
            if let Some(mut done) = cur.take() {
                done.removals = spans(&hunk_removed);
                f.hunks.push(done);
            }
            cur = Some(Hunk {
                text: line.to_vec(),
                ..Hunk::default()
            });
            hunk_removed.clear();
            old = i128::from(
                m.get(1)
                    .map(|d| number(d.as_bytes()))
                    .transpose()?
                    .unwrap_or(0),
            );
            next = i128::from(
                m.get(3)
                    .map(|d| number(d.as_bytes()))
                    .transpose()?
                    .unwrap_or(0),
            );
            m.get(2).map(|d| number(d.as_bytes())).transpose()?;
            m.get(4).map(|d| number(d.as_bytes())).transpose()?;
            continue;
        }
        if let Some(cur) = cur.as_mut() {
            cur.text.extend_from_slice(line);
        }
        if let Some(text) = line.strip_prefix(b"+") {
            f.added += 1;
            changed.push(line_number(next)?);
            next += 1;
            if let Some(cur) = cur.as_mut() {
                cur.added.push(trim_newline(text).to_vec());
            }
        } else if line.starts_with(b"-") {
            f.removed += 1;
            removed.push(line_number(old)?);
            hunk_removed.push(line_number(old)?);
            old += 1;
        } else if line.starts_with(b" ") {
            line_number(old)?;
            line_number(next)?;
            old += 1;
            next += 1;
        }
    }
    if let Some(mut done) = cur {
        done.removals = spans(&hunk_removed);
        f.hunks.push(done);
    }
    f.changed = spans(&changed);
    f.removals = spans(&removed);
    Ok(f)
}

/// Ascending line numbers merged into inclusive runs.
pub fn spans(lines: &[i64]) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::new();
    for &n in lines {
        if let Some(last) = out.last_mut()
            && last.last.checked_add(1) == Some(n)
        {
            last.last = n;
            continue;
        }
        out.push(Span { first: n, last: n });
    }
    out
}

fn unquote_name(s: &[u8]) -> Vec<u8> {
    if s.starts_with(b"\"")
        && let Some(name) = unquote(s)
    {
        return name;
    }
    s.to_vec()
}

/// The path of a `---` or `+++` line: its `a/` or `b/` prefix and any tab suffix dropped, and `/dev/null`
/// no name.
fn diff_name(s: &[u8]) -> Vec<u8> {
    let s = s.split(|b| *b == b'\t').next().unwrap_or_default();
    if s == b"/dev/null" {
        return Vec::new();
    }
    let s = unquote_name(s);
    if s.len() > 2 && (s.starts_with(b"a/") || s.starts_with(b"b/")) {
        return s[2..].to_vec();
    }
    s
}

/// The path from `a/P b/P` when both sides name the same file. Git C-quotes a name with control
/// characters, quotes, backslashes or, by default, non-ASCII bytes, and quotes both sides then.
fn header_path(s: &[u8]) -> Option<Vec<u8>> {
    if s.starts_with(b"\"") {
        return quoted_header_path(s);
    }
    if s.len() < 5 || !(s.len() - 5).is_multiple_of(2) {
        return None;
    }
    let n = (s.len() - 5) / 2;
    (s.starts_with(b"a/") && &s[2 + n..5 + n] == b" b/" && s[2..2 + n] == s[5 + n..])
        .then(|| s[2..2 + n].to_vec())
}

fn quoted_header_path(s: &[u8]) -> Option<Vec<u8>> {
    let (a, len) = unquote_prefix(s)?;
    let rest = s[len..].strip_prefix(b" ")?;
    let b = unquote(rest)?;
    (a.len() >= 3
        && b.len() >= 3
        && a.starts_with(b"a/")
        && b.starts_with(b"b/")
        && a[2..] == b[2..])
        .then(|| a[2..].to_vec())
}

impl FileDiff {
    /// The added and removed lines of the section, and the no-newline markers that follow them, ignoring
    /// context and hunk positions.
    pub fn changed_lines(&self) -> Vec<&[u8]> {
        let mut out = Vec::new();
        let (mut in_hunk, mut after_change) = (false, false);
        for line in lines(&self.text) {
            let line = trim_newline(line);
            if line.starts_with(b"@@ ") {
                in_hunk = true;
                after_change = false;
            } else if in_hunk && (line.starts_with(b"+") || line.starts_with(b"-")) {
                out.push(line);
                after_change = true;
            } else if in_hunk && line.starts_with(b"\\") {
                if after_change {
                    out.push(line);
                }
            } else {
                after_change = false;
            }
        }
        out
    }
}

/// Whether two patches change the same files the same way: the same added and removed lines, modes and,
/// for binary files, resulting content, whatever their context lines, hunk positions and messages.
pub fn same_changes(a: &[FileDiff], b: &[FileDiff]) -> bool {
    a.len() == b.len()
        && a.iter().zip(b).all(|(x, y)| {
            x.path == y.path
                && x.old_path == y.old_path
                && x.status == y.status
                && x.binary == y.binary
                && x.mode == y.mode
                && same_binary(x, y)
                && x.changed_lines() == y.changed_lines()
        })
}

/// Whether two binary sections leave the same file: the object their index lines name, or their payloads
/// when an index line is missing. Text sections always match.
fn same_binary(x: &FileDiff, y: &FileDiff) -> bool {
    if !x.binary {
        return true;
    }
    if !x.blob.is_empty() && !y.blob.is_empty() {
        return x.blob == y.blob;
    }
    x.blob == y.blob && x.payload == y.payload
}

/// Why the review skips the file by its path or kind alone, or `""` when the file needs a look at its
/// contents.
pub fn fixed_exclusion(f: &FileDiff) -> &'static str {
    let base = crate::text::base(&f.path);
    if base == "go.sum" || base == "flake.lock" {
        "generated"
    } else if f.path.split('/').any(|segment| segment == "testdata") {
        "test data (`**/testdata/**`)"
    } else if f.binary {
        "binary"
    } else {
        ""
    }
}

fn is_generated_marker(line: &[u8]) -> bool {
    line.starts_with(b"// Code generated ") && line.ends_with(b" DO NOT EDIT.")
}

/// The contents of the file a section adds or deletes whole: one hunk of added lines from line 1, or of
/// removed lines from line 1. Any other section does not show the file, and the result is empty.
pub fn whole_content(f: &FileDiff) -> Vec<u8> {
    let prefix = if f.status == "added"
        && f.removed == 0
        && f.hunks.len() == 1
        && f.changed.len() == 1
        && f.changed[0].first == 1
    {
        b'+'
    } else if f.status == "deleted"
        && f.added == 0
        && f.hunks.len() == 1
        && f.hunks[0].removals.len() == 1
        && f.hunks[0].removals[0].first == 1
    {
        b'-'
    } else {
        return Vec::new();
    };
    lines(&f.hunks[0].text)
        .filter(|line| line.first() == Some(&prefix))
        .flat_map(|line| line[1..].iter().copied())
        .collect()
}

/// Whether a file's contents carry the generated marker the way Go defines it: as a whole line comment
/// before the first source token. Blank space and complete line and block comments, however many lines a
/// block spans, may come first; the marker inside a block comment does not count.
pub fn generated_content(content: &[u8]) -> bool {
    let mut in_block = false;
    for line in lines(content) {
        let end = line
            .iter()
            .rposition(|b| *b != b'\r' && *b != b'\n')
            .map_or(0, |at| at + 1);
        let mut rest = &line[..end];
        if !in_block && is_generated_marker(rest) {
            return true;
        }
        while !rest.is_empty() {
            if in_block {
                let Some(at) = find(rest, b"*/") else {
                    break;
                };
                in_block = false;
                rest = &rest[at + 2..];
                continue;
            }
            let start = rest
                .iter()
                .position(|b| *b != b' ' && *b != b'\t')
                .unwrap_or(rest.len());
            rest = &rest[start..];
            if rest.is_empty() {
            } else if rest.starts_with(b"//") {
                rest = &[];
            } else if let Some(after) = rest.strip_prefix(b"/*") {
                in_block = true;
                rest = after;
            } else {
                return false;
            }
        }
    }
    false
}

/// The sections of several patches as one list, one entry per path in order of first appearance, so
/// every path gets one hunk file.
pub fn merge_by_path(patches: Vec<Vec<FileDiff>>) -> Vec<FileDiff> {
    let mut out: Vec<FileDiff> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for f in patches.into_iter().flatten() {
        let Some(&k) = at.get(&f.path) else {
            at.insert(f.path.clone(), out.len());
            out.push(f);
            continue;
        };
        let m = &mut out[k];
        m.text.extend_from_slice(&f.text);
        m.added += f.added;
        m.removed += f.removed;
        m.binary = m.binary || f.binary;
        m.changed.extend_from_slice(&f.changed);
    }
    out
}

#[cfg(test)]
mod numeric_tests {
    use test_case::test_case;

    use super::*;
    use crate::rangediff::lost_effects;

    #[test_case("0", 0 ; "zero")]
    #[test_case("9223372036854775807", i64::MAX ; "maximum")]
    fn exact_numbers(digits: &str, expected: i64) {
        assert_eq!(number(digits.as_bytes()).unwrap(), expected);
    }

    #[test_case("@@ -9223372036854775808 +1 @@" ; "old start")]
    #[test_case("@@ -1 +9223372036854775808 @@" ; "new start")]
    #[test_case("@@ -1,9223372036854775808 +1 @@" ; "old count")]
    #[test_case("@@ -1 +1,9223372036854775808 @@" ; "new count")]
    fn oversized_hunk_numbers_are_refused(header: &str) {
        let diff = format!("diff --git a/f b/f\n{header}\n");
        let error = parse_diff(diff.as_bytes()).unwrap_err();
        assert!(error.find::<std::num::ParseIntError>().is_some());
        let file = FileDiff {
            hunks: vec![Hunk {
                text: format!("{header}\n").into_bytes(),
                ..Hunk::default()
            }],
            ..FileDiff::default()
        };
        let error = lost_effects(&file, &[], &[]).unwrap_err();
        assert!(error.find::<std::num::ParseIntError>().is_some());
    }

    #[test_case("+x\n", true ; "added")]
    #[test_case("-x\n", false ; "removed")]
    fn last_representable_line_is_exact(body: &str, added: bool) {
        let diff = format!("diff --git a/f b/f\n@@ -{0} +{0} @@\n{body}", i64::MAX);
        let files = parse_diff(diff.as_bytes()).unwrap();
        let expected = vec![Span {
            first: i64::MAX,
            last: i64::MAX,
        }];
        assert_eq!(
            if added {
                &files[0].changed
            } else {
                &files[0].removals
            },
            &expected
        );
        assert_eq!(
            spans(&[i64::MAX - 1, i64::MAX]),
            vec![Span {
                first: i64::MAX - 1,
                last: i64::MAX
            }]
        );
        let diff = format!("{diff}{body}");
        let error = parse_diff(diff.as_bytes()).unwrap_err();
        assert!(error.find::<std::num::TryFromIntError>().is_some());
    }

    #[test_case(i64::MAX, 1 ; "positive")]
    #[test_case(i64::MIN, -1 ; "negative")]
    fn line_arithmetic_refuses_overflow(line: i64, by: i64) {
        assert!(
            advance_line(line, by)
                .unwrap_err()
                .find::<LineOverflow>()
                .is_some()
        );
    }

    #[test]
    fn range_diff_propagates_shift_overflow() {
        let diff = format!(
            "diff --git a/f b/f\n@@ -0,0 +1 @@\n+x\n@@ -{0} +{0} @@\n-y\n",
            i64::MAX
        );
        let reversal = parse_diff(diff.as_bytes()).unwrap();
        let error = lost_effects(&reversal[0], &reversal, &reversal).unwrap_err();
        assert!(error.find::<LineOverflow>().is_some());
    }
}
