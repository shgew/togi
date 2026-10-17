//! A re-review's patches: `git range-diff` between the previous record's range and the current one, and
//! the delta of what changed.

use std::sync::LazyLock;

use regex::Regex;
use regex::bytes::Regex as BytesRegex;

use crate::diff::{FileDiff, Hunk, lines, number, parse_diff, same_changes, spans, trim_newline};
use crate::manifest::PatchStatus;
use crate::text::{lossy, quote};
use crate::{Error, Programs};

/// `git range-diff --no-patch` output: old index, old commit, mark, new index, new commit, subject.
static RANGE_DIFF_LINE: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
    Regex::new(
        r"^[\t\n\f\r ]*(?:[0-9]+|-+):[\t\n\f\r ]+([0-9a-f]+|-+)[\t\n\f\r ]+([=!<>])[\t\n\f\r ]+(?:[0-9]+|-+):[\t\n\f\r ]+([0-9a-f]+|-+)[\t\n\f\r ]+(.*)$",
    )
});

static FULL_HUNK_HEADER: LazyLock<Result<BytesRegex, regex::Error>> =
    LazyLock::new(|| BytesRegex::new(r"(?-u)^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@(.*)$"));

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RangeDiffEntry {
    pub old: String,
    pub new: String,
    pub subject: String,
    pub mark: char,
}

pub fn parse_range_diff(out: &str) -> Result<Vec<RangeDiffEntry>, Error> {
    let line_pattern = RANGE_DIFF_LINE
        .as_ref()
        .map_err(|error| Error::wrap("compile a pattern", error.clone()))?;
    let mut entries = Vec::new();
    for line in out.split_inclusive('\n') {
        let line = line.trim_end_matches('\n');
        if line.is_empty() {
            continue;
        }
        let Some(m) = line_pattern.captures(line) else {
            return Err(Error::new(format!(
                "unrecognized range-diff line {}",
                quote(line)
            )));
        };
        let group = |i| m.get(i).map_or("", |g| g.as_str());
        let side = |i| {
            let id = group(i);
            if id.starts_with('-') {
                String::new()
            } else {
                id.to_owned()
            }
        };
        entries.push(RangeDiffEntry {
            old: side(1),
            new: side(3),
            subject: group(4).to_owned(),
            mark: group(2).chars().next().unwrap_or('='),
        });
    }
    Ok(entries)
}

/// A re-review's patch classification and the patches that make up its delta.
#[derive(Clone, Debug, Default)]
pub struct DeltaResult {
    pub patches: Vec<PatchStatus>,
    /// The delta's patches in range-diff order: the new version of each changed patch, each added patch,
    /// and the reverse of each removed one; then the remerge diff of each new merge that changed something.
    pub texts: Vec<Vec<u8>>,
    /// The sections of those patches; the reversed ones carry no changed new-side lines.
    pub files: Vec<Vec<FileDiff>>,
}

impl DeltaResult {
    pub fn carry_forward(&self) -> bool {
        self.patches
            .iter()
            .all(|p| !matches!(p.status.as_str(), "added" | "removed" | "changed"))
    }
}

fn patch(git: &mut dyn Programs, from: &str, to: &str) -> Result<(Vec<u8>, Vec<FileDiff>), Error> {
    let text = git
        .git(&["diff", "--no-color", "--no-ext-diff", from, to])
        .map_err(|error| Error::wrap(format!("diff {from} {to}"), error))?;
    let files =
        parse_diff(&text).map_err(|error| Error::wrap(format!("diff {from} {to}"), error))?;
    Ok((text, files))
}

