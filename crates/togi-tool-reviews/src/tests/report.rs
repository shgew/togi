use std::collections::BTreeMap;
use std::io::{self, Write};

use jiff::{SignedDuration, Timestamp};
use serde_json::{Value, json};
use test_case::test_case;

use super::{
    Comment, Counts, PULLS_QUERY, Place, Pull, Row, Totals, format_duration, median, place, render,
    run, summarize, total,
};
use crate::fakes::{Fake, answer};
use crate::parts::split_record;
use crate::record::tests::read_fixture;
use crate::record::{Finding, parse_record};

const MINUTE: i64 = 60_000_000_000;
const HOUR: i64 = 60 * MINUTE;

fn opened() -> Timestamp {
    "2026-10-01T09:00:00Z".parse().expect("timestamp")
}

fn after(minutes: i64) -> Timestamp {
    opened() + SignedDuration::from_mins(minutes)
}

fn finding(source: &str, priority: &str, text: &str, outcome: &Value) -> Value {
    json!({"source": source, "priority": priority, "finding": text, "location": null, "outcome": outcome})
}

fn record_body(findings: &[Value]) -> String {
    let record =
        json!({"version": 3, "head_sha": "", "base_sha": "", "findings": findings, "verdict": ""});
    format!("## Review record\n\n<!-- togi-review {record} -->\n")
}

fn comment(url: &str, created: Timestamp, body: String) -> Comment {
    Comment {
        url: url.to_owned(),
        created,
        body,
    }
}

fn pull(number: i64, title: &str, check: &str, comments: Vec<Comment>) -> Pull {
    Pull {
        number,
        title: title.to_owned(),
        opened: opened(),
        check: check.to_owned(),
        comments,
    }
}

fn counts(priority: [i64; 4], outcome: [i64; 3]) -> Counts {
    Counts { priority, outcome }
}

#[test]
fn summarize_counts_each_finding_once() {
    let deferred = json!({"status": "deferred", "issue": 7});
    let fixed = json!({"status": "fixed", "sha": "abc"});
    let pulls = vec![
        pull(12, "blocked", "failure", vec![]),
        pull(
            10,
            "chain",
            "success",
            vec![
                comment(
                    "second",
                    after(180),
                    record_body(&[
                        finding("A", "P1", "leak", &fixed),
                        finding(
                            "B",
                            "P2",
                            "race",
                            &json!({"status": "rejected", "reason": "guarded"}),
                        ),
                    ]),
                ),
                comment("ping", after(1), "@coderabbitai full review".to_owned()),
                comment(
                    "first",
                    after(90),
                    record_body(&[
                        finding("A", "P1", "leak", &deferred),
                        finding("B", "P3", "typo", &fixed),
                    ]),
                ),
            ],
        ),
        pull(
            11,
            "no check",
            "",
            vec![comment("v2", after(20), read_fixture("v2.md"))],
        ),
        pull(9, "before review", "", vec![]),
    ];
    let rows = summarize(&pulls).expect("summarize");
    let row = |number, title: &str, check: &str, records, first_record, findings, places| Row {
        number,
        title: title.to_owned(),
        check: check.to_owned(),
        records,
        first_record,
        findings,
        places,
    };
    let want = vec![
        row(
            9,
            "before review",
            "",
            0,
            0,
            Counts::default(),
            BTreeMap::new(),
        ),
        row(
            10,
            "chain",
            "success",
            2,
            90 * MINUTE,
            counts([0, 1, 1, 1], [2, 1, 0]),
            BTreeMap::from([(Place::Unknown, 3)]),
        ),
        row(
            11,
            "no check",
            "",
            1,
            20 * MINUTE,
            counts([0, 0, 1, 3], [3, 0, 1]),
            BTreeMap::from([
                (dir("docs"), 2),
                (dir("tools/bench"), 1),
                (dir("tools/fit"), 1),
            ]),
        ),
        row(
            12,
            "blocked",
            "failure",
            0,
            0,
            Counts::default(),
            BTreeMap::new(),
        ),
    ];
    assert_eq!(want, rows);
    let want_totals = Totals {
        pulls: 4,
        checked: 2,
        successful: 1,
        recorded: 2,
        findings: counts([0, 1, 2, 4], [5, 1, 1]),
        first_records: vec![20 * MINUTE, 90 * MINUTE],
        places: BTreeMap::from([
            (dir("docs"), 2),
            (dir("tools/bench"), 1),
            (dir("tools/fit"), 1),
            (Place::Unknown, 3),
        ]),
    };
    assert_eq!(want_totals, total(&rows));
}

