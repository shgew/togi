use std::path::Path;

use serde::Deserialize;
use test_case::test_case;

use super::{
    FindingsInput, RECORD_FILE, RECORD_JSON_FILE, RecordFinding, RecordModel, RecordOutcome,
    RecordReviewer, build_record, code, run_render, text,
};
use crate::fakes::{
    BASE_SHA, FIX_SHA, HEAD_SHA, OLD_BASE_SHA, OLD_HEAD_SHA, PREVIOUS_URL, check_golden, read_file,
};
use crate::manifest::{
    CoverRange, Coverage, MANIFEST_VERSION, Manifest, ManifestFile, PreviousRecord, read_manifest,
    write_manifest,
};
use crate::record::{RECORD_CLOSE, RECORD_OPEN, parse_record};

pub(crate) fn included(path: &str, added: i64, removed: i64) -> ManifestFile {
    ManifestFile {
        path: path.to_owned(),
        status: "modified".to_owned(),
        added,
        removed,
        hunk_file: "files/001.diff".to_owned(),
        ..ManifestFile::default()
    }
}

pub(crate) fn excluded(path: &str, status: &str, added: i64, exclusion: &str) -> ManifestFile {
    ManifestFile {
        path: path.to_owned(),
        status: status.to_owned(),
        added,
        exclusion: exclusion.to_owned(),
        ..ManifestFile::default()
    }
}

pub(crate) fn first_manifest(files: Vec<ManifestFile>) -> Manifest {
    Manifest {
        version: MANIFEST_VERSION,
        repository: "shgew/togi".to_owned(),
        pull_request: 700,
        head_sha: HEAD_SHA.to_owned(),
        base_sha: BASE_SHA.to_owned(),
        files,
        ..Manifest::default()
    }
}

pub(crate) fn range(path: &str, first: i64, last: i64) -> CoverRange {
    CoverRange {
        path: path.to_owned(),
        first,
        last,
    }
}

pub(crate) fn with_coverage(mut m: Manifest, ranges: Vec<CoverRange>) -> Manifest {
    m.coverage = Some(Coverage {
        reported: i64::try_from(ranges.len()).expect("count"),
        ranges,
        not_built: Vec::new(),
    });
    m
}

pub(crate) fn reviewer(name: &str, model: Option<&str>, files: &[&str]) -> RecordReviewer {
    RecordReviewer {
        name: name.to_owned(),
        model: model.map(str::to_owned),
        files: files.iter().map(|&f| f.to_owned()).collect(),
    }
}

pub(crate) fn outcome(status: &str, sha: &str, reason: &str, issue: i64) -> RecordOutcome {
    RecordOutcome {
        status: status.to_owned(),
        sha: sha.to_owned(),
        reason: reason.to_owned(),
        issue,
        body: String::new(),
    }
}

fn body_fix(sha: &str, body: &str) -> RecordOutcome {
    RecordOutcome {
        body: body.to_owned(),
        ..outcome("fixed", sha, "", 0)
    }
}

pub(crate) fn fixed() -> RecordOutcome {
    outcome("fixed", FIX_SHA, "", 0)
}

pub(crate) fn deferred(issue: i64) -> RecordOutcome {
    outcome("deferred", "", "", issue)
}

pub(crate) fn finding(
    source: &str,
    priority: &str,
    text: &str,
    location: Option<&str>,
    outcome: RecordOutcome,
) -> RecordFinding {
    RecordFinding {
        source: source.to_owned(),
        priority: priority.to_owned(),
        finding: text.to_owned(),
        location: location.map(str::to_owned),
        url: None,
        outcome,
    }
}

const MODEL: Option<&str> = Some("provider/model-1");