/// Compares the layer's patches between the previous record's range and the current one. A patch
/// range-diff marks changed whose added and removed lines are the same, so only its context moved, counts
/// as unchanged. Range-diff skips merge commits, so each merge the new range has and the previous head
/// lacks is classified by its remerge diff, what it changed beyond merging its parents mechanically: none
/// is unchanged, any (a conflict resolution, an edit made in the merge) is changed and joins the delta.
pub fn classify_range(
    git: &mut dyn Programs,
    old_base: &str,
    old_head: &str,
    new_base: &str,
    new_head: &str,
) -> Result<DeltaResult, Error> {
    let out = git
        .git(&[
            "range-diff",
            "--no-patch",
            "--no-color",
            &format!("{old_base}..{old_head}"),
            &format!("{new_base}..{new_head}"),
        ])
        .map_err(|error| {
            Error::wrap(
                format!("range-diff of {old_base}..{old_head} and {new_base}..{new_head}"),
                error,
            )
        })?;
    let entries = parse_range_diff(&lossy(&out))?;
    let mut d = DeltaResult::default();
    for e in entries {
        let mut s = PatchStatus {
            old: e.old.clone(),
            new: e.new.clone(),
            subject: e.subject.clone(),
            ..PatchStatus::default()
        };
        match e.mark {
            '=' => "unchanged".clone_into(&mut s.status),
            '<' => {
                // The patch left the range, but its effects may live on: a commit moved into the new base
                // is inherited, not removed. The delta holds the reversal of only what neither the new base
                // nor the new head keeps.
                "removed".clone_into(&mut s.status);
                if let Some(kept) = removed_effects(git, &e.old, new_base, new_head)? {
                    d.texts
                        .push(kept.iter().flat_map(|f| f.text.iter().copied()).collect());
                    d.files.push(kept);
                }
            }
            '>' => {
                "added".clone_into(&mut s.status);
                let (text, files) = patch(git, &format!("{}^", e.new), &e.new)?;
                d.texts.push(text);
                d.files.push(files);
            }
            '!' => {
                let (_, old_files) = patch(git, &format!("{}^", e.old), &e.old)?;
                let (text, new_files) = patch(git, &format!("{}^", e.new), &e.new)?;
                if same_changes(&old_files, &new_files) {
                    "unchanged".clone_into(&mut s.status);
                    s.context_only = true;
                } else {
                    "changed".clone_into(&mut s.status);
                    d.texts.push(text);
                    d.files.push(new_files);
                }
            }
            _ => {}
        }
        d.patches.push(s);
    }
    for (commit, subject) in merges_since(git, old_head, new_base, new_head)? {
        let text = git
            .git(&[
                "show",
                "--remerge-diff",
                "--no-color",
                "--no-ext-diff",
                "--format=",
                &commit,
            ])
            .map_err(|error| Error::wrap(format!("remerge diff of {commit}"), error))?;
        let files = parse_diff(&text)
            .map_err(|error| Error::wrap(format!("remerge diff of {commit}"), error))?;
        let status = if files.is_empty() {
            "unchanged"
        } else {
            d.texts.push(text);
            d.files.push(files);
            "changed"
        };
        d.patches.push(PatchStatus {
            new: commit,
            subject,
            status: status.to_owned(),
            ..PatchStatus::default()
        });
    }
    Ok(d)
}

/// The merge commits of `new_base..new_head` that `old_head` lacks, oldest first, each with its subject.
fn merges_since(
    git: &mut dyn Programs,
    old_head: &str,
    new_base: &str,
    new_head: &str,
) -> Result<Vec<(String, String)>, Error> {
    let out = git
        .git(&[
            "log",
            "--merges",
            "--reverse",
            "--no-color",
            "--format=%H %s",
            &format!("{new_base}..{new_head}"),
            &format!("^{old_head}"),
        ])
        .map_err(|error| {
            Error::wrap(format!("list the merges of {new_base}..{new_head}"), error)
        })?;
    Ok(lossy(&out)
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (commit, subject) = line.split_once(' ').unwrap_or((line, ""));
            (commit.to_owned(), subject.to_owned())
        })
        .collect())
}

/// The reversal of a removed patch's effects that the new range lost, or `None` when the new base
/// contains the patch, itself or rewritten, or nothing is lost.
fn removed_effects(
    git: &mut dyn Programs,
    old: &str,
    new_base: &str,
    new_head: &str,
) -> Result<Option<Vec<FileDiff>>, Error> {
    // A commit the new base contains, itself or rewritten, is inherited whatever lower commits did to
    // its lines since.
    if is_ancestor(git, old, new_base)? {
        return Ok(None);
    }
    let (_, files) = patch(git, old, &format!("{old}^"))?;
    if rewritten_into(git, old, new_base, &files)? {
        return Ok(None);
    }
    let kept = gone_effects(git, old, new_base, new_head, &files)?;
    Ok((!kept.is_empty()).then_some(kept))
}

