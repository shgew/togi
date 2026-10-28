//! Changelog fragments: one file per pull request under `changes/` (ADR 0033).

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::{fmt, fs, io};

use crate::{Pattern, PatternError, compiled, pattern};

/// The directory of pending fragments, relative to the repository root.
pub const FRAGMENT_DIR: &str = "changes";
/// The one file in [`FRAGMENT_DIR`] that is not a fragment.
pub const FRAGMENT_README: &str = "README.md";

/// A section a fragment may have.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Section {
    Added,
    Changed,
    Removed,
    Fixed,
}

impl Section {
    /// Every section, in the order the release writes them.
    pub const ALL: [Self; 4] = [Self::Added, Self::Changed, Self::Removed, Self::Fixed];

    /// The text after `### ` in the section's heading.
    pub const fn heading(self) -> &'static str {
        match self {
            Self::Added => "Added",
            Self::Changed => "Changed",
            Self::Removed => "Removed",
            Self::Fixed => "Fixed",
        }
    }
}

impl fmt::Display for Section {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.heading())
    }
}

/// [`Section::ALL`] as a list of headings, as in `Added, Changed, Removed, Fixed`.
fn section_list() -> String {
    Section::ALL.map(Section::heading).join(", ")
}

static PULL_REQUEST_LINK: Pattern = Pattern::new(|| {
    pattern(
        r"(?i)\[#[0-9]+\]|https?://(?:www\.)?github\.com/[a-z0-9_.-]+/[a-z0-9_.-]+/pull/[0-9]+(?-u:\b)",
    )
});

/// One `- ` line of a fragment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    /// The pull request the fragment is named by.
    pub pr: u64,
    pub section: Section,
    /// The entry without its `- ` marker, ending with a period.
    pub text: String,
}

impl Entry {
    fn breaking(&self) -> bool {
        self.text.starts_with("**BREAKING**")
    }
}

/// Why a fragment's name or content breaks the format in `changes/README.md`.
#[derive(Debug, thiserror::Error)]
pub enum FragmentError {
    #[error("{name}: name must be the pull request number, as in 123.md")]
    Name { name: String },
    #[error("{name}: parse pull request number")]
    Number {
        name: String,
        #[source]
        source: std::num::ParseIntError,
    },
    #[error("{name}:{line}: ### {section} has no entries")]
    EmptySection {
        name: String,
        line: usize,
        section: Section,
    },
    #[error("{name}:{line}: section {section:?} is not one of {}", section_list())]
    UnknownSection {
        name: String,
        line: usize,
        section: String,
    },
    #[error("{name}:{line}: entry before any ### section heading")]
    EntryBeforeSection { name: String, line: usize },
    #[error("{name}:{line}: entry must end with a period")]
    NoPeriod { name: String, line: usize },
    #[error("{name}:{line}: entry must not link a pull request; the release adds ([#{pr}])")]
    PullRequestLink { name: String, line: usize, pr: u64 },
    #[error("{name}:{line}: expected a ### section heading or a \"- \" entry on one line")]
    Line { name: String, line: usize },
    #[error("{name}: no entries")]
    Empty { name: String },
    #[error(transparent)]
    Pattern(#[from] PatternError),
}

/// Parses the fragment `name` holding `content` into its entries, in file order.
pub fn parse_fragment(name: &str, content: &str) -> Result<Vec<Entry>, FragmentError> {
    let digits = name
        .strip_suffix(".md")
        .filter(|digits| {
            digits.bytes().all(|b| b.is_ascii_digit())
                && digits.bytes().next().is_some_and(|b| b != b'0')
        })
        .ok_or_else(|| FragmentError::Name {
            name: name.to_owned(),
        })?;
    let pr = digits.parse().map_err(|source| FragmentError::Number {
        name: name.to_owned(),
        source,
    })?;
    let mut entries = Vec::new();
    let mut section: Option<(Section, usize)> = None;
    let close = |section: Option<(Section, usize)>, line: usize| match section {
        Some((section, 0)) => Err(FragmentError::EmptySection {
            name: name.to_owned(),
            line,
            section,
        }),
        _ => Ok(()),
    };
    let lines: Vec<&str> = content.split('\n').collect();
    for (index, line) in lines.iter().enumerate() {
        let n = index + 1;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(heading) = line.strip_prefix("### ") {
            close(section, n)?;
            let known = Section::ALL
                .into_iter()
                .find(|known| known.heading() == heading)
                .ok_or_else(|| FragmentError::UnknownSection {
                    name: name.to_owned(),
                    line: n,
                    section: heading.to_owned(),
                })?;
            section = Some((known, 0));
        } else if let Some(text) = line.strip_prefix("- ") {
            let text = text.trim();
            let Some((heading, count)) = section.as_mut() else {
                return Err(FragmentError::EntryBeforeSection {
                    name: name.to_owned(),
                    line: n,
                });
            };
            if !text.ends_with('.') || text == "." {
                return Err(FragmentError::NoPeriod {
                    name: name.to_owned(),
                    line: n,
                });
            }
            if compiled(&PULL_REQUEST_LINK)?.is_match(text) {
                return Err(FragmentError::PullRequestLink {
                    name: name.to_owned(),
                    line: n,
                    pr,
                });
            }
            entries.push(Entry {
                pr,
                section: *heading,
                text: text.to_owned(),
            });
            *count += 1;
        } else {
            return Err(FragmentError::Line {
                name: name.to_owned(),
                line: n,
            });
        }
    }
    close(section, lines.len())?;
    if entries.is_empty() {
        return Err(FragmentError::Empty {
            name: name.to_owned(),
        });
    }
    Ok(entries)
}

/// Parses every fragment of `files`, a directory listing of name to content, in name order; `README.md` is
/// not a fragment.
pub fn parse_fragments(files: &BTreeMap<String, String>) -> Result<Vec<Entry>, FragmentError> {
    let mut entries = Vec::new();
    for (name, content) in files {
        if name != FRAGMENT_README {
            entries.extend(parse_fragment(name, content)?);
        }
    }
    Ok(entries)
}

/// Why `-check` refused a fragment directory.
#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error("list {}", dir.display())]
    List {
        dir: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{name}: unexpected directory")]
    Directory { name: String },
    #[error("read {name}")]
    Read {
        name: String,
        #[source]
        source: io::Error,
    },
    #[error("check {}", dir.display())]
    Invalid {
        dir: PathBuf,
        #[source]
        source: FragmentError,
    },
}

/// Validates the fragments in `dir`, the `changes` flake check; a missing directory has none.
pub fn check_fragments(dir: &Path) -> Result<(), CheckError> {
    let files = read_fragment_dir(dir)?;
    parse_fragments(&files).map_err(|source| CheckError::Invalid {
        dir: dir.to_owned(),
        source,
    })?;
    Ok(())
}

fn read_fragment_dir(dir: &Path) -> Result<BTreeMap<String, String>, CheckError> {
    let list = match fs::read_dir(dir) {
        Ok(list) => list,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(source) => {
            return Err(CheckError::List {
                dir: dir.to_owned(),
                source,
            });
        }
    };
    let mut entries = Vec::new();
    for entry in list {
        entries.push(entry.map_err(|source| CheckError::List {
            dir: dir.to_owned(),
            source,
        })?);
    }
    // Directory order is arbitrary; refuse the first bad entry by name.
    entries.sort_by_key(fs::DirEntry::file_name);
    let mut files = BTreeMap::new();
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = entry.file_type().map_err(|source| CheckError::Read {
            name: name.clone(),
            source,
        })?;
        if kind.is_dir() {
            return Err(CheckError::Directory { name });
        }
        match fs::read_to_string(entry.path()) {
            Ok(content) => files.insert(name, content),
            Err(source) => return Err(CheckError::Read { name, source }),
        };
    }
    Ok(files)
}

