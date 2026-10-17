//! Records over GitHub's comment limit, posted as several comments, the record's parts, and joined back
//! into one record when read (docs/review-record.md).

use std::collections::BTreeMap;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::record::{RECORD_CLOSE, RECORD_OPEN, RecordError};
use crate::text::{json, nullable};
use crate::{Error, Programs, ROBOTOGI_BOT_LOGIN};

/// The longest comment body GitHub accepts, in characters.
pub const COMMENT_LIMIT: usize = 65_536;
pub const PART_OPEN: &str = "<!-- togi-review-part ";

/// The time an absent `createdAt` reads as: midnight UTC on 1 January of year 1.
pub const ZERO_TIME: Timestamp = Timestamp::constant(-62_135_596_800, 0);

/// A comment on a pull request.
#[derive(Clone, Debug)]
pub struct Comment {
    pub url: String,
    pub created: Timestamp,
    pub body: String,
}

/// The hidden block that ends each part: which part of whose record it is, and its slice of the record's
/// JSON block.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    head_sha: String,
    part: usize,
    parts: usize,
    data: String,
}

/// One part of a split record, read from its comment.
#[derive(Debug, PartialEq, Eq)]
pub struct Part {
    pub head_sha: String,
    pub part: usize,
    pub parts: usize,
    /// The part's slice of the record's Markdown before the JSON block.
    text: String,
    /// The part's slice of the JSON block.
    data: String,
}

fn note(part: usize, parts: usize) -> String {
    format!("<sub>Review record, part {part} of {parts}.</sub>\n")
}

fn part_body(text: &str, header: &Header) -> Result<String, Error> {
    let block = json(header).map_err(|error| Error::wrap("encode a record part", error))?;
    let gap = if text.is_empty() { "" } else { "\n" };
    Ok(format!(
        "{text}{gap}{}{PART_OPEN}{block}{RECORD_CLOSE}",
        note(header.part, header.parts)
    ))
}

/// The characters `c` takes once `json` has written it inside a string, or more.
fn escaped_len(c: char) -> usize {
    match c {
        '"' | '\\' => 2,
        '<' | '>' | '&' | '\u{2028}' | '\u{2029}' => 6,
        c if c < ' ' => 6,
        _ => 1,
    }
}

/// The comment bodies that post the rendered record `md` of `head_sha`: `md` itself when it fits in one
/// comment of `limit` characters, else its parts. Each part carries whole lines of the Markdown while they
/// fit, then a slice of the JSON block; joining them gives `md` back.
pub fn split_record(md: &str, head_sha: &str, limit: usize) -> Result<Vec<String>, Error> {
    if md.chars().count() <= limit {
        return Ok(vec![md.to_owned()]);
    }
    let Some(start) = md.rfind(RECORD_OPEN) else {
        return Err(Error::new("the record has no togi-review block"));
    };
    let mut visible = &md[..start];
    let after = &md[start + RECORD_OPEN.len()..];
    let Some(end) = after.find(RECORD_CLOSE) else {
        return Err(Error::new("the record's togi-review block is unterminated"));
    };
    let mut data = &after[..end];
    let widest = Header {
        head_sha: head_sha.to_owned(),
        part: usize::MAX,
        parts: usize::MAX,
        data: String::new(),
    };
    let overhead = part_body("\n", &widest)?.chars().count();
    let Some(budget) = limit.checked_sub(overhead).filter(|b| *b >= 6) else {
        return Err(Error::new(format!(
            "a comment limit of {limit} characters leaves no room for a record part"
        )));
    };
    let mut slices = Vec::new();
    while !visible.is_empty() || !data.is_empty() {
        let mut room = budget;
        let mut text = String::new();
        while !visible.is_empty() {
            let line_end = visible.find('\n').map_or(visible.len(), |i| i + 1);
            let len = visible[..line_end].chars().count();
            if len <= room {
                text.push_str(&visible[..line_end]);
                visible = &visible[line_end..];
                room -= len;
                continue;
            }
            if text.is_empty() {
                let cut = visible
                    .char_indices()
                    .nth(room)
                    .map_or(visible.len(), |(i, _)| i);
                text.push_str(&visible[..cut]);
                visible = &visible[cut..];
                room = 0;
            }
            break;
        }
        let mut cut = 0;
        if visible.is_empty() {
            for (i, c) in data.char_indices() {
                let cost = escaped_len(c);
                if cost > room {
                    break;
                }
                room -= cost;
                cut = i + c.len_utf8();
            }
        }
        slices.push((text, data[..cut].to_owned()));
        data = &data[cut..];
    }
    let parts = slices.len();
    slices
        .into_iter()
        .enumerate()
        .map(|(i, (text, data))| {
            part_body(
                &text,
                &Header {
                    head_sha: head_sha.to_owned(),
                    part: i + 1,
                    parts,
                    data,
                },
            )
        })
        .collect()
}

