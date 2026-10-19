//! `snapshot`: freezes a pull request's head, base and diff into a directory of hunk files and a manifest.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::sync::LazyLock;

use regex::Regex;
use serde::Deserialize;

use crate::cover::add_cover;
use crate::diff::{
    FileDiff, fixed_exclusion, generated_content, merge_by_path, parse_diff, whole_content,
};
use crate::manifest::{
    MANIFEST_VERSION, Manifest, ManifestFile, PreviousRecord, hunk_file_name, is_small,
    reviewer_count, total_lines, write_manifest,
};
use crate::parts::{join_records, parse_part, robotogi_comments};
use crate::rangediff::{DeltaResult, classify_range};
use crate::record::parse_record;
use crate::text::{join, nullable, path_escape, quote, short};
use crate::{Error, Programs, ROBOTOGI_BOT_LOGIN, Tools, files};

pub const SNAPSHOT_USAGE: &str = "usage: reviews snapshot [--previous <record comment URL> [--previous-base <full base SHA>]] [--cover <file or ->] <PR number>\n       reviews snapshot --cover <file or -> <snapshot directory>";

const FLAG_USAGE: &str = "Usage of snapshot:
  -cover just cover
    \tfile with just cover output, or - for standard input
  -previous string
    \tURL of the previous review record comment, for a re-review
  -previous-base string
    \tfull SHA of the base the pull request had at the previous record's head, for a record that does not name it (version 1, or 2 without base_sha)
";

static PULL_URL: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"^https://github\.com/([^/]+/[^/]+)/pull/[0-9]+$"));
static COMMENT_URL: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
    Regex::new(
        r"^https://github\.com/([^/]+/[^/]+)/(?:pull|issues)/([0-9]+)#issuecomment-([0-9]+)$",
    )
});
/// What `--previous-base` accepts: a complete lower-case commit SHA, SHA-1 or SHA-256.
static PREVIOUS_BASE_SHA: LazyLock<Result<Regex, regex::Error>> =
    LazyLock::new(|| Regex::new(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$"));

fn compiled(
    pattern: &'static LazyLock<Result<Regex, regex::Error>>,
) -> Result<&'static Regex, Error> {
    pattern
        .as_ref()
        .map_err(|error| Error::wrap("compile a pattern", error.clone()))
}

/// The head and base a snapshot freezes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct PullInfo {
    #[serde(default, deserialize_with = "nullable", rename = "headRefOid")]
    pub head: String,
    #[serde(default, deserialize_with = "nullable", rename = "baseRefOid")]
    pub base: String,
    #[serde(default, deserialize_with = "nullable")]
    pub url: String,
}

pub fn view_pull(gh: &mut dyn Programs, pr: i64) -> Result<PullInfo, Error> {
    let out = gh
        .gh(&[
            "pr",
            "view",
            &pr.to_string(),
            "--json",
            "headRefOid,baseRefOid,url",
        ])
        .map_err(|error| Error::wrap(format!("view pull request #{pr}"), error))?;
    let p: PullInfo = serde_json::from_slice(&out)
        .map_err(|error| Error::wrap(format!("decode pull request #{pr}"), error))?;
    if p.head.is_empty() || p.base.is_empty() {
        return Err(Error::new(format!(
            "pull request #{pr} has no head or base commit"
        )));
    }
    Ok(p)
}

impl PullInfo {
    fn repository(&self) -> Result<String, Error> {
        compiled(&PULL_URL)?
            .captures(&self.url)
            .map(|m| m[1].to_owned())
            .ok_or_else(|| {
                Error::new(format!(
                    "cannot read the repository from pull request URL {}",
                    quote(&self.url)
                ))
            })
    }
}

#[derive(Default)]
struct Flags {
    previous: String,
    previous_base: String,
    cover: String,
}

