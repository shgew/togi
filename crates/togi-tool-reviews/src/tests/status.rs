use std::io::Cursor;

use serde_json::{Value, json};
use test_case::test_case;

use crate::fakes::{BASE_SHA, Fake, HEAD_SHA, OLD_HEAD_SHA, PREVIOUS_URL, fail};
use crate::parts::split_record;
use crate::record::{RECORD_CLOSE, RECORD_OPEN};
use crate::{Error, Outcome, Tools, status};

/// Where a test cuts a GraphQL check connection short, so only REST has the rest.
#[derive(Clone, Copy)]
enum Truncation {
    Suites(&'static str),
    Runs(&'static str),
}

struct Fixture {
    pull: Value,
    comments: Vec<Value>,
    /// The `isResolved` flags of each review thread page, in page order.
    thread_pages: Vec<Vec<bool>>,
    behind: u64,
    /// The App whose review suite `pull` holds; GitHub hides its identity on the suite itself.
    reviews_app: i64,
    /// The runs each REST page holds once GitHub applies `filter=latest`.
    rest_pages: Vec<Vec<Value>>,
    /// Earlier attempts that only `filter=all` returns.
    superseded: Vec<Value>,
}

impl Fixture {
    fn ready() -> Self {
        Self {
            pull: json!({
                "number": 700, "title": "A pull request", "state": "OPEN",
                "isDraft": false, "headRefOid": HEAD_SHA, "baseRefName": "main",
                "baseRef": {"target": {"oid": BASE_SHA}}, "mergeable": "MERGEABLE",
                "commits": {"nodes": [{"commit": {
                    "checks": {"nodes": [{"runs": {
                        "nodes": [{"name": "check", "status": "COMPLETED", "conclusion": "SUCCESS"}]
                    }}]},
                    "reviews": {"nodes": [{"app": null, "runs": {
                        "nodes": [{"name": "review", "status": "COMPLETED", "conclusion": "SUCCESS"}]
                    }}]}
                }}]}
            }),
            comments: Vec::new(),
            thread_pages: vec![vec![true]],
            behind: 0,
            reviews_app: 5_162_510,
            rest_pages: Vec::new(),
            superseded: Vec::new(),
        }
    }

    fn draft(mut self) -> Self {
        self.pull["isDraft"] = json!(true);
        self
    }

    fn conflicts(mut self) -> Self {
        self.pull["mergeable"] = json!("CONFLICTING");
        self
    }

    fn check(mut self, status: &str, conclusion: Option<&str>) -> Self {
        self.pull["commits"]["nodes"][0]["commit"]["checks"]["nodes"][0]["runs"]["nodes"] =
            json!([{"name": "check", "status": status, "conclusion": conclusion}]);
        self
    }

    fn missing_check(mut self) -> Self {
        self.pull["commits"]["nodes"][0]["commit"]["checks"]["nodes"] = json!([]);
        self
    }

    fn unreviewed(mut self) -> Self {
        self.pull["commits"]["nodes"][0]["commit"]["reviews"]["nodes"] = json!([]);
        self
    }

    /// Keeps robotogi's review suite but leaves its run for the REST pages.
    fn review_run_cut(mut self) -> Self {
        self.pull["commits"]["nodes"][0]["commit"]["reviews"]["nodes"][0]["runs"]["nodes"] =
            json!([]);
        self
    }

    /// The review check belongs to another App, so the server's `appId` filter returns it only for that
    /// App's id.
    fn other_app(mut self) -> Self {
        self.reviews_app = 99;
        self
    }

    fn record(mut self, head: &str) -> Self {
        self.comments.push(comment(
            &record_body(head),
            PREVIOUS_URL,
            "2026-10-01T00:00:00Z",
        ));
        self
    }

    /// Two unresolved threads, on the first and the last of three pages.
    fn unresolved(self) -> Self {
        self.thread_pages(vec![vec![false], vec![true], vec![false]])
    }

    fn thread_pages(mut self, pages: Vec<Vec<bool>>) -> Self {
        self.thread_pages = pages;
        self
    }

    fn behind(mut self) -> Self {
        self.behind = 1;
        self
    }

    fn truncated(mut self, truncation: Truncation) -> Self {
        let commit = &mut self.pull["commits"]["nodes"][0]["commit"];
        let page_info = json!({"hasNextPage": true});
        match truncation {
            Truncation::Suites(connection) => commit[connection]["pageInfo"] = page_info,
            Truncation::Runs(connection) => {
                commit[connection]["nodes"][0]["runs"]["pageInfo"] = page_info;
            }
        }
        self
    }

    fn rest(mut self, pages: Vec<Vec<Value>>) -> Self {
        self.rest_pages = pages;
        self
    }

    fn superseded(mut self, runs: Vec<Value>) -> Self {
        self.superseded = runs;
        self
    }

    fn fake(self) -> Fake {
        Fake::new(
            Some(Box::new(move |args| {
                let query = args.iter().find(|arg| arg.starts_with("query="));
                let result = if let Some(query) = query {
                    assert!(args.iter().any(|a| a == "--paginate"));
                    assert!(args.iter().any(|a| a == "--slurp"));
                    if query.contains("reviewThreads(") {
                        let last = self.thread_pages.len() - 1;
                        Value::Array(
                            self.thread_pages
                                .iter()
                                .enumerate()
                                .map(|(page, resolved)| {
                                    let nodes: Vec<Value> = resolved
                                        .iter()
                                        .map(|resolved| json!({"isResolved": resolved}))
                                        .collect();
                                    json!({"data": {"repository": {"pullRequest": {"reviewThreads": {
                                        "nodes": nodes, "pageInfo": {"hasNextPage": page < last}
                                    }}}}})
                                })
                                .collect(),
                        )
                    } else {
                        assert!(query.contains("filterBy: {appId: $appId}"));
                        assert_eq!(
                            query.matches("checkType: LATEST").count(),
                            2,
                            "both check connections keep only the latest attempt"
                        );
                        assert!(query.contains("checkName: \"check\""));
                        assert!(query.contains("checkName: \"review\""));
                        assert!(args.iter().any(|a| a == "appId=5162510"));
                        let mut pull = self.pull.clone();
                        if !args.contains(&format!("appId={}", self.reviews_app)) {
                            pull["commits"]["nodes"][0]["commit"]["reviews"]["nodes"] = json!([]);
                        }
                        if args.iter().any(|a| a == "number=700") {
                            json!([{"data": {"repository": {"pullRequest": pull}}}])
                        } else {
                            assert!(query.contains("states: OPEN"));
                            json!([{"data": {"repository": {"pullRequests": {"nodes": [pull]}}}}])
                        }
                    }
                } else if args.iter().any(|a| a.contains("/comments")) {
                    json!([self.comments])
                } else if args.iter().any(|a| a.contains("/compare/")) {
                    assert!(
                        args.iter()
                            .any(|a| a.ends_with(&format!("{BASE_SHA}...{HEAD_SHA}")))
                    );
                    json!({"behind_by": self.behind})
                } else if let Some(url) = args.iter().find(|a| a.contains("/check-runs")) {
                    // GitHub returns superseded attempts only to `filter=all`.
                    let mut pages: Vec<Vec<Value>> = self.rest_pages.clone();
                    if url.contains("filter=all") {
                        pages.push(self.superseded.clone());
                    } else {
                        assert!(url.contains("filter=latest"), "unexpected filter in {url}");
                    }
                    Value::Array(
                        pages
                            .into_iter()
                            .map(|runs| json!({"check_runs": runs}))
                            .collect(),
                    )
                } else {
                    panic!("unexpected gh call: {args:?}");
                };
                Ok(serde_json::to_vec(&result).unwrap())
            })),
            None,
        )
    }
}

/// Runs `reviews status <args>` against `fake` with captured output.
fn run_status(fake: &mut Fake, args: &[&str]) -> (Result<Outcome, Error>, String) {
    let args: Vec<String> = args.iter().map(|&arg| arg.to_owned()).collect();
    let mut stdout = Vec::new();
    let result = status::run(fake, &mut stdout, &args);
    (result, String::from_utf8(stdout).unwrap())
}

/// One pull request's status lines under the shared title line.
fn pull_text(lines: &str) -> String {
    format!("  #700 A pull request\n{lines}")
}

fn rest_run(name: &str, conclusion: &str, app: i64) -> Value {
    json!({"name": name, "status": "completed", "conclusion": conclusion, "app": {"id": app}})
}

fn gh_requested(fake: &Fake, needle: &str) -> bool {
    fake.gh_calls
        .iter()
        .any(|args| args.iter().any(|a| a.contains(needle)))
}

fn record_body(head: &str) -> String {
    let record = json!({"version": 3, "head_sha": head, "findings": []});
    format!("{RECORD_OPEN}{record}{RECORD_CLOSE}")
}

fn comment(body: &str, url: &str, created: &str) -> Value {
    json!({
        "user": {"login": "robotogi[bot]"}, "body": body,
        "html_url": url, "created_at": created
    })
}

#[test_case(Fixture::ready().draft(), "    next: draft\n    review: done\n", Outcome::NotReady; "draft reviewed")]
#[test_case(Fixture::ready().draft().unreviewed(), "    next: draft\n    review: needs review\n", Outcome::NotReady; "draft unreviewed")]
#[test_case(Fixture::ready().draft().unreviewed().record(HEAD_SHA), "    next: draft\n    review: needs review: the record on this head has no passing review check\n", Outcome::NotReady; "draft record without check")]
#[test_case(Fixture::ready().conflicts(), "    next: conflicts with main\n    review: done\n", Outcome::NotReady; "conflicts reviewed")]
#[test_case(Fixture::ready().conflicts().unreviewed().record(OLD_HEAD_SHA), &format!("    next: conflicts with main\n    review: carry forward or review the delta since {PREVIOUS_URL}\n"), Outcome::NotReady; "conflicts carry forward")]
#[test_case(Fixture::ready().check("COMPLETED", Some("FAILURE")), "    next: check failed on aaaaaaa\n    review: done\n", Outcome::NotReady; "failed check reviewed")]
#[test_case(Fixture::ready().check("COMPLETED", Some("FAILURE")).unreviewed(), "    next: check failed on aaaaaaa\n    review: needs review\n", Outcome::NotReady; "failed check unreviewed")]
#[test_case(Fixture::ready().unreviewed(), "    next: needs review\n", Outcome::NotReady; "needs review")]
#[test_case(Fixture::ready().unreviewed().record(OLD_HEAD_SHA), &format!("    next: carry forward or review the delta since {PREVIOUS_URL}\n"), Outcome::NotReady; "carry forward")]
#[test_case(Fixture::ready().unresolved(), "    next: 2 unresolved review threads\n", Outcome::NotReady; "threads")]
#[test_case(Fixture::ready().behind(), "    next: behind main: just push-fix, then carry the review forward\n", Outcome::NotReady; "behind")]
#[test_case(Fixture::ready().check("IN_PROGRESS", None), "    next: check running\n", Outcome::Waiting; "running")]
#[test_case(Fixture::ready(), "    next: ready to merge\n", Outcome::Success; "ready")]
fn states_and_single_pull_exit_outcomes(fixture: Fixture, lines: &str, outcome: Outcome) {
    let mut fake = fixture.fake();
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert_eq!(result.unwrap(), outcome);
    assert_eq!(stdout, pull_text(lines));
}

#[test_case(Fixture::ready().draft().conflicts(), "    next: draft\n    review: done\n"; "draft over conflicts")]
#[test_case(Fixture::ready().conflicts().check("COMPLETED", Some("FAILURE")), "    next: conflicts with main\n    review: done\n"; "conflicts over failed check")]
#[test_case(Fixture::ready().check("COMPLETED", Some("FAILURE")).unreviewed(), "    next: check failed on aaaaaaa\n    review: needs review\n"; "failed check over needs review")]
#[test_case(Fixture::ready().unreviewed().unresolved(), "    next: needs review\n"; "needs review over threads")]
#[test_case(Fixture::ready().unreviewed().record(OLD_HEAD_SHA).unresolved(), &format!("    next: carry forward or review the delta since {PREVIOUS_URL}\n"); "carry forward over threads")]
#[test_case(Fixture::ready().unresolved().behind(), "    next: 2 unresolved review threads\n"; "threads over behind")]
#[test_case(Fixture::ready().behind().check("QUEUED", None), "    next: behind main: just push-fix, then carry the review forward\n"; "behind over running")]
#[test_case(Fixture::ready().check("QUEUED", None), "    next: check running\n"; "running over ready")]
fn first_matching_gate_wins(fixture: Fixture, lines: &str) {
    let mut fake = fixture.fake();
    let (result, stdout) = run_status(&mut fake, &[]);
    assert_eq!(result.unwrap(), Outcome::Success);
    assert_eq!(stdout, format!("Pull requests\n{}", pull_text(lines)));
}

#[test_case(Outcome::Success, 0; "success")]
#[test_case(Outcome::Waiting, 3; "waiting")]
#[test_case(Outcome::NotReady, 4; "not ready")]
fn outcomes_map_to_exit_codes(outcome: Outcome, code: u8) {
    assert_eq!(outcome.exit_code(), code);
}

#[test]
fn dispatch_routes_status_with_its_arguments() {
    let mut fake = Fixture::ready().fake();
    let mut stdin = Cursor::new(Vec::new());
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    let mut tools = Tools {
        programs: &mut fake,
        gh_token: String::new(),
        tmp: String::new(),
        stdin: &mut stdin,
        stdout: &mut stdout,
        stderr: &mut stderr,
    };
    let args = ["status".to_owned(), "700".to_owned()];
    assert_eq!(
        crate::dispatch(&mut tools, &args).unwrap(),
        Outcome::Success
    );
    assert_eq!(
        String::from_utf8(stdout).unwrap(),
        pull_text("    next: ready to merge\n")
    );
}

#[test_case(Fixture::ready().missing_check(); "absent")]
#[test_case(Fixture::ready().check("QUEUED", None); "queued")]
#[test_case(Fixture::ready().check("IN_PROGRESS", None); "in progress")]
#[test_case(Fixture::ready().check("WAITING", None); "waiting")]
fn incomplete_checks_wait(fixture: Fixture) {
    let mut fake = fixture.fake();
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert_eq!(result.unwrap(), Outcome::Waiting);
    assert_eq!(stdout, pull_text("    next: check running\n"));
}

#[test_case("CANCELLED"; "cancelled")]
#[test_case("TIMED_OUT"; "timed out")]
#[test_case("NEUTRAL"; "neutral")]
#[test_case("SKIPPED"; "skipped")]
#[test_case("ACTION_REQUIRED"; "action required")]
fn completed_checks_require_success(conclusion: &str) {
    let mut fake = Fixture::ready().check("COMPLETED", Some(conclusion)).fake();
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert_eq!(result.unwrap(), Outcome::NotReady);
    assert_eq!(
        stdout,
        pull_text("    next: check failed on aaaaaaa\n    review: done\n")
    );
}

#[test]
fn carried_forward_review_check_counts_without_reading_old_records() {
    let mut fake = Fixture::ready().record(OLD_HEAD_SHA).fake();
    let (result, _) = run_status(&mut fake, &["700"]);
    assert_eq!(result.unwrap(), Outcome::Success);
    assert!(!gh_requested(&fake, "/comments"));
}

#[test]
fn another_apps_review_does_not_count() {
    let mut fake = Fixture::ready().other_app().fake();
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert_eq!(result.unwrap(), Outcome::NotReady);
    assert_eq!(stdout, pull_text("    next: needs review\n"));
}

#[test]
fn record_on_head_without_passing_check_needs_review() {
    let mut fake = Fixture::ready().unreviewed().record(HEAD_SHA).fake();
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert_eq!(result.unwrap(), Outcome::NotReady);
    assert_eq!(
        stdout,
        pull_text("    next: needs review: the record on this head has no passing review check\n")
    );
}

#[test]
fn record_on_head_wins_over_a_newer_record_on_another_head() {
    let mut fixture = Fixture::ready().unreviewed().record(HEAD_SHA);
    fixture.comments.push(comment(
        &record_body(OLD_HEAD_SHA),
        "newer",
        "2026-10-02T00:00:00Z",
    ));
    let mut fake = fixture.fake();
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert_eq!(result.unwrap(), Outcome::NotReady);
    assert_eq!(
        stdout,
        pull_text("    next: needs review: the record on this head has no passing review check\n")
    );
}

#[test]
fn carry_forward_uses_latest_complete_records_url() {
    let mut fixture = Fixture::ready().unreviewed().record(OLD_HEAD_SHA);
    fixture.comments.push(comment(
        &record_body(BASE_SHA),
        "latest-record",
        "2026-10-02T00:00:00Z",
    ));
    let mut fake = fixture.fake();
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert_eq!(result.unwrap(), Outcome::NotReady);
    assert_eq!(
        stdout,
        pull_text("    next: carry forward or review the delta since latest-record\n")
    );
}

#[test_case(true, "needs review: the record on this head has no passing review check"; "complete")]
#[test_case(false, "needs review"; "missing last part")]
fn split_records_are_joined_before_classification(complete: bool, label: &str) {
    let body = format!(
        "{}\n{}",
        "Review details\n".repeat(100),
        record_body(HEAD_SHA)
    );
    let mut parts = split_record(&body, HEAD_SHA, 500).unwrap();
    assert!(parts.len() > 1);
    if !complete {
        parts.pop();
    }
    let mut fixture = Fixture::ready().unreviewed();
    fixture.comments = parts
        .iter()
        .enumerate()
        .map(|(i, body)| comment(body, &format!("part-{i}"), "2026-10-01T00:00:00Z"))
        .collect();
    let mut fake = fixture.fake();
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert_eq!(result.unwrap(), Outcome::NotReady);
    assert_eq!(stdout, pull_text(&format!("    next: {label}\n")));
}

#[test_case("robotogi"; "ordinary login")]
#[test_case("robotogi-helper[bot]"; "prefix login")]
fn only_exact_robotogi_bot_comments_are_records(login: &str) {
    let mut fixture = Fixture::ready().unreviewed().record(HEAD_SHA);
    fixture.comments[0]["user"]["login"] = json!(login);
    let mut fake = fixture.fake();
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert_eq!(result.unwrap(), Outcome::NotReady);
    assert_eq!(stdout, pull_text("    next: needs review\n"));
}

#[test]
fn robotogi_comment_without_a_record_block_is_skipped() {
    let mut fixture = Fixture::ready().unreviewed().record(OLD_HEAD_SHA);
    fixture.comments.push(comment(
        "Thanks for the update.",
        "plain-comment",
        "2026-10-02T00:00:00Z",
    ));
    let mut fake = fixture.fake();
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert_eq!(result.unwrap(), Outcome::NotReady);
    assert_eq!(
        stdout,
        pull_text(&format!(
            "    next: carry forward or review the delta since {PREVIOUS_URL}\n"
        ))
    );
}

#[test]
fn malformed_record_block_fails_the_command() {
    let mut fixture = Fixture::ready().unreviewed();
    fixture.comments.push(comment(
        &format!("{RECORD_OPEN}not json{RECORD_CLOSE}"),
        PREVIOUS_URL,
        "2026-10-01T00:00:00Z",
    ));
    let mut fake = fixture.fake();
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert!(
        result
            .unwrap_err()
            .render()
            .starts_with("parse review record: ")
    );
    assert_eq!(stdout, "");
}

#[test]
fn unresolved_threads_are_counted_across_all_pages() {
    let mut fake = Fixture::ready().unresolved().fake();
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert_eq!(result.unwrap(), Outcome::NotReady);
    assert_eq!(stdout, pull_text("    next: 2 unresolved review threads\n"));
}

#[test]
fn an_unresolved_thread_on_an_earlier_page_blocks_a_clean_last_page() {
    let mut fake = Fixture::ready()
        .thread_pages(vec![vec![true, false], vec![true]])
        .fake();
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert_eq!(result.unwrap(), Outcome::NotReady);
    assert_eq!(stdout, pull_text("    next: 1 unresolved review threads\n"));
}

#[test_case(Truncation::Suites("checks"); "check suites")]
#[test_case(Truncation::Runs("checks"); "check runs")]
fn a_failed_check_only_on_a_later_page_fails(truncation: Truncation) {
    let mut fake = Fixture::ready()
        .truncated(truncation)
        .rest(vec![
            vec![rest_run("check", "success", 1)],
            vec![
                rest_run("check", "failure", 1),
                rest_run("review", "success", 5_162_510),
            ],
        ])
        .fake();
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert_eq!(result.unwrap(), Outcome::NotReady);
    assert_eq!(
        stdout,
        pull_text("    next: check failed on aaaaaaa\n    review: done\n")
    );
    assert!(gh_requested(&fake, "/check-runs"));
}

#[test_case(Truncation::Suites("reviews"); "review suites")]
#[test_case(Truncation::Runs("reviews"); "review runs")]
fn a_passing_review_only_on_a_later_page_counts(truncation: Truncation) {
    let mut fake = Fixture::ready()
        .review_run_cut()
        .truncated(truncation)
        .rest(vec![
            vec![rest_run("check", "success", 1)],
            vec![rest_run("review", "success", 5_162_510)],
        ])
        .fake();
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert_eq!(result.unwrap(), Outcome::Success);
    assert_eq!(stdout, pull_text("    next: ready to merge\n"));
    assert!(gh_requested(&fake, "/check-runs"));
}

#[test]
fn a_later_page_review_from_another_app_does_not_count() {
    let mut fake = Fixture::ready()
        .review_run_cut()
        .truncated(Truncation::Suites("reviews"))
        .rest(vec![vec![
            rest_run("check", "success", 1),
            rest_run("review", "success", 99),
        ]])
        .fake();
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert_eq!(result.unwrap(), Outcome::NotReady);
    assert_eq!(stdout, pull_text("    next: needs review\n"));
}

#[test]
fn complete_connections_never_request_rest_checks() {
    let mut fake = Fixture::ready().fake();
    let (result, _) = run_status(&mut fake, &["700"]);
    assert_eq!(result.unwrap(), Outcome::Success);
    assert!(!gh_requested(&fake, "/check-runs"));
}

#[test]
fn only_the_latest_attempt_of_a_check_counts() {
    // The earlier failed attempt is superseded by a passing rerun; `filter=all` would return both.
    let mut fake = Fixture::ready()
        .truncated(Truncation::Suites("checks"))
        .rest(vec![vec![
            rest_run("check", "success", 1),
            rest_run("review", "success", 5_162_510),
        ]])
        .superseded(vec![rest_run("check", "failure", 1)])
        .fake();
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert_eq!(result.unwrap(), Outcome::Success);
    assert_eq!(stdout, pull_text("    next: ready to merge\n"));
    assert!(gh_requested(&fake, "filter=latest"));
    assert!(!gh_requested(&fake, "filter=all"));
}

#[test]
fn a_superseded_failure_does_not_hide_a_running_rerun() {
    let mut fake = Fixture::ready()
        .truncated(Truncation::Suites("checks"))
        .rest(vec![vec![
            json!({"name": "check", "status": "in_progress", "conclusion": null, "app": {"id": 1}}),
            rest_run("review", "success", 5_162_510),
        ]])
        .superseded(vec![rest_run("check", "failure", 1)])
        .fake();
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert_eq!(result.unwrap(), Outcome::Waiting);
    assert_eq!(stdout, pull_text("    next: check running\n"));
}

#[test]
fn empty_open_pull_section() {
    let mut fake = Fake::new(
        Some(Box::new(|_| {
            Ok(serde_json::to_vec(&json!([
                {"data": {"repository": {"pullRequests": {"nodes": []}}}}
            ]))
            .unwrap())
        })),
        None,
    );
    let (result, stdout) = run_status(&mut fake, &[]);
    assert_eq!(result.unwrap(), Outcome::Success);
    assert_eq!(stdout, "Pull requests\n  none\n");
}

#[test]
fn list_sorts_every_page_by_pull_number() {
    let later = Fixture::ready().draft().pull;
    let mut earlier = Fixture::ready().draft().pull;
    earlier["number"] = json!(100);
    earlier["title"] = json!("Earlier pull request");
    let mut fake = Fake::new(
        Some(Box::new(move |_| {
            Ok(serde_json::to_vec(&json!([
                {"data": {"repository": {"pullRequests": {"nodes": [later]}}}},
                {"data": {"repository": {"pullRequests": {"nodes": [earlier]}}}}
            ]))
            .unwrap())
        })),
        None,
    );
    let (result, stdout) = run_status(&mut fake, &[]);
    assert_eq!(result.unwrap(), Outcome::Success);
    assert_eq!(
        stdout,
        "Pull requests\n  #100 Earlier pull request\n    next: draft\n    review: done\n  #700 A pull request\n    next: draft\n    review: done\n"
    );
}

#[test_case(&["0"]; "zero")]
#[test_case(&["-1"]; "negative")]
#[test_case(&["abc"]; "not a number")]
#[test_case(&["700", "extra"]; "extra argument")]
fn bad_usage_is_an_error(args: &[&str]) {
    let mut fake = Fake::new(None, None);
    let (result, stdout) = run_status(&mut fake, args);
    assert_eq!(result.unwrap_err().render(), "usage: reviews status [N]");
    assert_eq!(stdout, "");
}

#[test_case("CLOSED"; "closed")]
#[test_case("MERGED"; "merged")]
fn non_open_pulls_are_errors(state: &str) {
    let mut fixture = Fixture::ready();
    fixture.pull["state"] = json!(state);
    let mut fake = fixture.fake();
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert!(result.is_err());
    assert_eq!(stdout, "");
}

#[test_case("[{\"data\":{\"repository\":{\"pullRequest\":null}}}]"; "unknown pull")]
#[test_case("not json"; "invalid response")]
fn invalid_pull_responses_are_errors(response: &'static str) {
    let mut fake = Fake::new(
        Some(Box::new(move |_| Ok(response.as_bytes().to_vec()))),
        None,
    );
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert!(result.is_err());
    assert_eq!(stdout, "");
}

#[test]
fn github_failure_keeps_the_cause() {
    let mut fake = Fake::new(Some(Box::new(|_| Err(fail("not authenticated")))), None);
    let (result, stdout) = run_status(&mut fake, &[]);
    assert_eq!(
        result.unwrap_err().render(),
        "fetch open pull requests: not authenticated"
    );
    assert_eq!(stdout, "");
}

#[test_case("COMPLETED", Some("FAILURE"); "failed")]
#[test_case("IN_PROGRESS", None; "running")]
#[test_case("COMPLETED", None; "no conclusion")]
fn review_requires_a_completed_success(status: &str, conclusion: Option<&str>) {
    let mut fixture = Fixture::ready();
    let review = &mut fixture.pull["commits"]["nodes"][0]["commit"]["reviews"]["nodes"][0]["runs"]
        ["nodes"][0];
    review["status"] = json!(status);
    review["conclusion"] = json!(conclusion);
    let mut fake = fixture.fake();
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert_eq!(result.unwrap(), Outcome::NotReady);
    assert_eq!(stdout, pull_text("    next: needs review\n"));
}

#[test_case(Fixture::ready().unreviewed(), "/comments"; "records")]
#[test_case(Fixture::ready().draft().unreviewed(), "/comments"; "records of a draft")]
#[test_case(Fixture::ready(), "reviewThreads("; "threads")]
#[test_case(Fixture::ready(), "/compare/"; "comparison")]
fn downstream_github_failures_are_errors(fixture: Fixture, target: &'static str) {
    let mut fake = fixture.fake();
    let mut handler = fake.gh.take().unwrap();
    fake.gh = Some(Box::new(move |args| {
        if args.iter().any(|arg| arg.contains(target)) {
            Err(fail("request failed"))
        } else {
            handler(args)
        }
    }));
    let (result, stdout) = run_status(&mut fake, &["700"]);
    assert!(result.is_err());
    assert_eq!(stdout, "");
}