fn render_case(name: &str) -> (Manifest, FindingsInput) {
    let doc_reviewers = vec![reviewer("reviewer-1", MODEL, &["docs/howto.md"])];
    let doc = || first_manifest(vec![included("docs/howto.md", 5, 2)]);
    match name {
        "success" => {
            let m = with_coverage(
                first_manifest(vec![
                    included("internal/tuner/step.go", 30, 4),
                    included("internal/tuner/step_test.go", 50, 0),
                ]),
                vec![
                    range("internal/tuner/step.go", 12, 14),
                    range("internal/tuner/step.go", 40, 40),
                ],
            );
            let mut first = finding(
                "reviewer-1",
                "P2",
                "Readback skipped on retry | second line\nof text",
                Some("internal/tuner/step.go:42"),
                fixed(),
            );
            first.url = Some("https://github.com/shgew/togi/pull/700#discussion_r1".to_owned());
            (m, FindingsInput {
                reviewers: vec![
                    reviewer("reviewer-1", MODEL, &["internal/tuner/step.go"]),
                    reviewer("reviewer-2", None, &["internal/tuner/step_test.go"]),
                ],
                findings: vec![
                    first,
                    finding("reviewer-2", "P3", "Stale comment <b>&</b>", None, outcome("rejected", "", "wording only", 0)),
                    finding("coordinator", "P2", "Deferred to a later issue", None, deferred(123)),
                ],
                coverage_judgment: "Both uncovered ranges are error paths the simulator cannot reach; neither became a finding."
                    .to_owned(),
                blocked: String::new(),
            })
        }
        "blocked" => (
            doc(),
            FindingsInput {
                reviewers: doc_reviewers,
                findings: vec![
                    finding(
                        "reviewer-1",
                        "P1",
                        "Wrong command",
                        Some("docs/howto.md:7"),
                        deferred(9),
                    ),
                    finding("reviewer-1", "P0", "Unsafe", None, deferred(10)),
                    finding("reviewer-1", "P2", "Minor", None, deferred(11)),
                ],
                blocked: "a review thread is unresolved.".to_owned(),
                ..FindingsInput::default()
            },
        ),
        "no-findings" => (
            doc(),
            FindingsInput {
                reviewers: doc_reviewers,
                ..FindingsInput::default()
            },
        ),
        "exclusions" => (
            first_manifest(vec![
                included("docs/howto.md", 5, 2),
                excluded("go.sum", "modified", 2, "generated"),
                excluded(
                    "pkg/testdata/in.txt",
                    "added",
                    1,
                    "test data (`**/testdata/**`)",
                ),
            ]),
            FindingsInput {
                reviewers: doc_reviewers,
                ..FindingsInput::default()
            },
        ),
        "no-included" => (
            first_manifest(vec![excluded("go.sum", "modified", 2, "generated")]),
            FindingsInput::default(),
        ),
        "coverage" => (
            with_coverage(
                first_manifest(vec![included("a.go", 3, 0)]),
                vec![range("a.go", 5, 5)],
            ),
            FindingsInput {
                reviewers: vec![reviewer("coordinator", None, &["a.go"])],
                coverage_judgment: "Judged a boundary a consumer could see; became a finding."
                    .to_owned(),
                findings: vec![finding(
                    "coordinator",
                    "P2",
                    "Boundary untested",
                    Some("a.go:5"),
                    fixed(),
                )],
                ..FindingsInput::default()
            },
        ),
        "delta" => {
            let mut m = with_coverage(
                first_manifest(vec![included("internal/tuner/step.go", 3, 1)]),
                vec![range("internal/tuner/step.go", 12, 12)],
            );
            m.previous = Some(PreviousRecord {
                url: PREVIOUS_URL.to_owned(),
                head_sha: OLD_HEAD_SHA.to_owned(),
                base_sha: OLD_BASE_SHA.to_owned(),
                ..PreviousRecord::default()
            });
            (
                m,
                FindingsInput {
                    reviewers: vec![reviewer("reviewer-1", MODEL, &["internal/tuner/step.go"])],
                    findings: vec![finding(
                        "reviewer-1",
                        "P2",
                        "Earlier finding, now fixed",
                        None,
                        fixed(),
                    )],
                    coverage_judgment: "The one range is a line the earlier record judged."
                        .to_owned(),
                    ..FindingsInput::default()
                },
            )
        }
        "fallback" => {
            let mut m = doc();
            m.previous = Some(PreviousRecord {
                url: PREVIOUS_URL.to_owned(),
                head_sha: OLD_HEAD_SHA.to_owned(),
                ..PreviousRecord::default()
            });
            "the previous record is version 1 and does not name the base the pull request had at its head, so no minimal delta can be established"
                .clone_into(&mut m.fallback);
            (
                m,
                FindingsInput {
                    reviewers: doc_reviewers,
                    ..FindingsInput::default()
                },
            )
        }
        _ => panic!("no render case {name}"),
    }
}

