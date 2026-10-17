//! `CHANGELOG.md`: its dated sections, the version bump its entries call for, the rewrite that adds a release
//! and the release notes of one section.

use crate::{Pattern, PatternError, compiled, pattern};
use jiff::Timestamp;
use jiff::tz::TimeZone;
use std::collections::HashSet;

static SECTION_HEADING: Pattern =
    Pattern::new(|| pattern(r"(?m)^## \[([^\]\n]+)\](?: - ([0-9]{4}-[0-9]{2}-[0-9]{2}))?[ \t]*$"));
static VERSION: Pattern =
    Pattern::new(|| pattern(r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$"));
static REFERENCE: Pattern = Pattern::new(|| pattern(r"\[([^\]\n]+)\](?:\[([^\]\n]*)\])?"));
static DEFINITION: Pattern = Pattern::new(|| pattern(r"(?m)^\[#([0-9]+)\]: .+$"));
static LINK_DEFINITION: Pattern = Pattern::new(|| pattern(r"(?m)^\[[^\]\n]+\]: [^\t\n\f\r ].*$"));

/// Whether `version` is `MAJOR.MINOR.PATCH` without leading zeros.
pub fn is_version(version: &str) -> Result<bool, PatternError> {
    Ok(compiled(&VERSION)?.is_match(version))
}

/// One `## [name] - date` section; `body` runs to the next heading or, for the last section, to the first
/// link definition after it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Section<'a> {
    pub name: &'a str,
    /// The release date, absent for an undated section.
    pub date: Option<&'a str>,
    pub body: &'a str,
}

fn sections(changelog: &str) -> Result<Vec<Section<'_>>, PatternError> {
    let headings: Vec<_> = compiled(&SECTION_HEADING)?
        .captures_iter(changelog)
        .collect();
    let mut result = Vec::with_capacity(headings.len());
    for (i, heading) in headings.iter().enumerate() {
        let Some(whole) = heading.get(0) else {
            continue;
        };
        let start = whole.end();
        let mut end = changelog.len();
        if let Some(next) = headings.get(i + 1).and_then(|next| next.get(0)) {
            end = next.start();
        } else if let Some(at) = compiled(&LINK_DEFINITION)?.find(&changelog[start..end]) {
            end = start + at.start();
        }
        result.push(Section {
            name: heading.get(1).map_or("", |m| m.as_str()),
            date: heading.get(2).map(|m| m.as_str()),
            body: &changelog[start..end],
        });
    }
    Ok(result)
}

/// The section named `name`, the first if several are.
pub fn section_named<'a>(
    changelog: &'a str,
    name: &str,
) -> Result<Option<Section<'a>>, PatternError> {
    Ok(sections(changelog)?.into_iter().find(|s| s.name == name))
}

/// The `- ` lines of `body`.
pub fn entries(body: &str) -> Vec<&str> {
    body.split('\n')
        .filter(|line| line.starts_with("- "))
        .collect()
}