/// The part `body` holds, `None` when it is not a part.
pub fn parse_part(body: &str) -> Option<Result<Part, RecordError>> {
    let body = body.replace("\r\n", "\n");
    let start = body.rfind(PART_OPEN)?;
    let after = &body[start + PART_OPEN.len()..];
    let Some(end) = after.find(RECORD_CLOSE) else {
        return Some(Err(RecordError::Unterminated));
    };
    let header: Header = match serde_json::from_str(&after[..end]) {
        Ok(header) => header,
        Err(error) => return Some(Err(RecordError::Decode(error))),
    };
    if header.head_sha.is_empty() || header.parts < 2 || !(1..=header.parts).contains(&header.part)
    {
        return Some(Err(RecordError::Part(format!(
            "part {} of {} on {:?}",
            header.part, header.parts, header.head_sha
        ))));
    }
    let Some(rest) = body[..start].strip_suffix(&note(header.part, header.parts)) else {
        return Some(Err(RecordError::Part(format!(
            "part {} of {} lacks its note",
            header.part, header.parts
        ))));
    };
    Some(Ok(Part {
        text: rest.strip_suffix('\n').unwrap_or(rest).to_owned(),
        head_sha: header.head_sha,
        part: header.part,
        parts: header.parts,
        data: header.data,
    }))
}

/// The record that `parts`, all of one record in order, make together.
fn join_parts<'a>(parts: impl Iterator<Item = &'a Part> + Clone) -> String {
    let text: String = parts.clone().map(|p| p.text.as_str()).collect();
    let data: String = parts.map(|p| p.data.as_str()).collect();
    format!("{text}{RECORD_OPEN}{data}{RECORD_CLOSE}\n")
}

/// The records among `comments`: a comment that is not a part stands as it is, first, in order; the parts
/// of a split record join into one record at its first part's URL, created when its last part was, after
/// them. A split record missing a part is left out: publish posts no check until every part is there.
pub fn join_records(comments: Vec<Comment>) -> Result<Vec<Comment>, Error> {
    let mut records = Vec::new();
    let mut split: BTreeMap<(String, usize), BTreeMap<usize, (Comment, Part)>> = BTreeMap::new();
    for comment in comments {
        let part = match parse_part(&comment.body) {
            None => {
                records.push(comment);
                continue;
            }
            Some(Ok(part)) => part,
            Some(Err(error)) => {
                return Err(Error::wrap(
                    format!("parse record part {}", comment.url),
                    error,
                ));
            }
        };
        split
            .entry((part.head_sha.clone(), part.parts))
            .or_default()
            .insert(part.part, (comment, part));
    }
    for ((_, parts), found) in split {
        if found.len() != parts {
            continue;
        }
        let body = join_parts(found.values().map(|(_, p)| p));
        let created = found
            .values()
            .map(|(c, _)| c.created)
            .max()
            .unwrap_or(ZERO_TIME);
        let Some((_, (first, _))) = found.into_iter().next() else {
            continue;
        };
        records.push(Comment {
            url: first.url,
            created,
            body,
        });
    }
    Ok(records)
}

#[derive(Default, Deserialize)]
struct CommentNode {
    #[serde(default, deserialize_with = "nullable")]
    html_url: String,
    #[serde(default)]
    created_at: Option<Timestamp>,
    #[serde(default, deserialize_with = "nullable")]
    body: String,
    #[serde(default, deserialize_with = "nullable")]
    user: User,
}

#[derive(Default, Deserialize)]
struct User {
    #[serde(default, deserialize_with = "nullable")]
    login: String,
}

/// robotogi's comments on pull request `pr` of `repo`, in order, each as posted.
pub fn robotogi_comments(
    programs: &mut dyn Programs,
    repo: &str,
    pr: i64,
) -> Result<Vec<Comment>, Error> {
    let out = programs
        .gh(&[
            "api",
            "--paginate",
            "--slurp",
            &format!("repos/{repo}/issues/{pr}/comments"),
        ])
        .map_err(|error| Error::wrap(format!("list comments of pull request #{pr}"), error))?;
    let pages: Vec<Option<Vec<CommentNode>>> = serde_json::from_slice(&out)
        .map_err(|error| Error::wrap(format!("decode comments of pull request #{pr}"), error))?;
    Ok(pages
        .into_iter()
        .flatten()
        .flatten()
        .filter(|c| c.user.login == ROBOTOGI_BOT_LOGIN)
        .map(|c| Comment {
            url: c.html_url,
            created: c.created_at.unwrap_or(ZERO_TIME),
            body: c.body,
        })
        .collect())
}

#[cfg(test)]
#[path = "tests/parts.rs"]
mod tests;
