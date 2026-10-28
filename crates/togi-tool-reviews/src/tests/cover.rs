use std::fmt::Write as _;

use test_case::test_case;

use super::{add_cover, intersect_cover};
use crate::diff::{FileDiff, Span, parse_diff};
use crate::headlines::{HeadLines, from_head_diff};
use crate::manifest::{CoverRange, NotBuilt, read_manifest, write_manifest};
use crate::render::tests::{dir_text, first_manifest, included, range, reviewer};
use crate::render::{FindingsInput, build_record};
use crate::snapshot::print_summary;

/// One patch of a delta's hunk file: it adds lines `first..=last` of `path`.
fn added_section(path: &str, first: i64, last: i64) -> String {
    let mut b = format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n");
    let _ = writeln!(b, "@@ -{},0 +{first},{} @@", first - 1, last - first + 1);
    for _ in first..=last {
        b.push_str("+line\n");
    }
    b
}

/// A snapshot directory whose one included file `a.go` has the sections concatenated in its hunk file, as a
/// delta's does.
fn cover_snapshot(sections: &[String]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("temporary directory");
    std::fs::create_dir_all(dir.path().join("files")).expect("mkdir");
    std::fs::write(dir.path().join("files/001.diff"), sections.concat()).expect("write hunks");
    let mut m = first_manifest(vec![included("a.go", 0, 0)]);
    let files = parse_diff(sections.concat().as_bytes()).expect("parse head diff");
    m.head_lines = from_head_diff(files.iter());
    write_manifest(&dir_text(dir.path()), &m).expect("write manifest");
    dir
}

#[test]
fn cover_merges_overlapping_delta_hunks() {
    let tmp = cover_snapshot(&[added_section("a.go", 10, 20), added_section("a.go", 15, 25)]);
    let dir = dir_text(tmp.path());
    let m = add_cover(&dir, "a.go:10-25\n").expect("add cover");
    let coverage = m.coverage.as_ref().expect("coverage");
    assert_eq!(vec![range("a.go", 10, 25)], coverage.ranges);
    assert_eq!(1, coverage.reported);
    let stored = read_manifest(&dir).expect("read manifest");
    assert_eq!(m.coverage, stored.coverage, "stored coverage");

    let mut summary = Vec::new();
    print_summary(&mut summary, &dir, &stored).expect("summary");
    let summary = String::from_utf8_lossy(&summary);
    assert!(
        summary.contains("uncovered: 1 of 1 reported ranges inside the hunks")
            && summary.matches("  a.go:").count() == 1,
        "summary = {summary:?}"
    );

    let r = build_record(
        &stored,
        FindingsInput {
            reviewers: vec![reviewer("r", None, &["a.go"])],
            coverage_judgment: "Judged.".to_owned(),
            ..FindingsInput::default()
        },
    )
    .expect("build record");
    assert_eq!(
        Some(1),
        r.coverage.as_ref().map(|c| c.uncovered),
        "record coverage"
    );
    let md = r.markdown();
    assert!(
        md.contains("- Coverage: 1 uncovered range. Judged."),
        "markdown lacks a single uncovered range:\n{md}"
    );
}

fn hunks(spans: &[(&str, &[(i64, i64)])]) -> HeadLines {
    let files: Vec<FileDiff> = spans
        .iter()
        .map(|&(path, spans)| FileDiff {
            path: path.to_owned(),
            changed: spans
                .iter()
                .map(|&(first, last)| Span { first, last })
                .collect(),
            ..FileDiff::default()
        })
        .collect();
    from_head_diff(files.iter())
}

#[test_case(&[("a.go", &[(10, 20), (15, 25)])], &[range("a.go", 10, 25)], &[range("a.go", 10, 25)], 1; "overlapping hunks from two patches")]
#[test_case(&[("a.go", &[(10, 20), (10, 20)])], &[range("a.go", 12, 18)], &[range("a.go", 12, 18)], 1; "exact duplicate hunks")]
#[test_case(&[("a.go", &[(10, 30), (15, 20)])], &[range("a.go", 5, 40)], &[range("a.go", 10, 30)], 1; "nested hunk")]
#[test_case(&[("a.go", &[(10, 20), (30, 40)])], &[range("a.go", 10, 40)], &[range("a.go", 10, 20), range("a.go", 30, 40)], 1; "disjoint hunks stay separate")]
#[test_case(&[("a.go", &[(10, 20), (21, 25)])], &[range("a.go", 10, 25)], &[range("a.go", 10, 20), range("a.go", 21, 25)], 1; "adjacent intervals stay separate")]
#[test_case(&[("a.go", &[(10, 20), (20, 25)])], &[range("a.go", 10, 25)], &[range("a.go", 10, 25)], 1; "touching on one line merges")]
#[test_case(&[("a.go", &[(10, 30)])], &[range("a.go", 10, 20), range("a.go", 15, 25)], &[range("a.go", 10, 25)], 2; "overlapping reported ranges")]
#[test_case(&[("a.go", &[(10, 20)]), ("b.go", &[(15, 25)])], &[range("a.go", 10, 20), range("b.go", 15, 25)], &[range("a.go", 10, 20), range("b.go", 15, 25)], 2; "other paths do not merge")]
fn intersect_cover_merges_per_path(
    spans: &[(&str, &[(i64, i64)])],
    ranges: &[CoverRange],
    want: &[CoverRange],
    reported: i64,
) {
    let not_built = |path: &str| NotBuilt {
        path: path.to_owned(),
        target: "darwin/arm64".to_owned(),
    };
    let got = intersect_cover(
        ranges,
        &[not_built("a.go"), not_built("z.go")],
        &hunks(spans),
    );
    assert_eq!(want, got.ranges);
    assert_eq!(reported, got.reported);
    assert_eq!(vec![not_built("a.go")], got.not_built);
}

#[test]
fn refuses_snapshot_before_head_coordinates() {
    let dir = tempfile::tempdir().expect("temporary directory");
    std::fs::write(dir.path().join("manifest.json"), r#"{"version":1}"#)
        .expect("write old manifest");
    let path = dir_text(dir.path());
    assert_eq!(
        format!("manifest in {path} has version 1; this tool reads version 2"),
        add_cover(&path, "a.go:1\n").expect_err("old snapshot accepted").to_string(),
    );
}
