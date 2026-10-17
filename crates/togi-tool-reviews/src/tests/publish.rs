use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use serde_json::{Value, json};

use super::{CheckPayload, run_publish};
use crate::fakes::{
    BASE_SHA, HEAD_SHA, Harness, MOVED_SHA, OLD_BASE_SHA, OLD_HEAD_SHA, PR_URL, PREVIOUS_URL, fail,
    read_file,
};
use crate::manifest::{PatchStatus, PreviousRecord, read_manifest, write_manifest};
use crate::render::tests::{
    deferred, dir_text, finding, first_manifest, fixed, included, reviewer, write_findings,
};
use crate::render::{FindingsInput, RECORD_FILE, RecordOutcome, run_render};
use crate::text::quote;

const RECORD_COMMENT_URL: &str = "https://github.com/shgew/togi/pull/700#issuecomment-5001";
const CHECK_RUN_URL: &str = "https://github.com/shgew/togi/runs/77";

/// Answers publish's gh calls and keeps what was posted.
#[derive(Default)]
struct FakeGitHub {
    /// `pr view` answers in turn.
    heads: Vec<&'static str>,
    /// The answer to the comment listing.
    comments: String,
    check_app: String,
    posted: Vec<&'static str>,
    comment_body: String,
    check_payload: String,
    views: usize,
}

impl FakeGitHub {
    fn handle(&mut self, args: &[String]) -> crate::process::Output {
        let arg = |i: usize| args.get(i).map_or("", String::as_str);
        if arg(0) == "pr" && arg(1) == "view" {
            let head = self.heads[self.views.min(self.heads.len() - 1)];
            self.views += 1;
            return Ok(format!(
                r#"{{"headRefOid":{},"baseRefOid":{},"url":{}}}"#,
                quote(head),
                quote(BASE_SHA),
                quote(PR_URL)
            )
            .into_bytes());
        }
        if arg(0) == "api" && args.iter().any(|a| a == "--paginate") {
            let comments = if self.comments.is_empty() {
                "[[]]"
            } else {
                &self.comments
            };
            return Ok(comments.as_bytes().to_vec());
        }
        if arg(0) == "api" && arg(3).ends_with("/issues/700/comments") {
            let Some(body) = arg(5).strip_prefix("body=@") else {
                return Err(fail(&format!("unexpected comment args {args:?}")));
            };
            self.comment_body = read_file(body);
            self.posted.push("comment");
            return Ok(json!({"html_url": RECORD_COMMENT_URL})
                .to_string()
                .into_bytes());
        }
        if arg(0) == "api" && arg(3).ends_with("/check-runs") {
            self.check_payload = read_file(arg(5));
            self.posted.push("check");
            let p: CheckPayload =
                serde_json::from_str(&self.check_payload).map_err(|e| fail(&e.to_string()))?;
            let slug = if self.check_app.is_empty() {
                "robotogi"
            } else {
                &self.check_app
            };
            let answer = json!({
                "html_url": CHECK_RUN_URL,
                "head_sha": p.head_sha,
                "app": {"slug": slug},
                "output": {"title": p.output.title, "summary": p.output.summary},
            });
            return Ok(answer.to_string().into_bytes());
        }
        Err(fail(&format!("unexpected gh {args:?}")))
    }
}

/// A harness whose gh is `github`.
fn harness(github: &Rc<RefCell<FakeGitHub>>) -> Harness {
    let github = Rc::clone(github);
    Harness::new(
        Some(Box::new(move |args| github.borrow_mut().handle(args))),
        None,
    )
}

fn github(heads: &[&'static str], comments: String) -> Rc<RefCell<FakeGitHub>> {
    Rc::new(RefCell::new(FakeGitHub {
        heads: heads.to_vec(),
        comments,
        ..FakeGitHub::default()
    }))
}

/// A manifest and its rendered record for a docs-only pull request, with the given finding outcome.
fn rendered_snapshot(outcome: RecordOutcome) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temporary directory");
    write_manifest(
        &dir_text(dir.path()),
        &first_manifest(vec![included("docs/howto.md", 5, 2)]),
    )
    .expect("write manifest");
    let input = FindingsInput {
        reviewers: vec![reviewer("reviewer-1", None, &["docs/howto.md"])],
        findings: vec![finding("reviewer-1", "P1", "x", None, outcome)],
        ..FindingsInput::default()
    };
    let file = write_findings(dir.path(), &serde_json::to_string(&input).expect("encode"));
    run_render(&mut Vec::new(), &[dir_text(dir.path()), file]).expect("render");
    dir
}

