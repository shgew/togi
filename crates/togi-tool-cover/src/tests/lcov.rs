use std::collections::HashMap;
use std::path::Path;

use test_case::test_case;

use super::*;
use crate::chain;

#[test]
fn unreached_lines_join_into_ranges_across_unmeasured_lines() {
    let trace = "TN:\nSF:/repo/crates/togi-a/src/lib.rs\nFN:3,f\nDA:3,1\nDA:4,0\nDA:5,0\nDA:7,0\nDA:8,2\nDA:10,0\nDA:10,3\nend_of_record\nSF:/elsewhere/dep.rs\nDA:1,0\nend_of_record\n";
    let files = parse_lcov(trace, Path::new("/repo")).unwrap();
    assert_eq!(
        unreached(&files),
        HashMap::from([(
            "crates/togi-a/src/lib.rs".to_owned(),
            vec![LineRange::new(4, 7)]
        )])
    );
}

#[test]
fn lines_after_the_end_of_a_record_are_ignored() {
    let trace = "SF:/repo/a.rs\nDA:1,0\nend_of_record\nDA:2,0\n";
    let files = parse_lcov(trace, Path::new("/repo")).unwrap();
    assert_eq!(files["a.rs"].keys().copied().collect::<Vec<_>>(), [1]);
}

#[test_case("DA:x,1" ; "line not a number")]
#[test_case("DA:1,y" ; "count not a number")]
#[test_case("DA:1" ; "no count")]
fn malformed_da_lines_are_refused(line: &str) {
    let trace = format!("SF:/repo/a.rs\n{line}\n");
    assert_eq!(
        chain(&parse_lcov(&trace, Path::new("/repo")).unwrap_err()),
        format!("malformed lcov line {line:?}")
    );
}

#[test_case("/repo", "/repo/a/../b.rs", Some("b.rs") ; "cleaned")]
#[test_case("/repo", "/repo", None ; "the root itself")]
#[test_case("/repo", "/repository/a.rs", None ; "a sibling with the root as a prefix")]
#[test_case("/repo", "/repo/../x.rs", None ; "escaping the root")]
#[test_case("/repo", "crates/a.rs", Some("crates/a.rs") ; "relative")]
fn relative_paths(root: &str, path: &str, want: Option<&str>) {
    assert_eq!(relative(Path::new(root), Path::new(path)).as_deref(), want);
}