pub(crate) fn dir_text(dir: &Path) -> String {
    dir.to_string_lossy().into_owned()
}

pub(crate) fn write_findings(dir: &Path, raw: &str) -> String {
    let file = dir.join("findings.json");
    std::fs::write(&file, raw).expect("write findings");
    dir_text(&file)
}

fn render(dir: &Path, findings: &str) -> Result<Vec<u8>, crate::Error> {
    let mut stdout = Vec::new();
    run_render(&mut stdout, &[dir_text(dir), findings.to_owned()])?;
    Ok(stdout)
}

#[test_case("success")]
#[test_case("blocked")]
#[test_case("no-findings")]
#[test_case("exclusions")]
#[test_case("no-included")]
#[test_case("coverage")]
#[test_case("delta")]
#[test_case("fallback")]
fn render_golden(name: &str) {
    let (m, input) = render_case(name);
    let dir = tempfile::tempdir().expect("temporary directory");
    write_manifest(&dir_text(dir.path()), &m).expect("write manifest");
    let findings = write_findings(
        dir.path(),
        &serde_json::to_string(&input).expect("encode findings"),
    );
    let stdout = render(dir.path(), &findings).expect("render");
    let md = read_file(dir.path().join(RECORD_FILE));
    let js = read_file(dir.path().join(RECORD_JSON_FILE));
    check_golden(&format!("render/{name}.md"), &md);
    check_golden(&format!("render/{name}.json"), &js);
    check_same_data(&md, &js);
    let base = dir
        .path()
        .file_name()
        .expect("name")
        .to_string_lossy()
        .into_owned();
    let want = format!(
        "{base}: {} on {HEAD_SHA}\nwrote {}/record.md and {}/record.json\n",
        if name == "blocked" {
            "blocked"
        } else {
            "success"
        },
        dir_text(dir.path()),
        dir_text(dir.path())
    );
    assert_eq!(want, String::from_utf8_lossy(&stdout));
}

/// Holds the Markdown and the JSON to carrying one record: the hidden block is the JSON file, and what the
/// JSON names is in the text.
fn check_same_data(md: &str, js: &str) {
    let block = js.strip_suffix('\n').unwrap_or(js);
    assert!(
        md.ends_with(&format!("\n{RECORD_OPEN}{block}{RECORD_CLOSE}\n")),
        "the Markdown does not end with the JSON block"
    );
    assert!(
        md.matches(RECORD_OPEN).count() == 1
            && !block.contains('\n')
            && !js.contains(['<', '>', '&']),
        "the block must be one line, once, with <, > and & escaped"
    );
    parse_record(md).expect("the record parses as a historical one");
    let mut de = serde_json::Deserializer::from_str(js);
    let r = RecordModel::deserialize(&mut de).expect("decode record.json strictly");
    assert_eq!(3, r.version);
    let verdict = format!("**{}**", r.verdict);
    for want in [&r.head_sha, &r.base_sha, &verdict]
        .into_iter()
        .chain(&r.files)
    {
        assert!(
            md.contains(want.as_str()),
            "the text lacks {want:?} that the JSON carries"
        );
    }
    for e in &r.excluded_files {
        assert!(
            md.contains(&e.path) && md.contains(&e.reason),
            "the text lacks the exclusion of {}",
            e.path
        );
    }
    let rows = md.matches("\n| ").count().saturating_sub(1);
    if !r.findings.is_empty() {
        assert_eq!(r.findings.len(), rows, "the table rows");
    }
    assert_eq!(
        r.findings.is_empty(),
        md.contains("No findings."),
        "the text says no findings when there are some, or the reverse"
    );
    if let Some(previous) = &r.previous {
        assert!(
            md.contains(&format!(
                "{}, range {}..{}",
                previous.url, previous.head_sha, r.head_sha
            )),
            "the text lacks the previous record and range"
        );
    }
}

fn rv(files: &[&str]) -> Vec<RecordReviewer> {
    vec![reviewer("r", None, files)]
}

