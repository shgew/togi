use test_case::test_case;

use super::{Record, RecordError, parse_record};
use crate::fakes::{read_file, testdata};

fn summarize_record(r: &Record) -> (i64, Vec<String>) {
    let findings = r
        .findings
        .iter()
        .map(|f| {
            let detail = if f.outcome.issue != 0 {
                format!("#{}", f.outcome.issue)
            } else if !f.outcome.reason.is_empty() {
                "with reason".to_owned()
            } else {
                f.outcome.sha.clone()
            };
            format!("{} {} {} {detail}", f.source, f.priority, f.outcome.status)
        })
        .collect();
    (r.version, findings)
}

pub(crate) fn read_fixture(name: &str) -> String {
    read_file(testdata(name))
}

const FIX_273: &str = "e0ed29e08c7f9277da3739f546e24af6c978ee9b";
const FIX_287: &str = "9db8b22b904dba6b31bdae5e3f9f24038a2bd83f";
const FIX_358: &str = "575bdc4843bcf8cd8896e2ac523632b11a8bbfbe";

#[test_case("v1.md", 1, &[
    &format!("SimRuntime273 P2 fixed {FIX_273}"),
    &format!("SimDocs273 P2 fixed {FIX_273}"),
    &format!("CodeRabbit P2 fixed {FIX_273}"),
    &format!("Coordinator273 P2 fixed {FIX_273}"),
]; "v1")]
#[test_case("v2.md", 2, &[
    &format!("R287S1 P3 fixed {FIX_287}"),
    "R287S3 P2 deferred #263",
    &format!("R287S4 P3 fixed {FIX_287}"),
    &format!("R287S4 P3 fixed {FIX_287}"),
]; "v2")]
#[test_case("v3.md", 3, &[
    &format!("ReviewPR358.Watch P2 fixed {FIX_358}"),
    &format!("ReviewPR358.Simrun P2 fixed {FIX_358}"),
    "ReviewPR358.JournalLegacy P2 rejected with reason",
    &format!("ReviewPR358.Docs P3 fixed {FIX_358}"),
    &format!("ReviewPR358.Docs P2 fixed {FIX_358}"),
    &format!("ReviewPR358.Docs P2 fixed {FIX_358}"),
    &format!("ReviewPR358.Docs P2 fixed {FIX_358}"),
]; "v3")]
#[test_case("v3-delta.md", 3, &[]; "v3 delta")]
fn parse_record_versions(fixture: &str, version: i64, findings: &[&str]) {
    let r = parse_record(&read_fixture(fixture)).expect("parse");
    assert_eq!(
        (version, findings.iter().map(|&f| f.to_owned()).collect()),
        summarize_record(&r)
    );
}

fn block(json: &str) -> String {
    format!("## Review record\n\n<!-- togi-review {json} -->\n")
}

#[test_case("@coderabbitai full review", "no togi-review block"; "no block")]
#[test_case("<!-- togi-review {\"version\":3}", "unterminated togi-review block"; "unterminated")]
#[test_case(&block(r#"{"version":3,"#), "decode togi-review block"; "bad json")]
#[test_case(&block(r#"{"version":4,"findings":[]}"#), "unsupported record version 4"; "unknown version")]
#[test_case(&block(r#"{"version":3,"findings":[{"source":"R","priority":"P4","outcome":{"status":"fixed","sha":"a"}}]}"#), r#"finding from R: unknown priority "P4""#; "unknown priority")]
#[test_case(&block(r#"{"version":3,"findings":[{"source":"R","priority":"P1","outcome":{"status":"open"}}]}"#), r#"finding from R: unknown outcome "open""#; "unknown outcome")]
fn parse_record_rejects(body: &str, want: &str) {
    let error = parse_record(body).expect_err("parse_record accepted");
    assert!(
        error.to_string().contains(want),
        "parse_record error = {error}, want containing {want:?}"
    );
    if want == "no togi-review block" {
        assert!(
            matches!(error, RecordError::NoBlock),
            "parse_record error = {error:?}, want NoBlock"
        );
    }
}