/// Reads the flags that come before the first argument that is not one, `-name value`, `-name=value` or
/// with two dashes; `--` ends them. A failure prints what went wrong and the flags' usage to `stderr`.
fn parse_flags(args: &[String], stderr: &mut dyn Write) -> Result<(Flags, Vec<String>), Error> {
    let mut flags = Flags::default();
    let mut i = 0;
    let fail = |stderr: &mut dyn Write, message: String| {
        let _ = write!(stderr, "{message}\n{FLAG_USAGE}");
        Error::new(message)
    };
    while i < args.len() {
        let arg = &args[i];
        if arg.len() < 2 || !arg.starts_with('-') {
            break;
        }
        let dashes = if arg.as_bytes()[1] == b'-' {
            if arg.len() == 2 {
                i += 1;
                break;
            }
            2
        } else {
            1
        };
        let name = &arg[dashes..];
        if name.is_empty() || name.starts_with('-') || name.starts_with('=') {
            return Err(fail(stderr, format!("bad flag syntax: {arg}")));
        }
        i += 1;
        let (name, value) = name
            .split_once('=')
            .map_or((name, None), |(name, value)| (name, Some(value.to_owned())));
        let slot = match name {
            "previous" => &mut flags.previous,
            "previous-base" => &mut flags.previous_base,
            "cover" => &mut flags.cover,
            "help" | "h" => {
                let _ = stderr.write_all(FLAG_USAGE.as_bytes());
                return Err(Error::new("flag: help requested"));
            }
            _ => {
                return Err(fail(
                    stderr,
                    format!("flag provided but not defined: -{name}"),
                ));
            }
        };
        *slot = if let Some(value) = value {
            value
        } else {
            let Some(value) = args.get(i) else {
                return Err(fail(stderr, format!("flag needs an argument: -{name}")));
            };
            i += 1;
            value.clone()
        };
    }
    Ok((flags, args[i..].to_vec()))
}

pub fn run_snapshot(t: &mut Tools<'_>, args: &[String]) -> Result<(), Error> {
    let (flags, rest) = parse_flags(args, t.stderr)?;
    let [target] = rest.as_slice() else {
        return Err(Error::new(SNAPSHOT_USAGE));
    };
    let cover_text = if !flags.cover.is_empty() {
        let bytes = read_input(t.stdin, &flags.cover)
            .map_err(|error| Error::wrap("read cover output", error))?;
        String::from_utf8_lossy(&bytes).into_owned()
    } else {
        String::new()
    };
    let Ok(pr) = target.parse::<i64>() else {
        if flags.cover.is_empty() || !flags.previous.is_empty() || !flags.previous_base.is_empty() {
            return Err(Error::new(SNAPSHOT_USAGE));
        }
        let m = add_cover(target, &cover_text)?;
        return print_summary(t.stdout, target, &m);
    };
    if !flags.previous_base.is_empty()
        && (flags.previous.is_empty()
            || !compiled(&PREVIOUS_BASE_SHA)?.is_match(&flags.previous_base))
    {
        return Err(Error::new(format!(
            "--previous-base needs --previous and a full lower-case commit SHA\n{SNAPSHOT_USAGE}"
        )));
    }
    let (dir, mut m) = create_snapshot(t, pr, &flags.previous, &flags.previous_base)?;
    if !flags.cover.is_empty() {
        m = add_cover(&dir, &cover_text)?;
    }
    print_summary(t.stdout, &dir, &m)
}

fn read_input(stdin: &mut dyn Read, name: &str) -> Result<Vec<u8>, Error> {
    if name == "-" {
        let mut bytes = Vec::new();
        stdin.read_to_end(&mut bytes).map_err(Error::cause)?;
        return Ok(bytes);
    }
    files::read(name)
}

