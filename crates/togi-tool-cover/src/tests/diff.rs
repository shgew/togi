use std::collections::HashMap;

use test_case::test_case;

use super::*;
use crate::chain;

fn ranges(blocks: &[(&str, u32, u32)]) -> HashMap<String, Vec<LineRange>> {
    let mut out: HashMap<String, Vec<LineRange>> = HashMap::new();
    for &(path, first, last) in blocks {
        out.entry(path.to_owned())
            .or_default()
            .push(LineRange::new(first, last));
    }
    out
}

fn gaps(diff: &str, unreached: &[(&str, u32, u32)]) -> Vec<String> {
    let changed = parse_diff(diff.as_bytes()).unwrap();
    intersect(&changed, &ranges(unreached))
        .iter()
        .map(ToString::to_string)
        .collect()
}

#[test]
fn an_unreached_range_outside_the_hunks_is_no_gap() {
    let diff =
        "diff --git a.rs a.rs\n--- a.rs\n+++ a.rs\n@@ -10,0 +11 @@ fn f() {\n+    let y = 2;\n";
    assert_eq!(gaps(diff, &[("a.rs", 3, 5)]), Vec::<String>::new());
}

#[test]
fn only_the_changed_lines_of_a_partly_changed_range() {
    let diff = "diff --git a.rs a.rs\n--- a.rs\n+++ a.rs\n@@ -1,2 +1,4 @@\n+mod a;\n@@ -9 +11,2 @@ fn f() {\n+    let x = 1;\n+    let y = 2;\n@@ -30,0 +33 @@ fn g() {\n+    let z = 3;\n";
    assert_eq!(gaps(diff, &[("a.rs", 3, 20)]), ["a.rs:3-4", "a.rs:11-12"]);
}

#[test]
fn adjacent_ranges_merge_and_files_sort() {
    let diff = "diff --git b.rs b.rs\n--- b.rs\n+++ b.rs\n@@ -1,0 +1,10 @@\n+x\ndiff --git a.rs a.rs\n--- a.rs\n+++ a.rs\n@@ -2 +2 @@\n+y\n";
    let unreached = [
        ("b.rs", 3, 4),
        ("b.rs", 5, 6),
        ("b.rs", 8, 8),
        ("a.rs", 1, 2),
    ];
    assert_eq!(gaps(diff, &unreached), ["a.rs:2", "b.rs:3-6", "b.rs:8"]);
}

#[test]
fn deletions_add_no_lines() {
    let diff = "diff --git a.rs a.rs\n--- a.rs\n+++ a.rs\n@@ -4,2 +3,0 @@\n-    gone();\n-    gone2();\ndiff --git old.rs old.rs\ndeleted file mode 100644\n--- old.rs\n+++ /dev/null\n@@ -1,3 +0,0 @@\n-mod a;\n";
    assert_eq!(parse_diff(diff.as_bytes()).unwrap(), Changed::new());
}

#[test]
fn added_content_resembling_a_header_is_content() {
    let diff = "diff --git a.rs a.rs\n--- a.rs\n+++ a.rs\n@@ -0,0 +1,2 @@\n+++ b.rs\n+x\n";
    assert_eq!(gaps(diff, &[("a.rs", 1, 3)]), ["a.rs:1-2"]);
}

#[test_case("@@ -1 @@" ; "no new side")]
#[test_case("@@ -1 -2 @@" ; "new side without a plus")]
#[test_case("@@ -1 +x,2 @@" ; "start not a number")]
#[test_case("@@ -1 +1,y @@" ; "count not a number")]
#[test_case("@@ -1 +0,1 @@" ; "nonempty zero start")]
#[test_case("@@ -1 +4294967296 @@" ; "start overflow")]
#[test_case("@@ -1 +1,4294967296 @@" ; "count overflow")]
#[test_case("@@ -1 +4294967295,2 @@" ; "end overflow")]
fn malformed_hunk_headers_are_refused(header: &str) {
    let diff = format!("diff --git a.rs a.rs\n--- a.rs\n+++ a.rs\n{header}\n");
    assert_eq!(
        chain(&parse_diff(diff.as_bytes()).unwrap_err()),
        format!("malformed hunk header {header:?}")
    );
}

#[test_case(1, 1 ; "first line")]
#[test_case(u32::MAX, u32::MAX ; "last line")]
#[test_case(1, u32::MAX ; "full range")]
fn checked_spans_keep_exact_indices(first: u32, last: u32) {
    let count = last - first + 1;
    let diff = format!("diff --git a.rs a.rs\n--- a.rs\n+++ a.rs\n@@ -0,0 +{first},{count} @@\n");
    let changed = parse_diff(diff.as_bytes()).unwrap();
    let range = changed["a.rs"][0];
    assert_eq!(range, LineRange::new(first, last));
    assert_eq!(range.indices(), (first - 1) as usize..last as usize);
}
