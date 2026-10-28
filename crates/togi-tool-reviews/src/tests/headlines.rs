use test_case::test_case;

use super::*;

#[test_case(5, 5, "@@ -1,0 +2,2 @@\n", &[(7, 7)]; "insertion above")]
#[test_case(5, 5, "@@ -1,2 +0,0 @@\n", &[(3, 3)]; "deletion above")]
#[test_case(5, 5, "@@ -5 +5 @@\n", &[]; "changed later")]
#[test_case(5, 5, "@@ -5 +4,0 @@\n", &[]; "deleted later")]
#[test_case(10, 10, "@@ -1,0 +2,2 @@\n@@ -4,2 +5,0 @@\n@@ -8 +8,3 @@\n", &[(12, 12)]; "several hunks")]
#[test_case(3, 4, "@@ -3,0 +4,2 @@\n", &[(3, 3), (6, 6)]; "pure insertion boundary")]
#[test_case(3, 7, "@@ -4,2 +4 @@\n", &[(3, 3), (5, 6)]; "replacement splits interval")]
#[test_case(2, 3, "@@ -7 +7,2 @@\n", &[(2, 3)]; "later hunk leaves earlier span")]
fn maps_surviving_lines(first: i64, last: i64, patch: &str, want: &[(i64, i64)]) {
    let got = shifted(
        &[Span { first, last }],
        &shifts(patch.as_bytes()).expect("shifts"),
    )
    .expect("translate");
    assert_eq!(
        want,
        got.iter()
            .map(|s| (s.first(), s.last()))
            .collect::<Vec<_>>()
    );
}

#[test_case(b"R100\0old\tfile.go\0new\nfile.go\0", "old\tfile.go", Some("new\nfile.go"); "rename with delimiters")]
#[test_case(b"D\0f.go\0", "f.go", None; "deletion")]
#[test_case(b"M\0f.go\0", "f.go", Some("f.go"); "modified")]
fn reads_head_destinations(raw: &[u8], source: &str, want: Option<&str>) {
    assert_eq!(Some(&want), destinations(raw).expect("paths").get(source));
}

#[test_case(b"C100\0f.go\0copy.go\0", &[]; "copy leaves source unchanged")]
#[test_case(b"C100\0f.go\0copy.go\0M\0f.go\0", &[("f.go", Some("f.go"))]; "copy and modified source")]
fn copies_do_not_redirect_source(raw: &[u8], want: &[(&str, Option<&str>)]) {
    assert_eq!(
        want.iter().copied().collect::<HashMap<_, _>>(),
        destinations(raw).expect("paths"),
    );
}

const SPLIT_RENAME: &str = "diff --git a/g.go b/g.go
new file mode 100644
--- /dev/null
+++ b/g.go
@@ -0,0 +1,3 @@
+one
+two
+three
diff --git a/f.go b/f.go
deleted file mode 100644
--- a/f.go
+++ /dev/null
@@ -1,3 +0,0 @@
-one
-two
-three
";

const PURE_RENAME_WITH_OTHER_ADDITION: &str = "diff --git a/other.go b/other.go
new file mode 100644
--- /dev/null
+++ b/other.go
@@ -0,0 +1 @@
+other
diff --git a/f.go b/g.go
similarity index 100%
rename from f.go
rename to g.go
";

#[test_case(SPLIT_RENAME, "f.go", &[(1, 3, 0)]; "split rename ignores new file insertion")]
#[test_case(PURE_RENAME_WITH_OTHER_ADDITION, "f.go", &[]; "pure rename ignores unrelated hunks")]
#[test_case("diff --git a/f.go b/f.go\nold mode 100644\nnew mode 100755\n", "f.go", &[]; "mode change is identity")]
#[test_case("diff --git a/f.go b/f.go\n--- a/f.go\n+++ b/f.go\n@@ -0,0 +1 @@\n+new\n", "f.go", &[(0, 0, 1)]; "modified file uses current path")]
fn selects_only_source_section(patch: &str, source: &str, want: &[(i64, i64, i64)]) {
    let got = source_shifts(patch.as_bytes(), source).expect("source shifts");
    assert_eq!(
        want,
        got.iter()
            .map(|s| (s.old, s.removed, s.added))
            .collect::<Vec<_>>(),
    );
}

#[test_case(""; "empty diff contradicts name status")]
#[test_case(PURE_RENAME_WITH_OTHER_ADDITION; "missing source section")]
fn refuses_missing_source_section(patch: &str) {
    let error = source_shifts(patch.as_bytes(), "missing.go")
        .err()
        .expect("missing source section accepted");
    assert_eq!(
        "find source section for missing.go in head-coordinate diff",
        error.to_string(),
    );
}