/// The sections of a removed patch's reversal that undo effects the new range really lost, with no changed
/// new-side lines. `old_patch` is the removed commit, `files` the sections of its reversal: their removed
/// lines are what the patch added, their added lines what it removed. Each hunk, and each file's mode or
/// binary change, is traced separately against `new_base`, which may have inherited the effect, and against
/// `new_head`, which may keep it through other commits; only what neither has is gone, so one lost hunk
/// never reverses another a lower layer already carries.
fn gone_effects(
    git: &mut dyn Programs,
    old_patch: &str,
    new_base: &str,
    new_head: &str,
    files: &[FileDiff],
) -> Result<Vec<FileDiff>, Error> {
    let mut kept = Vec::new();
    for f in files {
        let name = source_name(f);
        let mut specs = vec![format!(":(literal){name}")];
        if f.path != name {
            // A rename or copy: the file's other name tells whether it is back.
            specs.push(format!(":(literal){}", f.path));
        }
        let mut sides = Vec::with_capacity(2);
        for target in [new_base, new_head] {
            let mut args = vec![
                "diff",
                "--no-color",
                "--no-ext-diff",
                "--no-renames",
                "-U0",
                old_patch,
                target,
                "--",
            ];
            args.extend(specs.iter().map(String::as_str));
            let context = || format!("diff {old_patch} {target} -- {name}");
            let out = git
                .git(&args)
                .map_err(|error| Error::wrap(context(), error))?;
            sides.push(parse_diff(&out).map_err(|error| Error::wrap(context(), error))?);
        }
        if let Some(lost) = lost_effects(f, &sides[0], &sides[1]) {
            kept.push(lost);
        }
    }
    Ok(kept)
}

/// `f`, the reversal of a removed patch's changes to one file, narrowed to the changes that neither the new
/// base nor the new head keeps (`base` and `head` are the changes from the patch to each). Each changed line
/// is traced on its own, so adjacent lines of one hunk that a lower layer carries stay out of the reversal,
/// which is rendered again as coherent hunks. The file operation (creation, deletion, rename, copy) and the
/// mode are traced on their own too: one a lower layer carries leaves the header, and the text then applies
/// to the file as it stands now. `None` when nothing is lost. Anything it cannot tell apart counts as lost,
/// so an effect is never dropped from the review wrongly.
pub fn lost_effects(f: &FileDiff, base: &[FileDiff], head: &[FileDiff]) -> Option<FileDiff> {
    let mut f = f.clone();
    f.changed.clear();
    let name = source_name(&f).to_owned();
    let op_lost = f.status != "modified" && operation_lost(&f, base) && operation_lost(&f, head);
    let mode_lost = has_mode_change(&f) && changes_mode(base) && changes_mode(head);
    let lost = |sign: u8, old: i64, text: &[u8]| {
        touches(sign, old, text, &name, base) && touches(sign, old, text, &name, head)
    };
    let mut hunks = Vec::new();
    let mut body = Vec::new();
    let (mut offset, mut adds, mut dels, mut retained) = (0, 0, 0, false);
    for h in &f.hunks {
        let (narrowed, hunk_adds, hunk_dels, partial) = narrow_hunk(h, &lost, offset);
        retained = retained || partial;
        if narrowed.text.is_empty() {
            continue;
        }
        body.extend_from_slice(&narrowed.text);
        hunks.push(narrowed);
        adds += hunk_adds;
        dels += hunk_dels;
        offset += hunk_adds - hunk_dels;
    }
    let binary_lost = f.binary && !base.is_empty() && !head.is_empty();
    if hunks.is_empty() && !mode_lost && !op_lost && !binary_lost {
        return None;
    }
    let survives = f.status != "modified" && !op_lost;
    let cur = if f.status == "renamed" || f.status == "copied" {
        // The file stays where the patch left it.
        f.old_path.clone()
    } else {
        f.path.clone()
    };
    let header = narrow_header(&f, &cur, survives, mode_lost, retained, !hunks.is_empty());
    let mut text = header.clone();
    text.extend_from_slice(&body);
    if survives {
        if f.binary {
            text.extend_from_slice(format!("Binary files a/{cur} and b/{cur} differ\n").as_bytes());
        }
        f.path = cur;
        f.old_path.clear();
        "modified".clone_into(&mut f.status);
    }
    f.removals = hunks
        .iter()
        .flat_map(|h| h.removals.iter().copied())
        .collect();
    f.text = text;
    f.added = adds;
    f.removed = dels;
    f.mode = lines(&header)
        .map(trim_newline)
        .filter(|line| {
            line.starts_with(b"old mode ")
                || line.starts_with(b"new mode ")
                || line.starts_with(b"new file mode")
                || line.starts_with(b"deleted file mode")
        })
        .collect::<Vec<_>>()
        .join(&b'\n');
    f.header = header;
    f.hunks = hunks;
    Some(f)
}

/// The name of the file as the removed patch left it: the source of a reversal that renames or copies,
/// else its path.
fn source_name(f: &FileDiff) -> &str {
    if f.old_path.is_empty() {
        &f.path
    } else {
        &f.old_path
    }
}

