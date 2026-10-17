//! `publish`: posts the record and the `review` check as robotogi.

use serde::{Deserialize, Serialize};

use crate::manifest::{Manifest, read_manifest};
use crate::record::parse_record;
use crate::render::RECORD_FILE;
use crate::snapshot::view_pull;
use crate::text::{join, json, nullable, quote};
use crate::{Error, ROBOTOGI_BOT_LOGIN, ROBOTOGI_LOGIN, Tools, files};

pub const PUBLISH_USAGE: &str = "usage: just as-bot just reviews publish <snapshot directory>";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckPayload {
    pub name: String,
    pub head_sha: String,
    pub status: String,
    pub conclusion: String,
    pub output: CheckOutput,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckOutput {
    #[serde(default, deserialize_with = "nullable")]
    pub title: String,
    #[serde(default, deserialize_with = "nullable")]
    pub summary: String,
}

pub fn run_publish(t: &mut Tools<'_>, args: &[String]) -> Result<(), Error> {
    let [dir] = args else {
        return Err(Error::new(PUBLISH_USAGE));
    };
    let m = read_manifest(dir)?;
    if t.gh_token.is_empty() {
        return Err(Error::new(
            "GH_TOKEN is not set: run publish under `just as-bot`, so one robotogi App token posts both the record and the check",
        ));
    }
    require_head(t, &m)?;
    let mut check = CheckPayload {
        name: "review".to_owned(),
        head_sha: m.head_sha.clone(),
        status: "completed".to_owned(),
        conclusion: "success".to_owned(),
        output: CheckOutput::default(),
    };
    let record_url = if m.carry_forward {
        let Some(previous) = m.previous.as_ref().filter(|p| p.verdict == "success") else {
            return Err(Error::new(
                "the snapshot carries forward a previous record that is not recorded as a success: render and publish a new record",
            ));
        };
        check.output = CheckOutput {
            title: "Review carried forward".to_owned(),
            summary: carry_summary(&m),
        };
        previous.url.clone()
    } else {
        let record_path = join(dir, RECORD_FILE);
        let md = files::read(&record_path)
            .map_err(|error| Error::wrap(format!("read {RECORD_FILE}: run render first"), error))?;
        let md = String::from_utf8_lossy(&md).into_owned();
        let record =
            parse_record(&md).map_err(|error| Error::wrap(format!("read {RECORD_FILE}"), error))?;
        if record.head_sha != m.head_sha {
            return Err(Error::new(format!(
                "{RECORD_FILE} reviews {}, not the snapshot's head {}: run render again",
                record.head_sha, m.head_sha
            )));
        }
        let existing = existing_record(t, &m)?;
        let mut record_url = existing.url.clone();
        if !existing.url.is_empty() && normalize_record(&existing.body) != normalize_record(&md) {
            return Err(Error::new(format!(
                "robotogi's record {} on {} differs from {RECORD_FILE}: it is not updated and no check is posted; post the changed review on a new head",
                existing.url, m.head_sha
            )));
        }
        if record_url.is_empty() {
            record_url = post_record(t, &m, &record_path)?;
        }
        if record.verdict != "success" {
            return write!(
                t.stdout,
                "record {record_url}\nthe verdict is {}: no review check posted\n",
                record.verdict
            )
            .map_err(Error::cause);
        }
        check.output = CheckOutput {
            title: "Review recorded".to_owned(),
            summary: format!("Review record: {record_url}"),
        };
        record_url
    };
    let check_url = post_check(t, &m, dir, &check)?;
    write!(t.stdout, "record {record_url}\ncheck {check_url}\n").map_err(Error::cause)?;
    if let Err(error) = require_head(t, &m) {
        let _ = writeln!(
            t.stderr,
            "warning: published, but {}; the new head needs its own review",
            error.render()
        );
    }
    Ok(())
}

/// Refuses when the pull request's head is no longer the snapshot's.
fn require_head(t: &mut Tools<'_>, m: &Manifest) -> Result<(), Error> {
    let p = view_pull(t.programs, m.pull_request)?;
    if p.head != m.head_sha {
        return Err(Error::new(format!(
            "pull request #{} head is {}, not the snapshot's {}",
            m.pull_request, p.head, m.head_sha
        )));
    }
    Ok(())
}

pub fn carry_summary(m: &Manifest) -> String {
    let (url, old_base, old_head) = m.previous.as_ref().map_or(("", "", ""), |p| {
        (p.url.as_str(), p.base_sha.as_str(), p.head_sha.as_str())
    });
    let context = m.patches.iter().filter(|s| s.context_only).count();
    format!(
        "Review record: {url} carried forward. git range-diff {old_base}..{old_head} {}..{} pairs all {} patches unchanged, {context} of them with only context lines changed.",
        m.base_sha,
        m.head_sha,
        m.patches.len()
    )
}

/// A record comment already on the pull request.
#[derive(Default)]
struct ExistingComment {
    url: String,
    body: String,
}

/// A comment body made comparable with the rendered file: GitHub may store CRLF and trim trailing
/// whitespace.
pub fn normalize_record(s: &str) -> String {
    s.replace("\r\n", "\n").trim().to_owned()
}

#[derive(Default, Deserialize)]
struct CommentNode {
    #[serde(default, deserialize_with = "nullable", rename = "html_url")]
    url: String,
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

/// robotogi's last record comment for the snapshot's head, or an empty one when there is none.
fn existing_record(t: &mut Tools<'_>, m: &Manifest) -> Result<ExistingComment, Error> {
    let pr = m.pull_request;
    let out = t
        .programs
        .gh(&[
            "api",
            "--paginate",
            "--slurp",
            &format!("repos/{}/issues/{pr}/comments", m.repository),
        ])
        .map_err(|error| Error::wrap(format!("list comments of pull request #{pr}"), error))?;
    let pages: Vec<Option<Vec<CommentNode>>> = serde_json::from_slice(&out)
        .map_err(|error| Error::wrap(format!("decode comments of pull request #{pr}"), error))?;
    let mut found = ExistingComment::default();
    for c in pages.into_iter().flatten().flatten() {
        if c.user.login != ROBOTOGI_BOT_LOGIN {
            continue;
        }
        if parse_record(&c.body).is_ok_and(|r| r.head_sha == m.head_sha) {
            found = ExistingComment {
                url: c.url,
                body: c.body,
            };
        }
    }
    Ok(found)
}

#[derive(Default, Deserialize)]
struct PostedComment {
    #[serde(default, deserialize_with = "nullable")]
    html_url: String,
}

fn post_record(t: &mut Tools<'_>, m: &Manifest, file: &str) -> Result<String, Error> {
    let out = t
        .programs
        .gh(&[
            "api",
            "--method",
            "POST",
            &format!("repos/{}/issues/{}/comments", m.repository, m.pull_request),
            "-F",
            &format!("body=@{file}"),
        ])
        .map_err(|error| Error::wrap("post the record", error))?;
    match serde_json::from_slice::<PostedComment>(&out) {
        Ok(c) if !c.html_url.is_empty() => Ok(c.html_url),
        _ => Err(Error::new(format!(
            "the posted record's response names no URL: {}",
            String::from_utf8_lossy(&out).trim()
        ))),
    }
}

#[derive(Default, Deserialize)]
struct CheckRun {
    #[serde(default, deserialize_with = "nullable")]
    html_url: String,
    #[serde(default, deserialize_with = "nullable")]
    head_sha: String,
    #[serde(default, deserialize_with = "nullable")]
    app: App,
    #[serde(default, deserialize_with = "nullable")]
    output: CheckOutput,
}

#[derive(Default, Deserialize)]
struct App {
    #[serde(default, deserialize_with = "nullable")]
    slug: String,
}

/// Posts the review check and verifies the returned run: robotogi's, on the reviewed head, linking the
/// record.
fn post_check(
    t: &mut Tools<'_>,
    m: &Manifest,
    dir: &str,
    check: &CheckPayload,
) -> Result<String, Error> {
    let payload = json(check).map_err(|error| Error::wrap("encode check", error))?;
    let file = join(dir, "check.json");
    files::write(&file, payload.as_bytes())
        .map_err(|error| Error::wrap("write check.json", error))?;
    let out = t
        .programs
        .gh(&[
            "api",
            "--method",
            "POST",
            &format!("repos/{}/check-runs", m.repository),
            "--input",
            &file,
        ])
        .map_err(|error| Error::wrap("post the review check", error))?;
    let run: CheckRun = serde_json::from_slice(&out)
        .map_err(|error| Error::wrap("decode the review check", error))?;
    if run.app.slug != ROBOTOGI_LOGIN {
        return Err(Error::new(format!(
            "the review check at {} belongs to app {}, not {ROBOTOGI_LOGIN}",
            run.html_url,
            quote(&run.app.slug)
        )));
    }
    if run.head_sha != m.head_sha {
        return Err(Error::new(format!(
            "the review check at {} is on {}, not the reviewed {}",
            run.html_url, run.head_sha, m.head_sha
        )));
    }
    if run.output.summary != check.output.summary {
        return Err(Error::new(format!(
            "the review check at {} does not carry the record link",
            run.html_url
        )));
    }
    Ok(run.html_url)
}

#[cfg(test)]
#[path = "tests/publish.rs"]
mod tests;
