//! Changelog fragments: one file per pull request under `changes/` (ADR 0033).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::{fs, io};

use regex::Regex;

use crate::pattern;

/// The directory of pending fragments, relative to the repository root.
pub const FRAGMENT_DIR: &str = "changes";
/// The one file in [`FRAGMENT_DIR`] that is not a fragment.
pub const FRAGMENT_README: &str = "README.md";
/// The sections a fragment may have, in the order the release writes them.
pub const SECTIONS: [&str; 4] = ["Added", "Changed", "Removed", "Fixed"];

static PULL_REQUEST_LINK: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"(?i)\[#[0-9]+\]|https?://(?:www\.)?github\.com/[a-z0-9_.-]+/[a-z0-9_.-]+/pull/[0-9]+(?-u:\b)",
    )
});

/// One `- ` line of a fragment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Entry {
    /// The pull request the fragment is named by.
    pub pr: u64,
    pub section: &'static str,
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
        section: &'static str,
    },
    #[error("{name}:{line}: section {section:?} is not one of Added, Changed, Removed, Fixed")]
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
    let mut section: Option<(&'static str, usize)> = None;
    let close = |section: Option<(&'static str, usize)>, line: usize| match section {
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
            let known = SECTIONS
                .iter()
                .find(|known| **known == heading)
                .ok_or_else(|| FragmentError::UnknownSection {
                    name: name.to_owned(),
                    line: n,
                    section: heading.to_owned(),
                })?;
            section = Some((*known, 0));
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
            if PULL_REQUEST_LINK.is_match(text) {
                return Err(FragmentError::PullRequestLink {
                    name: name.to_owned(),
                    line: n,
                    pr,
                });
            }
            entries.push(Entry {
                pr,
                section: heading,
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
    let mut files = BTreeMap::new();
    for entry in list {
        let entry = entry.map_err(|source| CheckError::List {
            dir: dir.to_owned(),
            source,
        })?;
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
/// distinct. Sections follow [`SECTIONS`]; in each, breaking entries come first, then by pull request number,
/// each linked as ` ([#N])` before its final period.
pub fn assemble(entries: &[Entry]) -> (String, Vec<u64>) {
    let mut body = String::new();
    for section in SECTIONS {
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
            let _ = writeln!(body, "- {text} ([#{}]).", e.pr);
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

#[cfg(test)]
#[path = "tests/fragments.rs"]
mod tests;