/// The release section the entries make, without its heading, and the pull requests it links, ascending and
/// distinct. Sections follow [`Section::ALL`]; in each, breaking entries come first, then by pull request number,
/// each linked as ` ([#N])` before its final period, with its `../` links rewritten for `CHANGELOG.md`.
pub fn assemble(entries: &[Entry]) -> (String, Vec<u64>) {
    let mut body = String::new();
    for section in Section::ALL {
        let mut lines: Vec<&Entry> = entries.iter().filter(|e| e.section == section).collect();
        if lines.is_empty() {
            continue;
        }
        lines.sort_by_key(|e| (!e.breaking(), e.pr));
        if !body.is_empty() {
            body.push('\n');
        }
        let _ = write!(body, "### {section}\n\n");
        for e in lines {
            let text = e.text.strip_suffix('.').unwrap_or(&e.text);
            let _ = writeln!(body, "- {} ([#{}]).", relocate_links(text), e.pr);
        }
    }
    if body.ends_with('\n') {
        body.pop();
    }
    let mut prs: Vec<u64> = entries.iter().map(|e| e.pr).collect();
    prs.sort_unstable();
    prs.dedup();
    (body, prs)
}

/// `text` with its links relative to `CHANGELOG.md` at the repository root instead of to [`FRAGMENT_DIR`], where
/// the fragment was written: each inline link or image destination starting with `../`, after optional spaces
/// and an optional `<`, loses that step. A link needs an unescaped `[` before its `](`; code spans and
/// backslash-escaped characters are text, not links, and stay as written, as do other links.
fn relocate_links(text: &str) -> Cow<'_, str> {
    let bytes = text.as_bytes();
    let mut out: Option<String> = None;
    // `text[..copied]` is in `out`, rewritten.
    let mut copied = 0;
    // Unescaped `[` not yet closed by a `]`.
    let mut openers = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'`' => i = code_span_end(bytes, i),
            b'[' => {
                openers += 1;
                i += 1;
            }
            b']' if openers == 0 => i += 1,
            b']' if bytes.get(i + 1) != Some(&b'(') => {
                openers -= 1;
                i += 1;
            }
            b']' => {
                openers -= 1;
                let mut start = i + 2;
                while matches!(bytes.get(start), Some(b' ' | b'\t')) {
                    start += 1;
                }
                if bytes.get(start) == Some(&b'<') {
                    start += 1;
                }
                if bytes[start..].starts_with(b"../") {
                    let out = out.get_or_insert_with(|| String::with_capacity(text.len()));
                    out.push_str(&text[copied..start]);
                    copied = start + 3;
                }
                i = start;
            }
            _ => i += 1,
        }
    }
    match out {
        Some(mut out) => {
            out.push_str(&text[copied..]);
            Cow::Owned(out)
        }
        None => Cow::Borrowed(text),
    }
}

/// Where the backtick run at `start` stops counting as text: after the code span it opens, closed by a run of
/// exactly as many backticks, or, when nothing closes it, after the run itself, which is then literal.
fn code_span_end(bytes: &[u8], start: usize) -> usize {
    let run_at = |at: usize| bytes[at..].iter().take_while(|&&b| b == b'`').count();
    let open = run_at(start);
    let mut at = start + open;
    while at < bytes.len() {
        if bytes[at] == b'`' {
            let close = run_at(at);
            if close == open {
                return at + close;
            }
            at += close;
        } else {
            at += 1;
        }
    }
    start + open
}

#[cfg(test)]
#[path = "tests/fragments.rs"]
mod tests;