fn publish(h: &mut Harness, dir: &Path) -> Result<(), crate::Error> {
    run_publish(&mut h.tools(), &[dir_text(dir)])
}

fn record(dir: &Path) -> String {
    read_file(dir.join(RECORD_FILE))
}

fn comments(entries: &[(&str, &str, &str)]) -> String {
    let page: Vec<Value> = entries
        .iter()
        .map(|&(url, body, login)| json!({"html_url": url, "body": body, "user": {"login": login}}))
        .collect();
    json!([page]).to_string()
}

#[test]
fn publish_posts_record_and_check_on_the_reviewed_head() {
    let dir = rendered_snapshot(fixed());
    let gh = github(&[HEAD_SHA], String::new());
    let mut h = harness(&gh);
    publish(&mut h, dir.path()).expect("publish");
    let f = gh.borrow();
    assert_eq!(vec!["comment", "check"], f.posted);
    assert_eq!(
        record(dir.path()),
        f.comment_body,
        "the comment is not the rendered record"
    );
    let want_check = format!(
        r#"{{"name":"review","head_sha":"{HEAD_SHA}","status":"completed","conclusion":"success","output":{{"title":"Review recorded","summary":"Review record: {RECORD_COMMENT_URL}"}}}}"#
    );
    assert_eq!(want_check, f.check_payload);
    assert_eq!(
        format!("record {RECORD_COMMENT_URL}\ncheck {CHECK_RUN_URL}\n"),
        h.stdout()
    );
    let views = h
        .fake
        .gh_calls
        .iter()
        .filter(|args| args[0] == "pr")
        .count();
    assert_eq!(
        2, views,
        "the head is checked before posting and again after"
    );
}

#[test]
fn publish_refuses_moved_head() {
    let dir = rendered_snapshot(fixed());
    let gh = github(&[MOVED_SHA], String::new());
    let mut h = harness(&gh);
    let error = publish(&mut h, dir.path()).expect_err("publish accepted");
    assert!(
        error
            .to_string()
            .contains(&format!("not the snapshot's {HEAD_SHA}")),
        "error = {error}, want a refusal naming the snapshot's head"
    );
    assert!(
        gh.borrow().posted.is_empty() && h.stdout.is_empty(),
        "a refused publish posted or printed"
    );
}

#[test]
fn publish_warns_when_head_moves_after_posting() {
    let dir = rendered_snapshot(fixed());
    let gh = github(&[HEAD_SHA, MOVED_SHA], String::new());
    let mut h = harness(&gh);
    publish(&mut h, dir.path()).expect("publish");
    assert!(
        h.stdout().contains(RECORD_COMMENT_URL) && h.stderr().contains("needs its own review"),
        "stdout {:?}, stderr {:?}",
        h.stdout(),
        h.stderr()
    );
}

#[test]
fn publish_needs_one_token() {
    let dir = rendered_snapshot(fixed());
    let gh = github(&[HEAD_SHA], String::new());
    let mut h = harness(&gh);
    h.gh_token = String::new();
    let error = publish(&mut h, dir.path()).expect_err("publish accepted");
    assert!(
        error.to_string().contains("just as-bot"),
        "error = {error}, want the as-bot hint"
    );
    assert_eq!(0, gh.borrow().views, "publish called gh without a token");
}

#[test]
fn publish_blocked_posts_no_check() {
    let dir = rendered_snapshot(deferred(5));
    let gh = github(&[HEAD_SHA], String::new());
    let mut h = harness(&gh);
    publish(&mut h, dir.path()).expect("publish");
    assert_eq!(vec!["comment"], gh.borrow().posted);
    assert!(
        h.stdout().contains("blocked: no review check posted"),
        "output = {:?}",
        h.stdout()
    );
}

