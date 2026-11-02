use serde_json::{Value, json};
use test_case::test_case;

use crate::Outcome;
use crate::fakes::{BASE_SHA, HEAD_SHA, Harness, OLD_HEAD_SHA, PREVIOUS_URL, fail};
use crate::parts::split_record;
use crate::record::{RECORD_CLOSE, RECORD_OPEN};

struct Fixture {
    pull: Value,
    comments: Vec<Value>,
    threads: Vec<Value>,
    behind: u64,
    rest_checks: Option<Value>,
}

impl Fixture {
    fn ready() -> Self {
        Self {
            pull: json!({
                "number": 700, "title": "A pull request", "state": "OPEN",
                "isDraft": false, "headRefOid": HEAD_SHA, "baseRefName": "main",
                "baseRef": {"target": {"oid": BASE_SHA}}, "mergeable": "MERGEABLE",
                "commits": {"nodes": [{"commit": {
                    "checks": {"nodes": [{"app": {"databaseId": 1}, "runs": {
                        "nodes": [{"name": "check", "status": "COMPLETED", "conclusion": "SUCCESS"}]
                    }}]},
                    "reviews": {"nodes": [{"app": {"databaseId": 5_162_510}, "runs": {
                        "nodes": [{"name": "review", "status": "COMPLETED", "conclusion": "SUCCESS"}]
                    }}]}
                }}]}
            }),
            comments: Vec::new(),
            threads: vec![json!({"isResolved": true})],
            behind: 0,
            rest_checks: None,
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

    fn other_app(mut self) -> Self {
        self.pull["commits"]["nodes"][0]["commit"]["reviews"]["nodes"][0]["app"]["databaseId"] =
            json!(99);
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

    fn unresolved(mut self) -> Self {
        self.threads.push(json!({"isResolved": false}));
        self.threads.push(json!({"isResolved": false}));
        self
    }

    fn behind(mut self) -> Self {
        self.behind = 1;
        self
    }

    fn harness(self) -> Harness {
        Harness::new(
            Some(Box::new(move |args| {
                let query = args.iter().find(|arg| arg.starts_with("query="));
                let result = if let Some(query) = query {
                    assert!(args.iter().any(|a| a == "--paginate"));
                    assert!(args.iter().any(|a| a == "--slurp"));
                    if query.contains("reviewThreads(") {
                        // A resolved thread on page one does not hide unresolved threads on page two.
                        json!([
                            {"data": {"repository": {"pullRequest": {"reviewThreads": {
                                "nodes": [{"isResolved": true}], "pageInfo": {"hasNextPage": true}
                            }}}}},
                            {"data": {"repository": {"pullRequest": {"reviewThreads": {
                                "nodes": self.threads
                            }}}}}
                        ])
                    } else {
                        assert!(query.contains("filterBy: {appId: $appId}"));
                        assert!(query.contains("checkName: \"check\""));
                        assert!(query.contains("checkName: \"review\""));
                        assert!(args.iter().any(|a| a == "appId=5162510"));
                        if args.iter().any(|a| a == "number=700") {
                            json!([{"data": {"repository": {"pullRequest": self.pull}}}])
                        } else {
                            assert!(query.contains("states: OPEN"));
                            json!([{"data": {"repository": {"pullRequests": {"nodes": [self.pull]}}}}])
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
                } else if args.iter().any(|a| a.contains("/check-runs")) {
                    self.rest_checks.clone().expect("REST check pages")
                } else {
                    panic!("unexpected gh call: {args:?}");
                };
                Ok(serde_json::to_vec(&result).unwrap())
            })),
            None,
        )
    }
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

#[test_case(Fixture::ready().draft(), "draft", Outcome::NotReady; "draft")]
#[test_case(Fixture::ready().conflicts(), "conflicts with main", Outcome::NotReady; "conflicts")]
#[test_case(Fixture::ready().check("COMPLETED", Some("FAILURE")), "check failed on aaaaaaa", Outcome::NotReady; "failed check")]
#[test_case(Fixture::ready().unreviewed(), "needs review", Outcome::NotReady; "needs review")]
#[test_case(Fixture::ready().unreviewed().record(OLD_HEAD_SHA), &format!("carry forward or review the delta since {PREVIOUS_URL}"), Outcome::NotReady; "carry forward")]
#[test_case(Fixture::ready().unresolved(), "2 unresolved review threads", Outcome::NotReady; "threads")]
#[test_case(Fixture::ready().behind(), "behind main: just push-fix, then carry the review forward", Outcome::NotReady; "behind")]
#[test_case(Fixture::ready().check("IN_PROGRESS", None), "check running", Outcome::Waiting; "running")]
#[test_case(Fixture::ready(), "ready to merge", Outcome::Success; "ready")]
fn states_and_single_pull_exit_outcomes(fixture: Fixture, label: &str, outcome: Outcome) {
    let mut h = fixture.harness();
    assert_eq!(h.dispatch(&["status", "700"]).unwrap(), outcome);
    assert_eq!(
        h.stdout(),
        format!("  #700 A pull request\n    next: {label}\n")
    );
}

#[test_case(Fixture::ready().draft().conflicts(), "draft"; "draft over conflicts")]
#[test_case(Fixture::ready().conflicts().check("COMPLETED", Some("FAILURE")), "conflicts with main"; "conflicts over failed check")]
#[test_case(Fixture::ready().check("COMPLETED", Some("FAILURE")).unreviewed(), "check failed on aaaaaaa"; "failed check over needs review")]
#[test_case(Fixture::ready().unreviewed().unresolved(), "needs review"; "needs review over threads")]
#[test_case(Fixture::ready().unreviewed().record(OLD_HEAD_SHA).unresolved(), &format!("carry forward or review the delta since {PREVIOUS_URL}"); "carry forward over threads")]
#[test_case(Fixture::ready().unresolved().behind(), "2 unresolved review threads"; "threads over behind")]
#[test_case(Fixture::ready().behind().check("QUEUED", None), "behind main: just push-fix, then carry the review forward"; "behind over running")]
#[test_case(Fixture::ready().check("QUEUED", None), "check running"; "running over ready")]
fn first_matching_gate_wins(fixture: Fixture, label: &str) {
    let mut h = fixture.harness();
    assert_eq!(h.dispatch(&["status"]).unwrap(), Outcome::Success);
    assert_eq!(
        h.stdout(),
        format!("Pull requests\n  #700 A pull request\n    next: {label}\n")
    );
}

#[test_case(Fixture::ready().missing_check(); "absent")]
#[test_case(Fixture::ready().check("QUEUED", None); "queued")]
#[test_case(Fixture::ready().check("IN_PROGRESS", None); "in progress")]
#[test_case(Fixture::ready().check("WAITING", None); "waiting")]
fn incomplete_checks_wait(fixture: Fixture) {
    let mut h = fixture.harness();
    assert_eq!(h.dispatch(&["status", "700"]).unwrap(), Outcome::Waiting);
    assert_eq!(
        h.stdout(),
        "  #700 A pull request\n    next: check running\n"
    );
}

#[test_case("CANCELLED"; "cancelled")]
#[test_case("TIMED_OUT"; "timed out")]
#[test_case("NEUTRAL"; "neutral")]
#[test_case("SKIPPED"; "skipped")]
#[test_case("ACTION_REQUIRED"; "action required")]
fn completed_checks_require_success(conclusion: &str) {
    let mut h = Fixture::ready()
        .check("COMPLETED", Some(conclusion))
        .harness();
    assert_eq!(h.dispatch(&["status", "700"]).unwrap(), Outcome::NotReady);
    assert_eq!(
        h.stdout(),
        "  #700 A pull request\n    next: check failed on aaaaaaa\n"
    );
}

#[test]
fn carried_forward_review_check_counts_without_reading_old_records() {
    let mut h = Fixture::ready().record(OLD_HEAD_SHA).harness();
    assert_eq!(h.dispatch(&["status", "700"]).unwrap(), Outcome::Success);
    assert!(
        !h.fake
            .gh_calls
            .iter()
            .any(|args| args.iter().any(|a| a.contains("/comments")))
    );
}

#[test]
fn another_apps_review_does_not_count() {
    let mut h = Fixture::ready().other_app().harness();
    assert_eq!(h.dispatch(&["status", "700"]).unwrap(), Outcome::NotReady);
    assert_eq!(
        h.stdout(),
        "  #700 A pull request\n    next: needs review\n"
    );
}

#[test]
fn record_on_head_without_passing_check_needs_review() {
    let mut h = Fixture::ready().unreviewed().record(HEAD_SHA).harness();
    assert_eq!(h.dispatch(&["status", "700"]).unwrap(), Outcome::NotReady);
    assert_eq!(
        h.stdout(),
        "  #700 A pull request\n    next: needs review: the record on this head has no passing review check\n"
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
    let mut h = fixture.harness();
    assert_eq!(h.dispatch(&["status", "700"]).unwrap(), Outcome::NotReady);
    assert_eq!(
        h.stdout(),
        "  #700 A pull request\n    next: needs review: the record on this head has no passing review check\n"
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
    let mut h = fixture.harness();
    assert_eq!(h.dispatch(&["status", "700"]).unwrap(), Outcome::NotReady);
    assert_eq!(
        h.stdout(),
        "  #700 A pull request\n    next: carry forward or review the delta since latest-record\n"
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
    let mut h = fixture.harness();
    assert_eq!(h.dispatch(&["status", "700"]).unwrap(), Outcome::NotReady);
    assert_eq!(
        h.stdout(),
        format!("  #700 A pull request\n    next: {label}\n")
    );
}

#[test_case("robotogi"; "ordinary login")]
#[test_case("robotogi-helper[bot]"; "prefix login")]
fn only_exact_robotogi_bot_comments_are_records(login: &str) {
    let mut fixture = Fixture::ready().unreviewed().record(HEAD_SHA);
    fixture.comments[0]["user"]["login"] = json!(login);
    let mut h = fixture.harness();
    assert_eq!(h.dispatch(&["status", "700"]).unwrap(), Outcome::NotReady);
    assert_eq!(
        h.stdout(),
        "  #700 A pull request\n    next: needs review\n"
    );
}

#[test]
fn unresolved_threads_are_counted_across_all_pages() {
    let mut h = Fixture::ready().unresolved().harness();
    assert_eq!(h.dispatch(&["status", "700"]).unwrap(), Outcome::NotReady);
    assert_eq!(
        h.stdout(),
        "  #700 A pull request\n    next: 2 unresolved review threads\n"
    );
}

#[test_case("checks"; "check suites")]
#[test_case("reviews"; "review suites")]
fn truncated_check_connections_are_completed_via_rest(connection: &str) {
    let mut fixture = Fixture::ready();
    fixture.pull["commits"]["nodes"][0]["commit"][connection]["pageInfo"] =
        json!({"hasNextPage": true});
    fixture.rest_checks = Some(json!([
        {"check_runs": [{"name": "check", "status": "completed", "conclusion": "success", "app": {"id": 1}}]},
        {"check_runs": [{"name": "review", "status": "completed", "conclusion": "success", "app": {"id": 5_162_510}}]}
    ]));
    let mut h = fixture.harness();
    assert_eq!(h.dispatch(&["status", "700"]).unwrap(), Outcome::Success);
    assert_eq!(
        h.stdout(),
        "  #700 A pull request\n    next: ready to merge\n"
    );
}

#[test]
fn empty_open_pull_section() {
    let mut h = Harness::new(
        Some(Box::new(|_| {
            Ok(serde_json::to_vec(&json!([
                {"data": {"repository": {"pullRequests": {"nodes": []}}}}
            ]))
            .unwrap())
        })),
        None,
    );
    assert_eq!(h.dispatch(&["status"]).unwrap(), Outcome::Success);
    assert_eq!(h.stdout(), "Pull requests\n  none\n");
}

#[test]
fn list_sorts_every_page_by_pull_number() {
    let later = Fixture::ready().draft().pull;
    let mut earlier = Fixture::ready().draft().pull;
    earlier["number"] = json!(100);
    earlier["title"] = json!("Earlier pull request");
    let mut h = Harness::new(
        Some(Box::new(move |_| {
            Ok(serde_json::to_vec(&json!([
                {"data": {"repository": {"pullRequests": {"nodes": [later]}}}},
                {"data": {"repository": {"pullRequests": {"nodes": [earlier]}}}}
            ]))
            .unwrap())
        })),
        None,
    );
    assert_eq!(h.dispatch(&["status"]).unwrap(), Outcome::Success);
    assert_eq!(
        h.stdout(),
        "Pull requests\n  #100 Earlier pull request\n    next: draft\n  #700 A pull request\n    next: draft\n"
    );
}

#[test_case(&["status", "0"]; "zero")]
#[test_case(&["status", "-1"]; "negative")]
#[test_case(&["status", "abc"]; "not a number")]
#[test_case(&["status", "700", "extra"]; "extra argument")]
fn bad_usage_is_an_error(args: &[&str]) {
    let mut h = Harness::new(None, None);
    assert_eq!(
        h.dispatch(args).unwrap_err().render(),
        "usage: reviews status [N]"
    );
    assert_eq!(h.stdout(), "");
}

#[test_case("CLOSED"; "closed")]
#[test_case("MERGED"; "merged")]
fn non_open_pulls_are_errors(state: &str) {
    let mut fixture = Fixture::ready();
    fixture.pull["state"] = json!(state);
    let mut h = fixture.harness();
    assert!(h.dispatch(&["status", "700"]).is_err());
    assert_eq!(h.stdout(), "");
}

#[test_case("[{\"data\":{\"repository\":{\"pullRequest\":null}}}]"; "unknown pull")]
#[test_case("not json"; "invalid response")]
fn invalid_pull_responses_are_errors(response: &'static str) {
    let mut h = Harness::new(
        Some(Box::new(move |_| Ok(response.as_bytes().to_vec()))),
        None,
    );
    assert!(h.dispatch(&["status", "700"]).is_err());
    assert_eq!(h.stdout(), "");
}

#[test]
fn github_failure_keeps_the_cause() {
    let mut h = Harness::new(Some(Box::new(|_| Err(fail("not authenticated")))), None);
    assert_eq!(
        h.dispatch(&["status"]).unwrap_err().render(),
        "fetch open pull requests: not authenticated"
    );
    assert_eq!(h.stdout(), "");
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
    let mut h = fixture.harness();
    assert_eq!(h.dispatch(&["status", "700"]).unwrap(), Outcome::NotReady);
    assert_eq!(
        h.stdout(),
        "  #700 A pull request\n    next: needs review\n"
    );
}

#[test_case(Fixture::ready().unreviewed(), "/comments"; "records")]
#[test_case(Fixture::ready(), "reviewThreads("; "threads")]
#[test_case(Fixture::ready(), "/compare/"; "comparison")]
fn downstream_github_failures_are_errors(fixture: Fixture, target: &'static str) {
    let mut h = fixture.harness();
    let mut handler = h.fake.gh.take().unwrap();
    h.fake.gh = Some(Box::new(move |args| {
        if args.iter().any(|arg| arg.contains(target)) {
            Err(fail("request failed"))
        } else {
            handler(args)
        }
    }));
    assert!(h.dispatch(&["status", "700"]).is_err());
    assert_eq!(h.stdout(), "");
}