/// Freezes pull request `pr` and writes its snapshot directory. It refuses when the head or base moved
/// while it ran. `previous_base`, when set, is the base the previous record's head had, for a record that
/// does not name it.
pub fn create_snapshot(
    t: &mut Tools<'_>,
    pr: i64,
    previous_url: &str,
    previous_base: &str,
) -> Result<(String, Manifest), Error> {
    let before = view_pull(t.programs, pr)?;
    let repo = before.repository()?;
    let raw = pull_diff(t, pr, &before)?;
    let after = view_pull(t.programs, pr)?;
    moved_since(pr, &before, &after)?;
    let pr_files = parse_diff(&raw)
        .map_err(|error| Error::wrap(format!("read the diff of pull request #{pr}"), error))?;
    let mut m = Manifest {
        version: MANIFEST_VERSION,
        repository: repo.clone(),
        pull_request: pr,
        head_sha: before.head.clone(),
        base_sha: before.base.clone(),
        small: is_small(&pr_files),
        pr_files: crate::count(pr_files.len()),
        pr_lines: total_lines(&pr_files),
        ..Manifest::default()
    };
    let mut scope = pr_files;
    let mut delta = Vec::new();
    if !previous_url.is_empty() {
        previous_record_of(t, &repo, pr, previous_url, previous_base, &mut m)?;
        if m.fallback.is_empty() {
            let d = review_delta(t.programs, &m)?;
            m.carry_forward =
                d.carry_forward() && m.previous.as_ref().is_some_and(|p| p.verdict == "success");
            m.patches = d.patches;
            scope = merge_by_path(d.files);
            delta = d.texts.concat();
        }
    }
    // Everything that reaches out for the frozen head comes before the last look at the pull request.
    let complete = m.previous.is_none() || !m.fallback.is_empty();
    let mut exclusions = Vec::with_capacity(scope.len());
    for f in &scope {
        exclusions.push(exclusion_of(t.programs, &repo, &m.head_sha, f, complete)?);
    }
    let last = view_pull(t.programs, pr)?;
    moved_since(pr, &before, &last)?;
    let dir = join(&t.tmp, &format!("togi-review/{pr}-{}", short(&before.head)));
    files::remove_all(&dir).map_err(|error| Error::wrap(format!("clear {dir}"), error))?;
    files::create_dir_all(&join(&dir, "files"))
        .map_err(|error| Error::wrap(format!("create {dir}"), error))?;
    files::write(&join(&dir, "pr.diff"), &raw)
        .map_err(|error| Error::wrap("write pr.diff", error))?;
    if m.previous.is_some() && m.fallback.is_empty() {
        files::write(&join(&dir, "delta.diff"), &delta)
            .map_err(|error| Error::wrap("write delta.diff", error))?;
    }
    for (i, (f, exclusion)) in scope.iter().zip(exclusions).enumerate() {
        let mut mf = ManifestFile {
            path: f.path.clone(),
            old_path: f.old_path.clone(),
            status: f.status.clone(),
            added: f.added,
            removed: f.removed,
            binary: f.binary,
            exclusion,
            hunk_file: String::new(),
        };
        if mf.exclusion.is_empty() {
            mf.hunk_file = hunk_file_name(i + 1);
            files::write(&join(&dir, &mf.hunk_file), &f.text)
                .map_err(|error| Error::wrap(format!("write {}", mf.hunk_file), error))?;
            m.included_files += 1;
            m.included_lines += f.added + f.removed;
        }
        m.files.push(mf);
    }
    m.reviewers = reviewer_count(m.included_lines, m.included_files);
    write_manifest(&dir, &m)?;
    Ok((dir, m))
}

/// Refuses a snapshot whose pull request head or base is no longer the frozen one.
fn moved_since(pr: i64, before: &PullInfo, after: &PullInfo) -> Result<(), Error> {
    if after.head != before.head || after.base != before.base {
        return Err(Error::new(format!(
            "pull request #{pr} moved while the snapshot ran (head {}, base {} before; head {}, base {} after): run snapshot again",
            before.head, before.base, after.head, after.base
        )));
    }
    Ok(())
}