#[test]
fn summarize_malformed_record() {
    let pulls = vec![pull(
        1,
        "",
        "",
        vec![comment(
            "bad",
            opened(),
            "<!-- togi-review {\"version\":9} -->".to_owned(),
        )],
    )];
    let error = summarize(&pulls).expect_err("summarize accepted");
    assert!(
        error
            .render()
            .contains("parse record bad: unsupported record version 9"),
        "summarize error = {error}"
    );
}

#[test]
fn summarize_joins_a_split_record() {
    let fixed = json!({"status": "fixed", "sha": "abc"});
    let long = "x".repeat(300);
    let findings: Vec<Value> = (0..5)
        .map(|i| finding("A", "P2", &format!("{long} {i}"), &fixed))
        .collect();
    let bodies = split_record(&record_body(&findings), "abc", 800).expect("split");
    let last = i64::try_from(bodies.len()).expect("count");
    assert!(last > 1, "the record does not split");
    let pulls = vec![pull(
        1,
        "",
        "",
        bodies
            .into_iter()
            .enumerate()
            .map(|(i, body)| {
                comment(
                    &format!("part {i}"),
                    after(10 * (i64::try_from(i).expect("index") + 1)),
                    body,
                )
            })
            .collect(),
    )];
    let rows = summarize(&pulls).expect("summarize");
    assert_eq!(
        (1, 10 * last * MINUTE, counts([0, 0, 5, 0], [5, 0, 0])),
        (rows[0].records, rows[0].first_record, rows[0].findings),
        "a split record is one record, recorded when its last part was posted"
    );
}

fn at(location: &str, status: &str) -> String {
    format!(
        r#"{{"source":"A","priority":"P2","finding":"unchecked error"{location},"outcome":{{"status":"{status}"}}}}"#
    )
}

fn body(findings: &[String]) -> String {
    format!(
        r#"<!-- togi-review {{"version":3,"findings":[{}]}} -->"#,
        findings.join(",")
    )
}

