use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use test_case::test_case;

use super::*;
use crate::chain;
use crate::diff::LineRange;
use crate::fakes::{MemTree, linux_host};

const LIB: &str = "crates/a/src/lib.rs";

fn set(paths: &[&str]) -> HashSet<String> {
    paths.iter().map(|&path| path.to_owned()).collect()
}

#[test]
fn built_sources_are_the_dep_info_lines_ending_in_a_colon() {
    let tree = MemTree::new(
        "/repo",
        &[
            (
                "target/debug/deps/togi_a-1.d",
                "target/debug/deps/togi_a-1: crates/togi-a/src/lib.rs\n\ncrates/togi-a/src/lib.rs:\n",
            ),
            (
                "target/debug/build/togi-a-2/b.d",
                "target/debug/build/togi-a-2/b: crates/togi-a/build.rs\n\ncrates/togi-a/build.rs:\n",
            ),
            (
                "target/debug/deps/togi_b-3.d",
                "crates/with\\ space/src/lib.rs:\n/repo/crates/togi-b/src/lib.rs:\n/elsewhere/dep.rs:\n",
            ),
            (
                "target/debug/incremental/other.d",
                "crates/togi-c/src/lib.rs:\n",
            ),
            (
                "target/debug/deps/togi_a-1.rmeta",
                "crates/togi-d/src/lib.rs:\n",
            ),
        ],
    );
    let built = built_sources(&tree, Path::new("/repo/target/debug"), Path::new("/repo")).unwrap();
    assert_eq!(
        built,
        set(&[
            "crates/togi-a/src/lib.rs",
            "crates/togi-a/build.rs",
            "crates/with space/src/lib.rs",
            "crates/togi-b/src/lib.rs",
        ])
    );
}

#[test]
fn a_build_directory_without_dep_info_is_refused() {
    let tree = MemTree::new("/repo", &[("target/debug/incremental/other.d", "a.rs:\n")]);
    let error =
        built_sources(&tree, Path::new("/repo/target/debug"), Path::new("/repo")).unwrap_err();
    assert_eq!(
        chain(&error),
        "/repo/target/debug holds no dep-info files in deps or build/*"
    );
}

fn not_built_one(
    path: &str,
    src: &str,
    changed: &[(u32, u32)],
    built: &HashSet<String>,
    measured: &[(u32, bool)],
) -> Vec<String> {
    let tree = MemTree::new("/repo", &[(path, src)]);
    let changed = BTreeMap::from([(
        path.to_owned(),
        changed
            .iter()
            .map(|&(first, last)| LineRange::new(first, last))
            .collect(),
    )]);
    let measured = Measured::from([(path.to_owned(), measured.iter().copied().collect())]);
    not_built(
        &tree,
        Path::new("/repo"),
        &changed,
        built,
        &measured,
        &linux_host(),
    )
    .unwrap()
}

#[test_case("fn a() {}\n\n#[cfg(windows)]\nfn win() {\n    1;\n}\n", &[(5, 5)], &[(1, true)], true ; "an item whose cfg is false and that the trace does not measure")]
#[test_case("fn a() {}\n\n#[cfg(windows)]\nfn win() {\n    1;\n}\n", &[(1, 1)], &[(1, true)], false ; "a changed line outside the item")]
#[test_case("fn a() {}\n\n#[cfg(windows)]\nfn win() {\n    1;\n}\n", &[(4, 5)], &[(1, true), (5, false)], false ; "a false cfg item holding a measured line was compiled by some build")]
#[test_case("#[cfg(feature = \"x\")]\nconst X: u8 = 1;\nfn a() {}\n", &[(2, 2)], &[(3, true)], true ; "a cargo feature item with no measured line")]
#[test_case("#[cfg(feature = \"x\")]\nfn x() {\n    1;\n}\nfn a() {}\n", &[(3, 3)], &[(3, true), (5, true)], false ; "a cargo feature item with a measured line")]
#[test_case("#[cfg(my_build)]\nfn x() {\n    1;\n}\n", &[(3, 3)], &[(3, false)], false ; "a custom cfg item with a measured line")]
#[test_case("#[cfg(not(test))]\nfn release() {\n    1;\n}\n", &[(3, 3)], &[(3, true)], false ; "cfg not test code the unit test build compiles out but another build compiles")]
#[test_case("#[cfg(test)]\nmod tests {\n    fn t() {\n        1;\n    }\n}\n", &[(4, 4)], &[(4, true)], false ; "cfg test code is not claimed when the test build measures it")]
#[test_case("struct S {\n    #[cfg(windows)]\n    a: u32,\n    b: u32,\n}\nfn live() {\n    1;\n}\n", &[(3, 3)], &[(7, true)], true ; "a changed field the build leaves out beside a live field")]
#[test_case("struct S {\n    #[cfg(windows)]\n    a: u32,\n    b: u32,\n}\nfn live() {\n    1;\n}\n", &[(4, 4), (7, 7)], &[(7, true)], false ; "a changed live field after a cfg field")]
#[test_case("mod m {\n    #![cfg(windows)]\n    pub fn a() {}\n}\nfn live() {\n    1;\n}\n", &[(6, 6)], &[(6, true)], false ; "an inline module's inner cfg does not reach the code after the module")]
#[test_case("macro_rules! m {\n    () => {\n        #[cfg(windows)]\n        fn g() {}\n    };\n}\nfn live() {\n    1;\n}\n", &[(4, 8)], &[(8, true)], false ; "a cfg inside a macro token tree followed by a measured function")]
fn cfg_items(src: &str, changed: &[(u32, u32)], measured: &[(u32, bool)], left_out: bool) {
    let want: &[&str] = if left_out { &[LIB] } else { &[] };
    assert_eq!(
        not_built_one(LIB, src, changed, &set(&[LIB]), measured),
        want
    );
}