/// The diff of pull request `pr`. When GitHub refuses it as too large, git builds it from the frozen base
/// and head instead, as the merge-base diff GitHub would show.
fn pull_diff(t: &mut Tools<'_>, pr: i64, frozen: &PullInfo) -> Result<Vec<u8>, Error> {
    // The diff goes to files, never to a terminal, so gh's guard against escape sequences, which a diff of
    // terminal test data trips, does not apply.
    match t.programs.gh(&[
        "pr",
        "diff",
        &pr.to_string(),
        "--color=never",
        "--allow-escape-sequences",
    ]) {
        Ok(raw) => Ok(raw),
        Err(error) if diff_too_large(&error.to_string()) => local_pull_diff(t, pr, frozen),
        Err(error) => Err(Error::wrap(
            format!("fetch the diff of pull request #{pr}"),
            error,
        )),
    }
}

/// The merge-base diff of the frozen base and head, built in a temporary bare repository holding their
/// complete history, so neither the caller's checkout (shallow, or with its own configuration and
/// attributes) nor its Git environment shapes the bytes.
fn local_pull_diff(t: &mut Tools<'_>, pr: i64, frozen: &PullInfo) -> Result<Vec<u8>, Error> {
    let out = t.programs.git(&["remote", "get-url", "origin"]).map_err(|error| {
        Error::wrap(
            format!("read the origin URL to build the diff of pull request #{pr}, which GitHub refuses as too large"),
            error,
        )
    })?;
    let dir = tempfile::Builder::new()
        .prefix("togi-review-diff-")
        .tempdir_in(&t.tmp)
        .map_err(|error| {
            Error::wrap(
                format!("make a temporary repository to build the diff of pull request #{pr}, which GitHub refuses as too large"),
                Error::path("mkdirtemp", std::path::Path::new(&format!("{}/togi-review-diff-*", t.tmp)), error),
            )
        })?;
    t.programs
        .isolated_git(dir.path(), &["init", "--quiet", "--bare", "--template="])
        .map_err(|error| {
            Error::wrap(
                format!("initialise the temporary repository for the diff of pull request #{pr}, which GitHub refuses as too large"),
                error,
            )
        })?;
    let origin = String::from_utf8_lossy(&out);
    t.programs
        .isolated_git(
            dir.path(),
            &["fetch", "--no-tags", "--quiet", origin.trim(), &frozen.base, &frozen.head],
        )
        .map_err(|error| {
            Error::wrap(
                format!("fetch the frozen base and head of pull request #{pr}, whose diff GitHub refuses as too large"),
                error,
            )
        })?;
    t.programs
        .isolated_git(
            dir.path(),
            &[
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-textconv",
                "-M",
                "--src-prefix=a/",
                "--dst-prefix=b/",
                &format!("{}...{}", frozen.base, frozen.head),
            ],
        )
        .map_err(|error| {
            Error::wrap(
                format!("build the diff of pull request #{pr}, which GitHub refuses as too large"),
                error,
            )
        })
}

/// Whether gh failed because GitHub refuses a diff over its line or file limit.
pub fn diff_too_large(message: &str) -> bool {
    message.contains("HTTP 406")
        && (message.contains("the diff exceeded the maximum number of lines")
            || message.contains("the diff exceeded the maximum number of files"))
}

/// Why the review skips the file, or `""` when it is included. Paths and kinds exclude by themselves.
/// Whether a file is generated is decided on its contents, never on marker lines a patch shows: a marker in
/// context or outside the hunks counts, one after the package clause or inside a string does not, and one
/// an earlier patch of a delta added and a later one dropped does not either. `complete` says the sections
/// hold each file's whole change, as the pull request's diff does: an added or deleted file's section then
/// is its whole content and needs no read. Any other file is read at the frozen head; one the head lacks
/// has no contents to judge and stays in the review.
fn exclusion_of(
    gh: &mut dyn Programs,
    repo: &str,
    head: &str,
    f: &FileDiff,
    complete: bool,
) -> Result<String, Error> {
    let reason = fixed_exclusion(f);
    if !reason.is_empty() {
        return Ok(reason.to_owned());
    }
    let content = if complete && (f.status == "added" || f.status == "deleted") {
        whole_content(f)
    } else {
        file_at(gh, repo, head, &f.path)?.unwrap_or_default()
    };
    Ok(if generated_content(&content) {
        "generated"
    } else {
        ""
    }
    .to_owned())
}