#[test_case(
    &body(&[at(r#","location":"a.go:1""#, "deferred"), at(r#","location":"b.go:2""#, "rejected")]),
    &body(&[at(r#","location":"b.go:2""#, "fixed")]),
    counts([0, 0, 2, 0], [1, 0, 1]), (dir("."), 2); "update one of two locations")]
#[test_case(
    &body(&[at(r#","location":"a.go:1""#, "deferred")]),
    &body(&[at(r#","location":"b.go:2""#, "fixed")]),
    counts([0, 0, 2, 0], [1, 0, 1]), (dir("."), 2); "new location")]
#[test_case(
    &body(&[at(r#","location":null"#, "deferred")]),
    &body(&[at("", "fixed")]),
    counts([0, 0, 1, 0], [1, 0, 0]), (Place::Unknown, 1); "null then absent location")]
#[test_case(
    &body(&[at("", "deferred")]),
    &body(&[at(r#","location":null"#, "fixed")]),
    counts([0, 0, 1, 0], [1, 0, 0]), (Place::Unknown, 1); "absent then null location")]
fn summarize_shared_text(first: &str, second: &str, want: Counts, places: (Place, i64)) {
    let pulls = vec![pull(
        1,
        "",
        "",
        vec![
            comment("first", after(60), first.to_owned()),
            comment("second", after(120), second.to_owned()),
        ],
    )];
    let rows = summarize(&pulls).expect("summarize");
    let want = vec![Row {
        number: 1,
        records: 2,
        first_record: HOUR,
        findings: want,
        places: BTreeMap::from([places]),
        ..Row::default()
    }];
    assert_eq!(want, rows);
}

fn located(location: &str) -> Finding {
    let mut record = parse_record(&body(&[at(location, "fixed")])).expect("parse");
    record.findings.remove(0)
}

fn dir(name: &str) -> Place {
    Place::Directory(name.to_owned())
}

#[test_case(r#","location":{"path":"internal/tuner/step.go","line":7}"#, &dir("internal/tuner"); "version 1 object")]
#[test_case(r#","location":"crates/togi-tuner/src/decide.rs:42""#, &dir("crates/togi-tuner"); "string with a line")]
#[test_case(r#","location":"tools/audit/sweep.go:112-115""#, &dir("tools/audit"); "string with a range")]
#[test_case(r#","location":"crates/togi/tests/run/panics.rs""#, &dir("crates/togi"); "string without a line")]
#[test_case(r#","location":"docs/spec/tuner.md:227, docs/spec/workloads.md:193""#, &dir("docs/spec"); "several files count under the first")]
#[test_case(r#","location":"docs/adr/0058-native-rust-rewrite.md:13""#, &dir("docs/adr"); "adr")]
#[test_case(r#","location":"docs/review-record.md:12""#, &dir("docs"); "docs root")]
#[test_case(r#","location":"justfile:208-211""#, &dir("."); "repository root")]
#[test_case(r#","location":"AGENTS.md""#, &dir("."); "root without a line")]
#[test_case(r#","location":"cmd/togi/hostlock_test.go:352-355 (original head)""#, &dir("cmd/togi"); "trailing note")]
#[test_case(r#","location":"pull request body (demo)""#, &Place::Unknown; "not a file")]
#[test_case(r#","location":null"#, &Place::Unknown; "null")]
#[test_case("", &Place::Unknown; "absent")]
#[test_case(r#","location":"""#, &Place::Unknown; "empty")]
#[test_case(r#","location":7"#, &Place::Unknown; "number")]
#[test_case(r#","location":{"line":7}"#, &Place::Unknown; "object without a path")]
#[test_case(r#","location":["docs/spec/tuner.md"]"#, &Place::Unknown; "array of a path")]
fn place_of_a_finding(location: &str, want: &Place) {
    assert_eq!(*want, place(&located(location)));
}

#[test]
fn summarize_counts_an_updated_finding_once_in_its_place() {
    let pulls = vec![pull(
        1,
        "",
        "",
        vec![
            comment(
                "first",
                after(60),
                body(&[
                    at(r#","location":"crates/togi-tuner/src/a.rs:1""#, "deferred"),
                    at(r#","location":null"#, "deferred"),
                ]),
            ),
            comment(
                "second",
                after(120),
                body(&[at(r#","location":"crates/togi-tuner/src/a.rs:1""#, "fixed")]),
            ),
        ],
    )];
    let rows = summarize(&pulls).expect("summarize");
    assert_eq!(
        BTreeMap::from([(dir("crates/togi-tuner"), 1), (Place::Unknown, 1)]),
        rows[0].places
    );
}

#[test]
fn render_lists_directories_by_count_then_name_with_unknown_last() {
    let rows = vec![
        Row {
            places: BTreeMap::from([
                (dir("docs/spec"), 2),
                (Place::Unknown, 5),
                (dir("crates/b"), 1),
            ]),
            ..Row::default()
        },
        Row {
            places: BTreeMap::from([
                (dir("crates/a"), 2),
                (dir("crates/b"), 1),
                (dir("docs/adr"), 3),
            ]),
            ..Row::default()
        },
    ];
    let mut w = Vec::new();
    render(&mut w, &[], &total(&rows)).expect("render");
    let out = String::from_utf8(w).expect("utf-8");
    let want = "\nFindings by directory, each counted once:\ndocs/adr   3\ncrates/a   2\ncrates/b   2\ndocs/spec  2\nunknown    5\n";
    let directories = out
        .find("\nFindings by directory")
        .map(|start| &out[start..]);
    assert_eq!(Some(want), directories);
}

/// Accepts whole writes until `room` bytes are used, then fails the write that would exceed it.
struct FailingWriter {
    room: usize,
}

impl Write for FailingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.room {
            return Err(io::Error::other("write failed"));
        }
        self.room -= buf.len();
        Ok(buf.len())
    }

    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.write(buf).map(|_| ())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn recorded() -> Vec<Row> {
    vec![Row {
        number: 1,
        title: "recorded".to_owned(),
        check: "success".to_owned(),
        records: 1,
        first_record: 20 * MINUTE,
        places: BTreeMap::from([(dir("docs"), 1)]),
        ..Row::default()
    }]
}

fn unrecorded() -> Vec<Row> {
    vec![Row {
        number: 2,
        title: "unrecorded".to_owned(),
        ..Row::default()
    }]
}

#[test_case(&recorded(), "PR", "write table: write failed"; "table")]
#[test_case(&recorded(), "\n1 merged", "write totals: write failed"; "pull requests")]
#[test_case(&recorded(), "Findings:", "write totals: write failed"; "findings")]
#[test_case(&recorded(), "Time from", "write totals: write failed"; "first record")]
#[test_case(&recorded(), "\nFindings by", "write directories: write failed"; "directories")]
#[test_case(&unrecorded(), "\n1 merged", "write totals: write failed"; "pull requests unrecorded")]
#[test_case(&unrecorded(), "Findings:", "write totals: write failed"; "findings unrecorded")]
fn render_write_error(rows: &[Row], fail_at: &str, want: &str) {
    let mut full = Vec::new();
    render(&mut full, rows, &total(rows)).expect("render");
    let full = String::from_utf8(full).expect("utf-8");
    let room = full
        .find(fail_at)
        .unwrap_or_else(|| panic!("output lacks {fail_at:?}:\n{full}"));
    let error =
        render(&mut FailingWriter { room }, rows, &total(rows)).expect_err("render succeeded");
    assert_eq!(want, error.render());
    assert!(
        error.find::<io::Error>().is_some(),
        "render error {error} does not carry the write failure"
    );
}

fn pull_json(number: i64, title: &str, conclusions: &[&str], comments: &[Value]) -> Value {
    let runs: Vec<Value> = conclusions
        .iter()
        .map(|c| json!({"conclusion": c}))
        .collect();
    json!({
        "number": number, "title": title, "createdAt": "2026-10-01T09:00:00Z",
        "commits": {"nodes": [{"commit": {"checkSuites": {"nodes": [{"checkRuns": {"nodes": runs}}]}}}]},
        "comments": {"pageInfo": {"hasNextPage": false}, "nodes": comments},
    })
}

fn comment_node(login: Option<&str>, created: &str, url: &str, body: &str) -> Value {
    json!({"author": login.map(|login| json!({"login": login})), "createdAt": created, "url": url, "body": body})
}

fn page(pulls: &[Value]) -> Value {
    json!({"data": {"repository": {"pullRequests": {"nodes": pulls}}}})
}

#[test]
fn run_prints_the_track_record() {
    let (bot, owner) = (Some("robotogi"), Some("shgew"));
    let pages = json!([
        page(&[
            pull_json(
                358,
                "Rename failure points",
                &["FAILURE", "SUCCESS"],
                &[
                    comment_node(owner, "2026-10-01T09:05:00Z", "u0", &read_fixture("v1.md")),
                    comment_node(bot, "2026-10-01T11:30:00Z", "u1", &read_fixture("v3.md")),
                ]
            ),
            pull_json(
                287,
                "Fit the target machine",
                &[],
                &[
                    comment_node(None, "2026-10-01T09:01:00Z", "u2", "ghost"),
                    comment_node(bot, "2026-10-01T09:45:00Z", "u3", &read_fixture("v2.md")),
                ]
            ),
        ]),
        page(&[
            pull_json(
                273,
                "Simulate memory",
                &[""],
                &[comment_node(
                    bot,
                    "2026-10-01T10:00:00Z",
                    "u4",
                    &read_fixture("v1.md")
                ),]
            ),
            pull_json(200, "Before the App", &[], &[]),
        ]),
    ]);
    let out = pages.to_string().into_bytes();
    let mut fake = Fake::new(Some(Box::new(move |_| Ok(out.clone()))), None);
    let mut w = Vec::new();
    run(&mut fake, &mut w).expect("run");
    let query = format!("query={PULLS_QUERY}");
    let want_args = [
        "api",
        "graphql",
        "--paginate",
        "--slurp",
        "-F",
        "owner={owner}",
        "-F",
        "name={repo}",
        "-f",
        &query,
    ];
    assert_eq!(vec![want_args.map(str::to_owned).to_vec()], fake.gh_calls);
    let want = "PR     CHECK    RECORDS  P0  P1  P2  P3  FIXED  REJECTED  DEFERRED  FIRST RECORD  TITLE
#200   none     0        0   0   0   0   0      0         0         -             Before the App
#273   pending  1        0   0   4   0   4      0         0         1h00m         Simulate memory
#287   none     1        0   0   1   3   3      0         1         45m           Fit the target machine
#358   success  1        0   0   6   1   6      1         0         2h30m         Rename failure points
total  2/4      3/4      0   0   11  4   13     1         1

4 merged pull requests: 2 with robotogi's review check on the head (1 successful), 3 with a review record.
Findings: P0 0, P1 0, P2 11, P3 4; fixed 13, rejected 1, deferred 1.
Time from opening to the first record: median 1h00m, longest 2h30m.

Findings by directory, each counted once:
docs              4
internal/simrun   3
docs/spec         2
.                 1
internal/journal  1
internal/watch    1
tools/bench       1
tools/fit         1
tools/sim         1
";
    assert_eq!(want, String::from_utf8_lossy(&w));
}

#[test]
fn run_refuses_truncated_comments() {
    let mut fake = Fake::new(
        Some(answer(
            r#"[{"data":{"repository":{"pullRequests":{"nodes":[{"number":5,"comments":{"pageInfo":{"hasNextPage":true},"nodes":[]}}]}}}}]"#,
        )),
        None,
    );
    let error = run(&mut fake, &mut Vec::new()).expect_err("run accepted");
    assert!(
        error
            .render()
            .contains("pull request #5 has more than 100 comments"),
        "run error = {error}"
    );
}

#[test_case(&[20 * MINUTE], 20 * MINUTE; "one")]
#[test_case(&[20 * MINUTE, 90 * MINUTE], 55 * MINUTE; "two")]
#[test_case(&[10 * MINUTE, 20 * MINUTE, 90 * MINUTE], 20 * MINUTE; "three")]
#[test_case(&[10 * MINUTE, 20 * MINUTE, 30 * MINUTE, 90 * MINUTE], 25 * MINUTE; "four")]
fn median_of_sorted(sorted: &[i64], want: i64) {
    assert_eq!(want, median(sorted));
}

#[test_case(29_000_000_000, "0m"; "under half a minute")]
#[test_case(90_000_000_000, "2m"; "half a minute up")]
#[test_case(59 * MINUTE, "59m"; "minutes")]
#[test_case(26 * HOUR + 4 * MINUTE, "26h04m"; "hours")]
#[test_case(HOUR + 30_000_000_000 - 1, "1h00m"; "just under half a minute over")]
fn format_duration_rounds_to_the_minute(nanos: i64, want: &str) {
    assert_eq!(want, format_duration(nanos));
}