fn one_finding(outcome: RecordOutcome) -> Vec<RecordFinding> {
    vec![finding("r", "P2", "x", None, outcome)]
}

fn input(
    reviewers: Vec<RecordReviewer>,
    findings: Vec<RecordFinding>,
    judgment: &str,
) -> FindingsInput {
    FindingsInput {
        reviewers,
        findings,
        coverage_judgment: judgment.to_owned(),
        blocked: String::new(),
    }
}

fn go_manifest() -> Manifest {
    with_coverage(
        first_manifest(vec![included("a.go", 1, 0)]),
        vec![range("a.go", 1, 1)],
    )
}

fn doc_manifest() -> Manifest {
    first_manifest(vec![included("docs/x.md", 1, 0)])
}

fn carry_manifest() -> Manifest {
    let mut m = doc_manifest();
    m.carry_forward = true;
    m
}

const RUST: &str = "crates/togi-x/src/lib.rs";

#[test_case(&carry_manifest(), FindingsInput::default(), "carry the record forward"; "carry forward")]
#[test_case(&doc_manifest(), FindingsInput::default(), "reviewers are required"; "no reviewers")]
#[test_case(&doc_manifest(), input(rv(&[]), vec![], ""), "no reviewer covers docs/x.md"; "uncovered file")]
#[test_case(&doc_manifest(), input(rv(&["docs/x.md", "other.md"]), vec![], ""), "does not include"; "foreign file")]
#[test_case(&first_manifest(vec![excluded("go.sum", "", 0, "generated")]), input(rv(&[]), vec![], ""), "no file is included"; "reviewers without files")]
#[test_case(&first_manifest(vec![included("a.go", 1, 0)]), input(rv(&["a.go"]), vec![], "j"), "has no coverage"; "no coverage")]
#[test_case(&go_manifest(), input(rv(&["a.go"]), vec![], ""), "coverage_judgment is required"; "no judgment")]
#[test_case(&doc_manifest(), input(rv(&["docs/x.md"]), vec![], "j"), "changes no Go or Rust code"; "judgment without Go")]
#[test_case(&first_manifest(vec![included(RUST, 1, 0)]), input(rv(&[RUST]), vec![], "j"), "has no coverage"; "no coverage for Rust")]
#[test_case(&with_coverage(first_manifest(vec![included(RUST, 1, 0)]), vec![]), input(rv(&[RUST]), vec![], ""), "coverage_judgment is required"; "no judgment for Rust")]
#[test_case(&doc_manifest(), input(rv(&["docs/x.md"]), vec![finding("r", "P4", "x", None, outcome("rejected", "", "r", 0))], ""), "unknown priority"; "bad priority")]
#[test_case(&doc_manifest(), input(rv(&["docs/x.md"]), one_finding(outcome("fixed", "abc", "", 0)), ""), "outcome must be"; "short sha")]
#[test_case(&doc_manifest(), input(rv(&["docs/x.md"]), one_finding(outcome("deferred", "", "x", 1)), ""), "outcome must be"; "two outcome forms")]
#[test_case(&doc_manifest(), input(rv(&["docs/x.md"]), one_finding(outcome("open", "", "", 0)), ""), "outcome must be"; "unknown status")]
#[test_case(&doc_manifest(), input(rv(&["docs/x.md"]), one_finding(body_fix(FIX_SHA, "mapping")), ""), "outcome must be"; "sha and body")]
#[test_case(&doc_manifest(), input(rv(&["docs/x.md"]), one_finding(body_fix("abc", "mapping")), ""), "outcome must be"; "short sha and body")]
#[test_case(&doc_manifest(), input(rv(&["docs/x.md"]), one_finding(outcome("fixed", "", "", 0)), ""), "outcome must be"; "fixed without sha or body")]
#[test_case(&doc_manifest(), input(rv(&["docs/x.md"]), one_finding(RecordOutcome { body: "mapping".to_owned(), ..outcome("rejected", "", "r", 0) }), ""), "outcome must be"; "rejected with body")]
#[test_case(&doc_manifest(), input(rv(&["docs/x.md"]), one_finding(RecordOutcome { body: "mapping".to_owned(), ..deferred(1) }), ""), "outcome must be"; "deferred with body")]
#[test_case(&doc_manifest(), input(vec![reviewer("", None, &["docs/x.md"])], vec![], ""), "a reviewer has no name"; "nameless reviewer")]
#[test_case(&doc_manifest(), input(rv(&["docs/x.md"]), vec![finding("", "P2", "x", None, outcome("rejected", "", "r", 0))], ""), "a finding needs a source and text"; "finding without source")]
#[test_case(&doc_manifest(), input(rv(&["docs/x.md"]), vec![finding("r", "P2", "", None, outcome("rejected", "", "r", 0))], ""), "a finding needs a source and text"; "finding without text")]
fn build_record_rejects(m: &Manifest, input: FindingsInput, want: &str) {
    let error = build_record(m, input).expect_err("build_record accepted");
    assert!(
        error.render().contains(want),
        "error = {error}, want one containing {want:?}"
    );
}