/// A file at a commit, read through the GitHub API so the reviewing host needs no checkout of it; `None`
/// when the commit lacks it.
fn file_at(
    gh: &mut dyn Programs,
    repo: &str,
    reference: &str,
    name: &str,
) -> Result<Option<Vec<u8>>, Error> {
    let path = name
        .split('/')
        .map(path_escape)
        .collect::<Vec<_>>()
        .join("/");
    match gh.gh(&[
        "api",
        "-H",
        "Accept: application/vnd.github.raw+json",
        &format!("repos/{repo}/contents/{path}?ref={reference}"),
    ]) {
        Ok(out) => Ok(Some(out)),
        Err(error) if error.to_string().contains("HTTP 404") => Ok(None),
        Err(error) => Err(Error::wrap(
            format!("read {name} at {}", short(reference)),
            error,
        )),
    }
}

#[derive(Default, Deserialize)]
struct CommentResponse {
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

/// Reads the previous record comment and sets `m.previous`. A record that lacks its base takes
/// `previous_base` when the caller recovered one; without either it cannot be diffed and `m.fallback` says
/// the whole layer is reviewed.
fn previous_record_of(
    t: &mut Tools<'_>,
    repo: &str,
    pr: i64,
    url: &str,
    previous_base: &str,
    m: &mut Manifest,
) -> Result<(), Error> {
    let Some(found) = compiled(&COMMENT_URL)?.captures(url) else {
        return Err(Error::new(format!(
            "previous {} is not a pull request comment URL",
            quote(url)
        )));
    };
    if &found[1] != repo || found[2] != pr.to_string() {
        return Err(Error::new(format!(
            "previous record {url} is not on pull request #{pr} of {repo}"
        )));
    }
    let out = t
        .programs
        .gh(&[
            "api",
            &format!("repos/{repo}/issues/comments/{}", &found[3]),
        ])
        .map_err(|error| Error::wrap("fetch the previous record", error))?;
    let c: CommentResponse = serde_json::from_slice(&out)
        .map_err(|error| Error::wrap("decode the previous record comment", error))?;
    if c.user.login != ROBOTOGI_BOT_LOGIN {
        return Err(Error::new(format!(
            "previous record {url} was posted by {}, not {ROBOTOGI_BOT_LOGIN}",
            quote(&c.user.login)
        )));
    }
    let body = if parse_part(&c.body).is_some() {
        let records = join_records(robotogi_comments(t.programs, repo, pr)?)?;
        let comment = format!("#issuecomment-{}", &found[3]);
        let Some(record) = records.into_iter().find(|r| r.url.ends_with(&comment)) else {
            return Err(Error::new(format!(
                "previous record {url} is not the first part of a record whose parts are all posted"
            )));
        };
        record.body
    } else {
        c.body
    };
    let r = parse_record(&body)
        .map_err(|error| Error::wrap(format!("read the previous record {url}"), error))?;
    if r.head_sha.is_empty() {
        return Err(Error::new(format!(
            "previous record {url} names no head_sha"
        )));
    }
    if r.head_sha == m.head_sha {
        return Err(Error::new(format!(
            "the previous record already covers this head: {url}"
        )));
    }
    let base = if r.base_sha.is_empty() {
        previous_base.to_owned()
    } else if !previous_base.is_empty() && previous_base != r.base_sha {
        return Err(Error::new(format!(
            "previous record {url} names base {}, not the supplied --previous-base {previous_base}",
            r.base_sha
        )));
    } else {
        r.base_sha.clone()
    };
    if base.is_empty() {
        m.fallback = format!(
            "the previous record is version {} and does not name the base the pull request had at its head, so no minimal delta can be established",
            r.version
        );
    }
    m.previous = Some(PreviousRecord {
        url: url.to_owned(),
        head_sha: r.head_sha,
        base_sha: base,
        verdict: r.verdict,
    });
    Ok(())
}

/// Fetches the commits of both ranges and classifies the layer's patches between them.
fn review_delta(git: &mut dyn Programs, m: &Manifest) -> Result<DeltaResult, Error> {
    let Some(p) = &m.previous else {
        return Ok(DeltaResult::default());
    };
    git.git(&[
        "fetch",
        "--quiet",
        "origin",
        &p.base_sha,
        &p.head_sha,
        &m.base_sha,
        &m.head_sha,
    ])
    .map_err(|error| Error::wrap("fetch the commits of both ranges", error))?;
    classify_range(git, &p.base_sha, &p.head_sha, &m.base_sha, &m.head_sha)
}

/// Prints what a coordinator needs to start: where the snapshot is, what it froze and how to size the
/// review.
pub fn print_summary(w: &mut dyn Write, dir: &str, m: &Manifest) -> Result<(), Error> {
    let mut b = String::new();
    let _ = writeln!(b, "snapshot {dir}");
    let _ = write!(
        b,
        "pull request #{} in {}\nhead {}\nbase {}\n",
        m.pull_request, m.repository, m.head_sha, m.base_sha
    );
    if let Some(previous) = &m.previous {
        let _ = writeln!(
            b,
            "previous record {} on {}",
            previous.url, previous.head_sha
        );
        if m.fallback.is_empty() {
            let count = |status: &str| m.patches.iter().filter(|p| p.status == status).count();
            let context = m.patches.iter().filter(|p| p.context_only).count();
            let (changed, added, removed) = (count("changed"), count("added"), count("removed"));
            let _ = writeln!(
                b,
                "patches: {} unchanged ({context} with only context changed), {changed} changed, {added} added, {removed} removed",
                count("unchanged")
            );
            if m.carry_forward {
                b.push_str("every patch is unchanged: carry the record forward with a check on the new head\n");
            } else if changed + added + removed == 0 {
                let verdict = if previous.verdict.is_empty() {
                    "not recorded"
                } else {
                    &previous.verdict
                };
                let _ = writeln!(
                    b,
                    "every patch is unchanged, but the previous record's verdict is {verdict}, not success: it is not carried forward and the new head needs its own record"
                );
            }
        } else {
            let _ = writeln!(b, "fallback: {}", m.fallback);
        }
    }
    let _ = writeln!(
        b,
        "files: {}, included {}, excluded {}",
        m.files.len(),
        m.included_files,
        crate::count(m.files.len()) - m.included_files
    );
    for f in &m.files {
        if f.exclusion.is_empty() {
            let _ = writeln!(
                b,
                "  {} {} +{} -{}",
                f.hunk_file, f.path, f.added, f.removed
            );
        } else {
            let _ = writeln!(
                b,
                "  excluded {} +{} -{}: {}",
                f.path, f.added, f.removed, f.exclusion
            );
        }
    }
    let _ = writeln!(
        b,
        "L {}, F {}: {} reviewers",
        m.included_lines, m.included_files, m.reviewers
    );
    let _ = writeln!(
        b,
        "small pull request: {} (whole diff {} files, {} lines)",
        m.small, m.pr_files, m.pr_lines
    );
    if let Some(coverage) = &m.coverage {
        let _ = writeln!(
            b,
            "uncovered: {} of {} reported ranges inside the hunks, {} files not built",
            coverage.ranges.len(),
            coverage.reported,
            coverage.not_built.len()
        );
        for r in &coverage.ranges {
            let _ = writeln!(b, "  {}:{}-{}", r.path, r.first, r.last);
        }
    } else if m.changes_code() {
        b.push_str("uncovered: not recorded yet; run `just cover <base>` and pass its output to `snapshot --cover`\n");
    }
    w.write_all(b.as_bytes()).map_err(Error::cause)
}

#[cfg(test)]
#[path = "tests/snapshot.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "tests/snapshot_fixes.rs"]
mod fixes_tests;

#[cfg(test)]
#[path = "tests/snapshot_removed.rs"]
mod removed_tests;

#[cfg(test)]
#[path = "tests/snapshot_process.rs"]
mod process_tests;