/// Whether the changes since the patch undo the file operation of `f`, the reversal of what the patch did:
/// the patch created the file (`f` deletes it) and it is gone, the patch deleted it (`f` creates it) and it
/// is back, or the patch renamed or copied it (`f` undoes that) and the source is gone or the target is back.
fn operation_lost(f: &FileDiff, since: &[FileDiff]) -> bool {
    let has = |path: &str, status: &str| since.iter().any(|s| s.path == path && s.status == status);
    match f.status.as_str() {
        "deleted" => has(&f.path, "deleted"),
        "added" | "copied" => has(&f.path, "added"),
        "renamed" => has(&f.old_path, "deleted") || has(&f.path, "added"),
        _ => false,
    }
}

fn has_mode_change(f: &FileDiff) -> bool {
    crate::diff::find(&f.mode, b"old mode").is_some()
}

fn changes_mode(since: &[FileDiff]) -> bool {
    since.iter().any(has_mode_change)
}

/// Whether the changes since the patch undo one line of the reversal: the line the reversal removes
/// (old-side line number `old` of the file `name`), which they removed too, or the line the reversal adds
/// (`text`), which they brought back.
fn touches(sign: u8, old: i64, text: &[u8], name: &str, since: &[FileDiff]) -> bool {
    for s in since {
        if s.binary {
            return true;
        }
        if sign == b'-' {
            if s.path == name && s.removals.iter().any(|r| r.first <= old && old <= r.last) {
                return true;
            }
            continue;
        }
        if s.changed_lines()
            .iter()
            .any(|line| line.strip_prefix(b"+") == Some(text))
        {
            return true;
        }
    }
    false
}

/// The changes of one reversal hunk that `lost` reports lost, as one coherent hunk: a reversed removal that
/// is kept stays as context, a reversed addition that is kept is dropped, and the line counts and the
/// new-side start, which `offset` shifts by the hunks before it, are rewritten. A hunk with no lost change
/// comes back with empty text. `retained` says some change was left out; a hunk kept whole comes back
/// verbatim when its numbering is unchanged.
fn narrow_hunk(
    h: &Hunk,
    lost: &dyn Fn(u8, i64, &[u8]) -> bool,
    offset: i64,
) -> (Hunk, i64, i64, bool) {
    let hunk_lines: Vec<&[u8]> = lines(&h.text).collect();
    let header = trim_newline(hunk_lines.first().copied().unwrap_or_default());
    let Some(m) = FULL_HUNK_HEADER
        .as_ref()
        .ok()
        .and_then(|pattern| pattern.captures(header))
    else {
        return (h.clone(), 0, 0, false);
    };
    let old_start = m.get(1).map_or(0, |d| number(d.as_bytes()));
    let new_start = m.get(3).map_or(0, |d| number(d.as_bytes()));
    let context = m.get(5).map_or(&[][..], |g| g.as_bytes());
    let mut old_no = old_start;
    let mut body = Vec::new();
    let mut removed = Vec::new();
    let mut added = Vec::new();
    let (mut adds, mut dels, mut retained) = (0, 0, false);
    let (mut old_count, mut new_count) = (0, 0);
    let mut last_kept = false;
    for line in hunk_lines.iter().skip(1) {
        let text = trim_newline(line);
        match line.first() {
            Some(b' ') => {
                body.extend_from_slice(line);
                old_count += 1;
                new_count += 1;
                old_no += 1;
                last_kept = true;
            }
            Some(b'-') => {
                if lost(b'-', old_no, &text[1..]) {
                    body.extend_from_slice(line);
                    removed.push(old_no);
                    dels += 1;
                    old_count += 1;
                } else {
                    body.push(b' ');
                    body.extend_from_slice(&line[1..]);
                    old_count += 1;
                    new_count += 1;
                    retained = true;
                }
                old_no += 1;
                last_kept = true;
            }
            Some(b'+') => {
                if lost(b'+', 0, &text[1..]) {
                    body.extend_from_slice(line);
                    added.push(text[1..].to_vec());
                    adds += 1;
                    new_count += 1;
                    last_kept = true;
                } else {
                    retained = true;
                    last_kept = false;
                }
            }
            Some(b'\\') if last_kept => body.extend_from_slice(line),
            _ => {}
        }
    }
    if adds + dels == 0 {
        return (Hunk::default(), 0, 0, retained);
    }
    let mut first = old_start;
    if old_count == 0 {
        first += 1;
    }
    first += offset;
    if new_count == 0 {
        first -= 1;
    }
    if !retained && first == new_start {
        return (h.clone(), adds, dels, false);
    }
    let mut text = format!("@@ -{old_start},{old_count} +{first},{new_count} @@").into_bytes();
    text.extend_from_slice(context);
    text.push(b'\n');
    text.extend_from_slice(&body);
    let hunk = Hunk {
        text,
        removals: spans(&removed),
        added,
    };
    (hunk, adds, dels, retained)
}

