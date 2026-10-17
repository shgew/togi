use test_case::test_case;

use super::{base, clean, join};

#[test_case("", "."; "empty")]
#[test_case("a/b/../c", "a/c"; "inner parent")]
#[test_case("../x", "../x"; "leading parent kept")]
#[test_case("a/../..", ".."; "parent past the start")]
#[test_case("../../a/..", "../.."; "parents stack")]
#[test_case("/..", "/"; "parent of the root")]
#[test_case("/a/../../b", "/b"; "rooted parents stop at the root")]
#[test_case("a//./b/", "a/b"; "empty and current elements")]
#[test_case("/", "/"; "root")]
fn clean_like_go(path: &str, want: &str) {
    assert_eq!(want, clean(path), "clean({path:?})");
}

#[test_case("", "x", "x"; "no directory")]
#[test_case("/tmp/togi-review/7-abc", "record.md", "/tmp/togi-review/7-abc/record.md"; "file in a directory")]
#[test_case("../dir/", "record.md", "../dir/record.md"; "relative directory")]
#[test_case("dir", "../record.md", "record.md"; "name climbs out")]
fn join_like_go(dir: &str, name: &str, want: &str) {
    assert_eq!(want, join(dir, name), "join({dir:?}, {name:?})");
}

#[test_case("/tmp/togi-review/7-abc", "7-abc"; "last element")]
#[test_case("dir/", "dir"; "trailing slash")]
#[test_case("/", "/"; "root")]
fn base_like_go(path: &str, want: &str) {
    assert_eq!(want, base(path), "base({path:?})");
}