/// Why the next version could not be computed.
#[derive(Debug, thiserror::Error)]
pub enum BumpError {
    #[error("invalid semantic version {0:?}")]
    Invalid(String),
    #[error("parse version {version:?}")]
    Parse {
        version: String,
        #[source]
        source: std::num::ParseIntError,
    },
    #[error("version {0:?} has no next version")]
    Overflow(String),
    #[error(transparent)]
    Pattern(#[from] PatternError),
}

/// The version after `version` that the release section `body` calls for, and the reason for the bump. Before
/// 1.0 a breaking entry bumps MINOR and anything else PATCH; from 1.0 a breaking entry bumps MAJOR, an
/// `### Added` entry MINOR and anything else PATCH.
pub fn bump(version: &str, body: &str) -> Result<(String, String), BumpError> {
    let captures = compiled(&VERSION)?
        .captures(version)
        .ok_or_else(|| BumpError::Invalid(version.to_owned()))?;
    let mut numbers = [0_u64; 3];
    for (i, number) in numbers.iter_mut().enumerate() {
        *number = captures
            .get(i + 1)
            .map_or("", |m| m.as_str())
            .parse()
            .map_err(|source| BumpError::Parse {
                version: version.to_owned(),
                source,
            })?;
    }
    let overflow = || BumpError::Overflow(version.to_owned());
    let [major, minor, patch] = numbers;
    let breaking: Vec<&str> = entries(body)
        .into_iter()
        .filter(|line| line.starts_with("- **BREAKING**"))
        .collect();
    if !breaking.is_empty() {
        let next = if major == 0 {
            [0, minor.checked_add(1).ok_or_else(overflow)?, 0]
        } else {
            [major.checked_add(1).ok_or_else(overflow)?, 0, 0]
        };
        return Ok((
            render(next),
            format!("Breaking changes:\n{}", breaking.join("\n")),
        ));
    }
    if major > 0 && added_entries(body) {
        let next = [major, minor.checked_add(1).ok_or_else(overflow)?, 0];
        return Ok((
            render(next),
            "Added entries since the previous release (minor bump).".to_owned(),
        ));
    }
    let next = [major, minor, patch.checked_add(1).ok_or_else(overflow)?];
    Ok((render(next), "No breaking changes (patch bump).".to_owned()))
}

fn render([major, minor, patch]: [u64; 3]) -> String {
    format!("{major}.{minor}.{patch}")
}

fn added_entries(body: &str) -> bool {
    let mut in_added = false;
    for line in body.split('\n') {
        if line.starts_with("### ") {
            in_added = line == "### Added";
        } else if in_added && line.starts_with("- ") {
            return true;
        }
    }
    false
}

/// The release notes of `section`: its trimmed body and, in the changelog's order, the link definitions its
/// references use, each once.
pub fn release_notes(changelog: &str, section: Section<'_>) -> Result<String, PatternError> {
    let body = section.body.trim();
    let mut used = HashSet::new();
    for reference in compiled(&REFERENCE)?.captures_iter(body) {
        let (Some(whole), Some(text)) = (reference.get(0), reference.get(1)) else {
            continue;
        };
        let label = reference.get(2);
        if label.is_none() && body.as_bytes().get(whole.end()) == Some(&b'(') {
            continue;
        }
        let label = label.filter(|m| !m.is_empty()).unwrap_or(text);
        used.insert(normalize(label.as_str()));
    }
    let mut notes = body.to_owned();
    for definition in compiled(&LINK_DEFINITION)?.find_iter(changelog) {
        let line = definition.as_str();
        let label = line[1..].split(']').next().unwrap_or("");
        if used.remove(&normalize(label)) {
            notes.push_str("\n\n");
            notes.push_str(line);
        }
    }
    Ok(notes)
}

fn normalize(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Why the changelog could not take the release.
#[derive(Debug, thiserror::Error)]
pub enum RewriteError {
    #[error("version [{0}] already exists")]
    Exists(String),
    #[error(transparent)]
    Pattern(#[from] PatternError),
}

/// `changelog` with a new section `## [version] - date` holding `body` before the newest section, dated
/// `today` in UTC, a `[#N]:` definition for each pull request in `prs` among the others in numeric order, and
/// the version's link definition before the first pull request definition. `web_url` is the repository's
/// web address.
pub fn rewrite_changelog(
    changelog: &str,
    version: &str,
    body: &str,
    prs: &[u64],
    web_url: &str,
    today: Timestamp,
) -> Result<String, RewriteError> {
    if section_named(changelog, version)?.is_some() {
        return Err(RewriteError::Exists(version.to_owned()));
    }
    let web_url = web_url.trim_end_matches('/');
    let date = today.to_zoned(TimeZone::UTC).date();
    let released = format!("## [{version}] - {date}\n\n{}\n\n", body.trim());
    let mut changelog = changelog.to_owned();
    let at = if let Some(m) = compiled(&SECTION_HEADING)?.find(&changelog) {
        m.start()
    } else if let Some(m) = compiled(&LINK_DEFINITION)?.find(&changelog) {
        m.start()
    } else {
        changelog.truncate(changelog.trim_end_matches('\n').len());
        changelog.push_str("\n\n");
        changelog.len()
    };
    let mut updated = format!("{}{released}{}", &changelog[..at], &changelog[at..]);
    for pr in prs {
        updated = add_definition(&updated, *pr, &format!("[#{pr}]: {web_url}/pull/{pr}"))?;
    }
    let link = format!("[{version}]: {web_url}/releases/tag/v{version}\n");
    Ok(match compiled(&DEFINITION)?.find(&updated) {
        Some(at) => format!(
            "{}{link}\n{}",
            &updated[..at.start()],
            &updated[at.start()..]
        ),
        None => format!("{}\n\n{link}", updated.trim_end_matches('\n')),
    })
}

fn add_definition(changelog: &str, pr: u64, line: &str) -> Result<String, PatternError> {
    let mut last_end = None;
    for definition in compiled(&DEFINITION)?.captures_iter(changelog) {
        let (Some(whole), Some(number)) = (definition.get(0), definition.get(1)) else {
            continue;
        };
        last_end = Some(whole.end());
        match number.as_str().parse::<u64>() {
            Ok(n) if n == pr => return Ok(changelog.to_owned()),
            Ok(n) if n > pr => {
                return Ok(format!(
                    "{}{line}\n{}",
                    &changelog[..whole.start()],
                    &changelog[whole.start()..]
                ));
            }
            Ok(_) | Err(_) => {}
        }
    }
    Ok(match last_end {
        Some(end) => format!("{}\n{line}{}", &changelog[..end], &changelog[end..]),
        None => format!("{}\n\n{line}\n", changelog.trim_end_matches('\n')),
    })
}

#[cfg(test)]
#[path = "tests/changelog.rs"]
mod tests;
