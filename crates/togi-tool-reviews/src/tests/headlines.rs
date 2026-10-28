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
    let got = shifted(&[Span { first, last }], &shifts(patch.as_bytes()).expect("shifts"))
        .expect("translate");
    assert_eq!(want, got.iter().map(|s| (s.first(), s.last())).collect::<Vec<_>>());
}

#[test_case(b"R100\0old\tfile.go\0new\nfile.go\0", "old\tfile.go", Some("new\nfile.go"); "rename with delimiters")]
#[test_case(b"D\0f.go\0", "f.go", None; "deletion")]
#[test_case(b"M\0f.go\0", "f.go", Some("f.go"); "modified")]
fn reads_head_destinations(raw: &[u8], source: &str, want: Option<&str>) {
    assert_eq!(Some(&want), destinations(raw).expect("paths").get(source));
}