#[test]
fn publish_reuses_the_record_on_the_head() {
    let dir = rendered_snapshot(fixed());
    let existing = record(dir.path());
    let gh = github(
        &[HEAD_SHA],
        comments(&[
            ("https://example.test/other", "hello", "shgew"),
            (RECORD_COMMENT_URL, &existing, "robotogi[bot]"),
        ]),
    );
    let mut h = harness(&gh);
    publish(&mut h, dir.path()).expect("publish");
    assert_eq!(vec!["check"], gh.borrow().posted);
    assert!(
        h.stdout().contains(&format!("record {RECORD_COMMENT_URL}")),
        "output = {:?}",
        h.stdout()
    );
}

#[test]
fn publish_ignores_records_from_others() {
    let dir = rendered_snapshot(fixed());
    let existing = record(dir.path());
    let gh = github(
        &[HEAD_SHA],
        comments(&[("https://example.test/copy", &existing, "shgew")]),
    );
    let mut h = harness(&gh);
    publish(&mut h, dir.path()).expect("publish");
    assert_eq!(vec!["comment", "check"], gh.borrow().posted);
}

#[test]
fn publish_rejects_foreign_check() {
    let dir = rendered_snapshot(fixed());
    let gh = github(&[HEAD_SHA], String::new());
    "github-actions".clone_into(&mut gh.borrow_mut().check_app);
    let mut h = harness(&gh);
    let error = publish(&mut h, dir.path()).expect_err("publish accepted");
    assert!(
        error
            .to_string()
            .contains(r#"belongs to app "github-actions""#),
        "error = {error}, want a foreign-app refusal"
    );
}

#[test]
fn publish_needs_a_rendered_record_for_the_head() {
    let dir = tempfile::tempdir().expect("temporary directory");
    write_manifest(
        &dir_text(dir.path()),
        &first_manifest(vec![included("docs/x.md", 1, 0)]),
    )
    .expect("write manifest");
    let gh = github(&[HEAD_SHA], String::new());
    let error = publish(&mut harness(&gh), dir.path()).expect_err("publish accepted");
    assert!(
        error.to_string().contains("run render first"),
        "error = {error}, want a render hint"
    );

    let stale = rendered_snapshot(fixed());
    let stale_dir = dir_text(stale.path());
    let mut m = read_manifest(&stale_dir).expect("read manifest");
    MOVED_SHA.clone_into(&mut m.head_sha);
    write_manifest(&stale_dir, &m).expect("write manifest");
    let gh = github(&[MOVED_SHA], String::new());
    let error = publish(&mut harness(&gh), stale.path()).expect_err("publish accepted");
    assert!(
        error.to_string().contains("run render again"),
        "error = {error}, want a stale-record refusal"
    );
}

fn carry_snapshot(verdict: &str, patches: Vec<PatchStatus>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temporary directory");
    let mut m = first_manifest(vec![]);
    m.carry_forward = true;
    m.previous = Some(PreviousRecord {
        url: PREVIOUS_URL.to_owned(),
        head_sha: OLD_HEAD_SHA.to_owned(),
        base_sha: OLD_BASE_SHA.to_owned(),
        verdict: verdict.to_owned(),
    });
    m.patches = patches;
    write_manifest(&dir_text(dir.path()), &m).expect("write manifest");
    dir
}

fn unchanged(context_only: bool) -> PatchStatus {
    PatchStatus {
        status: "unchanged".to_owned(),
        context_only,
        ..PatchStatus::default()
    }
}

#[test]
fn publish_carries_forward() {
    let dir = carry_snapshot("success", vec![unchanged(false), unchanged(true)]);
    let gh = github(&[HEAD_SHA], String::new());
    let mut h = harness(&gh);
    publish(&mut h, dir.path()).expect("publish");
    let f = gh.borrow();
    assert_eq!(vec!["check"], f.posted);
    let p: CheckPayload = serde_json::from_str(&f.check_payload).expect("decode payload");
    let want = format!(
        "Review record: {PREVIOUS_URL} carried forward. git range-diff {OLD_BASE_SHA}..{OLD_HEAD_SHA} {BASE_SHA}..{HEAD_SHA} pairs all 2 patches unchanged, 1 of them with only context lines changed."
    );
    assert_eq!(
        (HEAD_SHA, want.as_str()),
        (p.head_sha.as_str(), p.output.summary.as_str())
    );
    assert!(
        h.stdout().contains(&format!("record {PREVIOUS_URL}")),
        "output = {:?}",
        h.stdout()
    );
}

#[test]
fn publish_refuses_carry_forward_of_a_prior_that_is_not_a_success() {
    for verdict in ["blocked", ""] {
        let dir = carry_snapshot(verdict, vec![unchanged(false)]);
        let gh = github(&[HEAD_SHA], String::new());
        let mut h = harness(&gh);
        let error = publish(&mut h, dir.path()).expect_err("publish accepted");
        assert!(
            error.to_string().contains("not recorded as a success"),
            "verdict {verdict:?}: error = {error}, want a carry-forward refusal"
        );
        assert!(
            gh.borrow().posted.is_empty() && h.stdout.is_empty(),
            "verdict {verdict:?}: a refused carry-forward posted or printed"
        );
    }
}

#[test]
fn publish_ignores_prefix_lookalike_bots() {
    let dir = rendered_snapshot(fixed());
    let existing = record(dir.path());
    let urls: Vec<String> = (0..3)
        .map(|i| format!("https://example.test/lookalike-{i}"))
        .collect();
    let gh = github(
        &[HEAD_SHA],
        comments(&[
            (&urls[0], &existing, "robotogi-helper"),
            (&urls[1], &existing, "robotogi"),
            (&urls[2], &existing, "robotogi[bot]x"),
        ]),
    );
    let mut h = harness(&gh);
    publish(&mut h, dir.path()).expect("publish");
    let f = gh.borrow();
    assert_eq!(vec!["comment", "check"], f.posted);
    assert!(
        !h.stdout().contains("example.test")
            && f.check_payload.contains(RECORD_COMMENT_URL)
            && !f.check_payload.contains("example.test"),
        "a lookalike's comment was linked: output {:?}, check {}",
        h.stdout(),
        f.check_payload
    );
}

#[test]
fn publish_refuses_an_existing_record_that_differs() {
    let blocked_dir = rendered_snapshot(deferred(5));
    let blocked = record(blocked_dir.path());
    let dir = rendered_snapshot(fixed());
    let extra = format!("{}\nextra visible text\n", record(dir.path()));
    for (name, body) in [("blocked record", &blocked), ("other data", &extra)] {
        let gh = github(
            &[HEAD_SHA],
            comments(&[(RECORD_COMMENT_URL, body, "robotogi[bot]")]),
        );
        let mut h = harness(&gh);
        let error = publish(&mut h, dir.path()).expect_err("publish accepted");
        assert!(
            error
                .to_string()
                .contains(&format!("differs from {RECORD_FILE}")),
            "{name}: error = {error}, want a mismatch refusal"
        );
        let f = gh.borrow();
        assert!(
            f.posted.is_empty() && f.check_payload.is_empty() && h.stdout.is_empty(),
            "{name}: a refused publish posted or printed"
        );
    }
}

#[test]
fn publish_reuses_an_identical_record_despite_line_endings() {
    let dir = rendered_snapshot(fixed());
    let body = record(dir.path()).replace('\n', "\r\n");
    let gh = github(
        &[HEAD_SHA],
        comments(&[(RECORD_COMMENT_URL, &body, "robotogi[bot]")]),
    );
    publish(&mut harness(&gh), dir.path()).expect("publish");
    assert_eq!(vec!["check"], gh.borrow().posted);
}

#[test]
fn publish_usage() {
    let mut h = Harness::new(None, None);
    let error = run_publish(&mut h.tools(), &[]).expect_err("publish accepted");
    assert!(
        error.to_string().contains("usage:"),
        "error = {error}, want usage"
    );
}

#[test]
fn dispatch_routes_subcommands() {
    let mut h = Harness::new(
        Some(crate::fakes::answer(
            r#"[{"data":{"repository":{"pullRequests":{"nodes":[]}}}}]"#,
        )),
        None,
    );
    for args in [&[][..], &["report"]] {
        h.dispatch(args)
            .unwrap_or_else(|error| panic!("dispatch({args:?}): {error}"));
    }
    for args in [&["report", "x"][..], &["bogus"]] {
        assert!(h.dispatch(args).is_err(), "dispatch({args:?}) succeeded");
    }
}