#[test_case(LIB, "mod b;\n#[cfg(test)]\nmod tests;\n", &[LIB, "crates/a/src/tests.rs"], false ; "a cfg test module the test build compiled from its own file")]
#[test_case(LIB, "#[cfg(test)]\npub(crate) mod tests;\n", &[LIB, "crates/a/src/tests/mod.rs"], false ; "a module in a directory")]
#[test_case(LIB, "#[cfg(test)]\nmod tests;\n", &[LIB], true ; "a cfg test module no dep-info names")]
#[test_case(LIB, "#[cfg(feature = \"x\")]\nmod x;\n", &[LIB, "crates/b/src/x.rs"], true ; "a feature module whose file another crate's dep-info names")]
#[test_case("crates/a/src/foo.rs", "#[cfg(test)]\nmod tests;\n", &["crates/a/src/foo.rs", "crates/a/src/tests.rs"], true ; "a module declared in a non-root file is not proven by a sibling file")]
#[test_case("crates/a/src/foo.rs", "#[cfg(test)]\nmod tests;\n", &["crates/a/src/foo.rs", "crates/a/src/foo/tests.rs"], false ; "a module declared in a non-root file found under the file's stem")]
#[test_case(LIB, "#[cfg(windows)]\n#[path = \"win.rs\"]\nmod imp;\n", &[LIB, "crates/a/src/imp.rs"], true ; "a module whose path attribute names another file")]
#[test_case(LIB, "#[cfg(windows)]\n#[cfg_attr(unix, path = \"x.rs\")]\nmod imp;\n", &[LIB, "crates/a/src/imp.rs"], true ; "a module whose cfg_attr path names another file")]
#[test_case(LIB, "mod outer {\n    #[cfg(feature = \"x\")]\n    mod tests;\n}\n", &[LIB, "crates/a/src/tests.rs"], true ; "a module declared inside an inline module is not proven by a top-level file")]
fn module_declarations(path: &str, src: &str, built: &[&str], left_out: bool) {
    let want: &[&str] = if left_out { &[path] } else { &[] };
    assert_eq!(not_built_one(path, src, &[(1, 3)], &set(built), &[]), want);
}

#[test]
fn files_no_dep_info_names_are_left_out_in_order() {
    let tree = MemTree::new(
        "/repo",
        &[
            ("crates/a/src/lib.rs", "fn a() {}\n"),
            ("crates/a/src/mac.rs", "fn only_mac() {}\n"),
            ("crates/a/src/untested.rs", "fn u() {}\n"),
        ],
    );
    let changed: Changed = [
        "crates/a/src/untested.rs",
        "crates/a/src/lib.rs",
        "crates/a/src/mac.rs",
    ]
    .into_iter()
    .map(|path| (path.to_owned(), vec![LineRange::new(1, 1)]))
    .collect();
    let measured = Measured::from([(
        "crates/a/src/mac.rs".to_owned(),
        BTreeMap::from([(1, true)]),
    )]);
    let got = not_built(
        &tree,
        Path::new("/repo"),
        &changed,
        &set(&["crates/a/src/lib.rs"]),
        &measured,
        &linux_host(),
    )
    .unwrap();
    assert_eq!(got, ["crates/a/src/mac.rs", "crates/a/src/untested.rs"]);
}

#[test]
fn an_unreadable_changed_source_is_refused() {
    let changed = Changed::from([(LIB.to_owned(), vec![LineRange::new(1, 1)])]);
    let error = not_built(
        &MemTree::default(),
        Path::new("/repo"),
        &changed,
        &set(&[LIB]),
        &Measured::new(),
        &linux_host(),
    )
    .unwrap_err();
    assert_eq!(chain(&error), format!("read {LIB}: entity not found"));
}