#[test]
fn render_rejects_unknown_findings_field() {
    let dir = tempfile::tempdir().expect("temporary directory");
    write_manifest(&dir_text(dir.path()), &first_manifest(vec![])).expect("write manifest");
    let file = write_findings(dir.path(), r#"{"reviewer":[]}"#);
    let error = render(dir.path(), &file).expect_err("render accepted");
    assert!(
        error.render().contains("unknown field"),
        "error = {error}, want an unknown-field refusal"
    );
    let error = run_render(&mut Vec::new(), &[dir_text(dir.path())]).expect_err("render accepted");
    assert!(
        error.render().contains("usage: reviews render"),
        "error = {error}, want usage"
    );
}

#[test_case(r#"[1,"shgew/togi",700,"a","b",null,"",false,[],0,0,0,false,0,0,[],null]"#; "array manifest")]
#[test_case(r#"{"version":1,"files":[["docs/x.md","","modified",1,0,false,"","files/001.diff"]]}"#; "array file")]
#[test_case(r#"{"version":1,"coverage":{"reported":1,"ranges":[["a.rs",1,2]],"not_built":[]}}"#; "array range")]
fn read_manifest_refuses_arrays(raw: &str) {
    let dir = tempfile::tempdir().expect("temporary directory");
    std::fs::write(dir.path().join("manifest.json"), raw).expect("write manifest");
    let error = read_manifest(&dir_text(dir.path())).expect_err("read_manifest accepted");
    assert!(
        error
            .render()
            .contains("invalid type: sequence, expected an object"),
        "error = {}, want an array refusal",
        error.render()
    );
}

/// `s` with the entities render writes read back as characters.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';') else { break };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            _ => entity
                .strip_prefix('#')
                .and_then(|n| n.parse().ok())
                .and_then(char::from_u32),
        };
        if let Some(c) = decoded {
            out.push(c);
            rest = &rest[end + 1..];
        } else {
            out.push('&');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

#[test]
fn render_keeps_comment_markers_visible() {
    const MARKER: &str = r#"a <!-- togi-review {"version":9} --> & <b>x</b> --!> <!-->"#;
    const REASON: &str = "<!-- hidden --> wording & more";
    const PLACE: &str = "a<!--.go:1";
    const SOURCE: &str = "R <!-- s -->";
    const BLOCKED: &str = "thread <!-- open --> unresolved";
    const JUDGED: &str = "range <!-- j --> & judged";
    const IN_CODE: &str = "Reject `<nil>` and `<!-- togi-review {} -->`";
    let dir = tempfile::tempdir().expect("temporary directory");
    write_manifest(
        &dir_text(dir.path()),
        &with_coverage(
            first_manifest(vec![included("a.go", 3, 0)]),
            vec![range("a.go", 5, 5)],
        ),
    )
    .expect("write manifest");
    let mut marked = finding(
        SOURCE,
        "P2",
        MARKER,
        Some(PLACE),
        outcome("rejected", "", REASON, 0),
    );
    marked.url = Some("https://example.test/a?x=<!--&y".to_owned());
    let input = FindingsInput {
        reviewers: vec![reviewer(
            "coordinator <!-- n -->",
            Some("m <!-- m -->"),
            &["a.go"],
        )],
        findings: vec![marked, finding("R", "P3", IN_CODE, None, deferred(2))],
        coverage_judgment: JUDGED.to_owned(),
        blocked: BLOCKED.to_owned(),
    };
    let file = write_findings(
        dir.path(),
        &serde_json::to_string(&input).expect("encode findings"),
    );
    render(dir.path(), &file).expect("render");
    let md = read_file(dir.path().join(RECORD_FILE));
    let js = read_file(dir.path().join(RECORD_JSON_FILE));
    check_same_data(&md, &js);
    assert!(
        md.matches("<!--").count() == 1
            && md.matches("-->").count() == 1
            && !md.contains("--!>")
            && !md.contains("<!-->"),
        "the Markdown holds more than the one hidden block:\n{md}"
    );
    let shown = md
        .split(&format!("\n{RECORD_OPEN}"))
        .next()
        .unwrap_or_default();
    let visible = unescape(shown.strip_suffix('\n').unwrap_or(shown));
    let reason = format!("rejected: {REASON}");
    let place = format!("(<code>{PLACE}</code>)");
    for want in [
        MARKER,
        &reason,
        &place,
        SOURCE,
        BLOCKED,
        JUDGED,
        "coordinator <!-- n --> (m <!-- m -->)",
    ] {
        assert!(
            visible.contains(want),
            "the visible text lost {want:?}:\n{visible}"
        );
    }
    for want in [
        r#"a &lt;!-- togi-review {"version":9} --&gt; &amp; &lt;b&gt;x&lt;/b&gt; --!&gt; &lt;!--&gt;"#,
        "rejected: &lt;!-- hidden --&gt; wording &amp; more",
        "(<code>a&lt;&#33;&#45;&#45;&#46;go&#58;1</code>)",
        "Reject <code>&lt;nil&gt;</code> and <code>&lt;&#33;&#45;&#45; togi&#45;review &#123;&#125; &#45;&#45;&gt;</code>",
    ] {
        assert!(
            md.contains(want),
            "the Markdown lacks the exact escaped text {want:?}:\n{md}"
        );
    }
    let record = parse_record(&md).expect("the hidden block parses");
    let findings: Vec<&str> = record.findings.iter().map(|f| f.finding.as_str()).collect();
    assert_eq!(vec![MARKER, IN_CODE], findings);
}

#[test_case("a < b & c > d", "a &lt; b &amp; c &gt; d"; "plain")]
#[test_case("x <!-- y --> z", "x &lt;!-- y --&gt; z"; "comment")]
#[test_case("Reject `<nil>` first", "Reject <code>&lt;nil&gt;</code> first"; "code span keeps its meaning")]
#[test_case("use ``a`b`` here", "use <code>a&#96;b</code> here"; "double backtick span holds a backtick")]
#[test_case("``a`b`` and `c`", "<code>a&#96;b</code> and <code>c</code>"; "mixed delimiters close on the same run")]
#[test_case("``a`b", "``a`b"; "shorter run does not close")]
#[test_case("a ` <b>", "a ` &lt;b&gt;"; "unmatched backtick is literal")]
#[test_case("\\`<b>`", "\\`&lt;b&gt;`"; "escaped backtick opens nothing")]
#[test_case(r"\<b>", "&#92;&lt;b&gt;"; "backslash cannot undo an entity")]
#[test_case("a|b", r"a\|b"; "pipe")]
#[test_case("`a|b`", "<code>a&#124;b</code>"; "pipe in code")]
#[test_case("`` `a` ``", "<code>&#96;a&#96;</code>"; "padding spaces stripped")]
#[test_case("`a\n\nb`", "`a\n\nb`"; "blank line ends the span")]
fn markdown_text(input: &str, want: &str) {
    assert_eq!(want, text(input));
}

#[test]
fn markdown_cell_and_code() {
    assert_eq!(
        r"a <code>&lt;nil&gt;</code> \|",
        super::cell("a\n`<nil>`\t|")
    );
    for (input, want) in [
        ("internal/a.go:4", "`internal/a.go:4`"),
        ("a`b", "<code>a&#96;b</code>"),
        ("<nil>.go", "<code>&lt;nil&gt;&#46;go</code>"),
        (" a", "<code> a</code>"),
    ] {
        assert_eq!(want, code(input), "code({input:?})");
    }
}

const ONE: &str = r#"{"reviewers":[{"name":"r","files":["docs/x.md"]}],"findings":[]}"#;

#[test_case(&format!("{ONE} \r\n\t\n"), ""; "trailing whitespace")]
#[test_case(&format!(r#"{ONE}{{"blocked":"a blocker"}}"#), "data follows"; "second object")]
#[test_case(&format!("{ONE}\n{ONE}"), "data follows"; "second object after newline")]
#[test_case(&format!("{ONE} junk"), "data follows"; "trailing text")]
#[test_case(&format!("{ONE}}}"), "data follows"; "trailing closer")]
#[test_case(&ONE[..ONE.len() - 1], "decode findings"; "truncated")]
#[test_case("", "decode findings"; "empty")]
#[test_case("[]", "invalid type: sequence, expected an object"; "empty array")]
#[test_case(r#"[[{"name":"r","files":["docs/x.md"]}],[],"",""]"#, "invalid type: sequence, expected an object"; "array of the fields")]
#[test_case(r#"{"reviewers":[["r",null,["docs/x.md"]]],"findings":[]}"#, "invalid type: sequence, expected an object"; "array reviewer")]
#[test_case(r#"{"reviewers":[{"name":"r","files":["docs/x.md"]}],"findings":[{"source":"r","priority":"P3","finding":"x","outcome":["rejected","","why",0]}]}"#, "invalid type: sequence, expected an object"; "array outcome")]
fn render_reads_exactly_one_findings_object(raw: &str, want: &str) {
    let dir = tempfile::tempdir().expect("temporary directory");
    write_manifest(
        &dir_text(dir.path()),
        &first_manifest(vec![included("docs/x.md", 1, 0)]),
    )
    .expect("write manifest");
    let file = write_findings(dir.path(), raw);
    let mut stdout = Vec::new();
    let result = run_render(&mut stdout, &[dir_text(dir.path()), file]);
    if want.is_empty() {
        result.expect("render");
        return;
    }
    let error = result.expect_err("render accepted");
    assert!(
        error.render().contains(want),
        "error = {error}, want one containing {want:?}"
    );
    for f in [RECORD_FILE, RECORD_JSON_FILE] {
        assert!(
            !dir.path().join(f).exists(),
            "{f} written for invalid findings"
        );
    }
    assert!(
        stdout.is_empty(),
        "stdout = {:?} for invalid findings",
        String::from_utf8_lossy(&stdout)
    );
}

#[test]
fn render_records_a_finding_fixed_in_the_pull_request_body() {
    let dir = tempfile::tempdir().expect("temporary directory");
    write_manifest(
        &dir_text(dir.path()),
        &first_manifest(vec![included("docs/x.md", 1, 0)]),
    )
    .expect("write manifest");
    let file = write_findings(
        dir.path(),
        r#"{"reviewers":[{"name":"r","files":["docs/x.md"]}],"findings":[{"source":"r","priority":"P3","finding":"Go test mapped twice","outcome":{"status":"fixed","body":"Mapped TestStep to step_rejects_overflow"}}]}"#,
    );
    run_render(&mut Vec::new(), &[dir_text(dir.path()), file]).expect("render");
    let md = read_file(dir.path().join(RECORD_FILE));
    assert!(
        md.contains(
            "| r | P3 | Go test mapped twice | fixed in the pull request body: Mapped TestStep to step_rejects_overflow |"
        ),
        "the table does not show the body fix:\n{md}"
    );
    assert!(
        read_file(dir.path().join(RECORD_JSON_FILE)).contains(
            r#""outcome":{"status":"fixed","body":"Mapped TestStep to step_rejects_overflow"}"#
        ),
        "the block does not carry the body fix"
    );
    assert_eq!(
        "fixed",
        parse_record(&md).expect("parse").findings[0].outcome.status
    );
}

#[test_case("success", "success")]
#[test_case("no-findings", "success")]
#[test_case("blocked", "blocked")]
fn render_verdict(name: &str, want: &str) {
    let (m, input) = render_case(name);
    assert_eq!(want, build_record(&m, input).expect("build").verdict);
}
