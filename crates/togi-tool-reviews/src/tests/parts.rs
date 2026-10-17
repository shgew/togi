use std::fmt::Write as _;

use jiff::Timestamp;
use test_case::test_case;

use super::{COMMENT_LIMIT, Comment, PART_OPEN, join_records, parse_part, split_record};
use crate::fakes::HEAD_SHA;
use crate::record::parse_record;

/// A record whose visible lines and JSON block hold `findings` findings of the given text.
fn record_md(findings: usize, text: &str) -> String {
    let mut md = String::from(
        "## Review record\n\n**success** on x.\n\n| Source | Priority | Finding | Outcome |\n|---|---|---|---|\n",
    );
    let mut block = Vec::new();
    for i in 0..findings {
        let _ = writeln!(md, "| r | P2 | {text} {i} | rejected: \"why\" |");
        block.push(format!(
            r#"{{"source":"r","priority":"P2","finding":"{text} {i} \"q\" \\ \u003c","location":"a.rs:{i}","url":null,"outcome":{{"status":"rejected","reason":"why"}}}}"#
        ));
    }
    md.push_str("\n<details>\n<summary>Review data</summary>\n\n</details>\n");
    format!(
        "{md}\n<!-- togi-review {{\"version\":3,\"head_sha\":\"{HEAD_SHA}\",\"findings\":[{}],\"verdict\":\"success\"}} -->\n",
        block.join(",")
    )
}

fn comment(i: usize, body: &str) -> Comment {
    Comment {
        url: format!(
            "https://github.com/shgew/togi/pull/700#issuecomment-{}",
            5001 + i
        ),
        created: Timestamp::from_second(i64::try_from(i).expect("index")).expect("time"),
        body: body.to_owned(),
    }
}

#[test]
fn a_record_that_fits_is_one_comment_as_rendered() {
    let md = record_md(3, "short");
    assert_eq!(
        vec![md.clone()],
        split_record(&md, HEAD_SHA, md.chars().count()).expect("split")
    );
    assert!(parse_part(&md).is_none(), "a whole record read as a part");
}

#[test_case(1, "plain", 400; "json split across parts")]
#[test_case(40, "plain", 600; "lines and json split")]
#[test_case(40, "ünïcödé ✓", 600; "characters, not bytes")]
#[test_case(2, &"long line ".repeat(200), 500; "a line longer than a part")]
#[test_case(400, "finding", COMMENT_LIMIT; "at github's limit")]
fn split_parts_fit_and_join_back(findings: usize, text: &str, limit: usize) {
    let md = record_md(findings, text);
    let bodies = split_record(&md, HEAD_SHA, limit).expect("split");
    assert!(
        bodies.len() > 1,
        "{} characters fit in one comment",
        md.chars().count()
    );
    for (i, body) in bodies.iter().enumerate() {
        assert!(
            body.chars().count() <= limit,
            "part {} has {} characters, over {limit}",
            i + 1,
            body.chars().count()
        );
        let part = parse_part(body).expect("a part").expect("parse part");
        assert_eq!((i + 1, bodies.len()), (part.part, part.parts));
    }
    let mut comments: Vec<Comment> = bodies
        .iter()
        .enumerate()
        .map(|(i, b)| comment(i, b))
        .collect();
    comments.reverse();
    comments.insert(1, comment(99, "hello"));
    let records = join_records(comments).expect("join");
    assert_eq!(2, records.len());
    let joined = &records[1];
    assert_eq!(
        md, joined.body,
        "the parts do not join back into the record"
    );
    assert_eq!(
        comment(0, "").url,
        joined.url,
        "the record is not at its first part"
    );
    assert_eq!(
        comment(bodies.len() - 1, "").created,
        joined.created,
        "the record is not created when its last part was"
    );
    let record = parse_record(&joined.body).expect("parse the joined record");
    assert_eq!(findings, record.findings.len());
}

#[test]
fn parts_read_back_through_crlf() {
    let md = record_md(10, "plain");
    let bodies = split_record(&md, HEAD_SHA, 500).expect("split");
    let comments = bodies
        .iter()
        .enumerate()
        .map(|(i, b)| comment(i, &b.replace('\n', "\r\n")))
        .collect();
    let records = join_records(comments).expect("join");
    assert_eq!(
        vec![md],
        records.into_iter().map(|r| r.body).collect::<Vec<_>>()
    );
}

#[test]
fn a_record_missing_a_part_is_left_out() {
    let md = record_md(10, "plain");
    let bodies = split_record(&md, HEAD_SHA, 500).expect("split");
    let comments = bodies
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != 1)
        .map(|(i, b)| comment(i, b))
        .collect();
    assert!(join_records(comments).expect("join").is_empty());
}

#[test]
fn a_reposted_part_counts_once() {
    let md = record_md(10, "plain");
    let bodies = split_record(&md, HEAD_SHA, 500).expect("split");
    let mut comments: Vec<Comment> = bodies
        .iter()
        .enumerate()
        .map(|(i, b)| comment(i, b))
        .collect();
    comments.push(comment(50, &bodies[0]));
    let records = join_records(comments).expect("join");
    assert_eq!(1, records.len());
    assert_eq!(md, records[0].body);
}

#[test_case(&format!("x\n{PART_OPEN}{{\"head_sha\":\"a\",\"part\":1,\"parts\":2,\"data\":\"\"}} -->"), "lacks its note"; "no note")]
#[test_case(&format!("<sub>Review record, part 3 of 2.</sub>\n{PART_OPEN}{{\"head_sha\":\"a\",\"part\":3,\"parts\":2,\"data\":\"\"}} -->"), "part 3 of 2 on head \"a\" is out of range"; "part out of range")]
#[test_case(&format!("{PART_OPEN}{{\"head_sha\":\"a\"}}"), "unterminated togi-review-part block"; "unterminated")]
#[test_case(&format!("{PART_OPEN}{{\"head_sha\":\"a\",\"extra\":1}} -->"), "decode togi-review-part block"; "unknown field")]
#[test_case(&format!("<sub>Review record, part 1 of 2.</sub>\n{PART_OPEN}[\"a\",1,2,\"\"] -->"), "decode togi-review-part block: invalid type: sequence, expected an object"; "array header")]
fn malformed_parts_are_refused(body: &str, want: &str) {
    let error = join_records(vec![comment(0, body)]).expect_err("join accepted");
    let text = error.render();
    assert!(
        text.contains(want),
        "error = {text}, want containing {want:?}"
    );
}