/// The header of a narrowed reversal. Mode lines stay only when the mode change is lost. When the file
/// operation survives (`survives`), the old header, which names a creation, deletion, rename or copy, is
/// replaced by a content-only header on `cur`, the file's current name. Otherwise the old header stays,
/// minus the index line when some change of the text is left out, and minus the file name lines when no
/// hunk is left.
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "each flag is a separate fact about the reversal the header follows"
)]
fn narrow_header(
    f: &FileDiff,
    cur: &str,
    survives: bool,
    mode_lost: bool,
    partial: bool,
    hunks: bool,
) -> Vec<u8> {
    let mut b = Vec::new();
    if survives {
        b.extend_from_slice(format!("diff --git a/{cur} b/{cur}\n").as_bytes());
    }
    for line in lines(&f.header) {
        let t = trim_newline(line);
        let skip = if t.starts_with(b"old mode ") || t.starts_with(b"new mode ") {
            !mode_lost
        } else {
            survives
                || (!hunks && (t.starts_with(b"--- ") || t.starts_with(b"+++ ")))
                || (t.starts_with(b"index ") && partial)
        };
        if !skip {
            b.extend_from_slice(line);
        }
    }
    if survives && hunks {
        b.extend_from_slice(format!("--- a/{cur}\n+++ b/{cur}\n").as_bytes());
    }
    b
}

/// Whether the new base contains `commit` itself. Git's exit status 1 means it does not; any other failure
/// is an error.
fn is_ancestor(git: &mut dyn Programs, commit: &str, base: &str) -> Result<bool, Error> {
    match git.git(&["merge-base", "--is-ancestor", commit, base]) {
        Ok(_) => Ok(true),
        Err(error) if error.exit_code() == Some(1) => Ok(false),
        Err(error) => Err(Error::wrap(
            format!("merge-base --is-ancestor {commit} {base}"),
            error,
        )),
    }
}

/// Whether the new base, which does not contain `commit` itself, contains it rewritten: a commit of the base
/// that changes the files `commit` touches (those of `files`, its reversal) exactly as `commit` does.
/// `same_changes` is the proof: the same added and removed lines byte for byte, modes and binary contents,
/// wherever the hunks sit. Patch IDs, as git cherry compares them, are no proof: they ignore whitespace, so
/// "a b" and "ab" would match. When no proof exists the removal is traced by its lines instead.
fn rewritten_into(
    git: &mut dyn Programs,
    commit: &str,
    base: &str,
    files: &[FileDiff],
) -> Result<bool, Error> {
    let mut specs: Vec<String> = Vec::new();
    for f in files {
        for p in [&f.path, &f.old_path] {
            let spec = format!(":(literal){p}");
            if !p.is_empty() && !specs.contains(&spec) {
                specs.push(spec);
            }
        }
    }
    if specs.is_empty() {
        return Ok(false);
    }
    let exact = [
        "--no-color",
        "--no-ext-diff",
        "--no-renames",
        "--full-index",
    ];
    let range = format!("{commit}..{base}");
    // Each commit of the base that touches those files, as its patch limited to them, after a NUL.
    let mut args = vec![
        "log",
        "--no-merges",
        "--no-show-signature",
        "-p",
        "--format=%x00",
    ];
    args.extend(exact);
    args.extend([range.as_str(), "--"]);
    args.extend(specs.iter().map(String::as_str));
    let log_context = || format!("log {commit}..{base}");
    let out = git
        .git(&args)
        .map_err(|error| Error::wrap(log_context(), error))?;
    let candidates: Vec<&[u8]> = out.split(|b| *b == 0).skip(1).collect();
    if candidates.is_empty() {
        return Ok(false);
    }
    let parent = format!("{commit}^");
    let mut args = vec!["diff"];
    args.extend(exact);
    args.extend([parent.as_str(), commit]);
    let diff_context = || format!("diff {commit}^ {commit}");
    let out = git
        .git(&args)
        .map_err(|error| Error::wrap(diff_context(), error))?;
    let own = parse_diff(&out).map_err(|error| Error::wrap(diff_context(), error))?;
    for candidate in candidates {
        let start = candidate
            .iter()
            .position(|b| *b != b'\n')
            .unwrap_or(candidate.len());
        let theirs =
            parse_diff(&candidate[start..]).map_err(|error| Error::wrap(log_context(), error))?;
        if same_changes(&own, &theirs) {
            return Ok(true);
        }
    }
    Ok(false)
}
