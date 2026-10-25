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

fn sources(paths: &[&str]) -> Built {
    Built {
        sources: set(paths),
        tested: HashSet::new(),
    }
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
        built.sources,
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

#[test]
fn integration_tests_are_the_sources_their_own_dep_info_names() {
    let tree = MemTree::new(
        "/repo",
        &[
            ("crates/a/Cargo.toml", ""),
            (
                "target/debug/deps/day-1.d",
                "target/debug/deps/day-1: crates/a/tests/day.rs crates/a/tests/support/mod.rs\n\ncrates/a/tests/day.rs:\n\ncrates/a/tests/support/mod.rs:\n",
            ),
            (
                "target/debug/deps/multi-2.d",
                "crates/a/tests/multi/main.rs:\ncrates/a/tests/multi/util.rs:\n",
            ),
            (
                "target/debug/deps/a-3.d",
                "crates/a/src/lib.rs:\ncrates/a/tests/shared.rs:\n",
            ),
            (
                "target/debug/build/a-4/build_script_build-5.d",
                "crates/a/build.rs:\ncrates/a/tests/helper.rs:\n",
            ),
            (
                "target/debug/deps/util-6.d",
                "crates/a/tests/multi/util.rs:\ncrates/a/tests/multi/more.rs:\n",
            ),
        ],
    );
    let built = built_sources(&tree, Path::new("/repo/target/debug"), Path::new("/repo")).unwrap();
    assert_eq!(
        built.tested,
        set(&[
            "crates/a/tests/day.rs",
            "crates/a/tests/support/mod.rs",
            "crates/a/tests/multi/main.rs",
            "crates/a/tests/multi/util.rs",
        ])
    );
    assert!(built.sources.contains("crates/a/tests/helper.rs"));
    assert!(!built.tested.contains("crates/a/tests/helper.rs"));
    assert!(!built.tested.contains("crates/a/tests/multi/more.rs"));
}

fn not_built_one(
    path: &str,
    src: &str,
    changed: &[(u32, u32)],
    built: &Built,
    measured: &[(u32, bool)],
) -> Vec<String> {
    let tree = MemTree::new("/repo", &[(path, src), ("crates/a/Cargo.toml", "")]);
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
        not_built_one(LIB, src, changed, &sources(&[LIB]), measured),
        want
    );
}

const DAY: &str = "crates/a/tests/day.rs";
const GATED: &str = "#![cfg(test)]\n\nfn t() {\n    1;\n}\n";
const ITEM: &str = "#[cfg(test)]\nfn t() {\n    1;\n}\n";
const NOT_TEST: &str = "#[cfg(not(test))]\nfn t() {\n    1;\n}\n";
const MACOS: &str = "#[cfg(target_os = \"macos\")]\nfn t() {\n    1;\n}\n";
const HELPER: &str = "crates/a/tests/day/helpers.rs";

#[test_case(DAY, GATED, &[DAY], &[DAY], false ; "an integration test with an inner cfg test that its dep-info names")]
#[test_case(DAY, GATED, &[LIB], &[], true ; "an integration test with an inner cfg test that no dep-info names")]
#[test_case(DAY, ITEM, &[DAY], &[DAY], false ; "an integration test item under cfg test")]
#[test_case(HELPER, GATED, &[DAY, HELPER], &[DAY, HELPER], false ; "a module of an integration test")]
#[test_case(DAY, NOT_TEST, &[DAY], &[DAY], true ; "an integration test item under cfg not test is never compiled")]
#[test_case(DAY, MACOS, &[DAY], &[DAY], true ; "an integration test item for another target os")]
#[test_case(DAY, GATED, &[DAY], &[], true ; "a tests file only a non-test build names")]
#[test_case("crates/a/src/day.rs", GATED, &["crates/a/src/day.rs"], &[], true ; "a source file with an inner cfg test the trace does not measure")]
fn integration_tests_are_built_with_cfg_test(
    path: &str,
    src: &str,
    compiled: &[&str],
    tested: &[&str],
    left_out: bool,
) {
    let built = Built {
        sources: set(compiled),
        tested: set(tested),
    };
    let want: &[&str] = if left_out { &[path] } else { &[] };
    assert_eq!(not_built_one(path, src, &[(1, 5)], &built, &[]), want);
}

#[test_case(LIB, "mod b;\n#[cfg(test)]\nmod tests;\n", &[LIB, "crates/a/src/tests.rs"], false ; "a cfg test module the test build compiled from its own file")]
#[test_case(LIB, "#[cfg(test)]\npub(crate) mod tests;\n", &[LIB, "crates/a/src/tests/mod.rs"], false ; "a module in a directory")]
#[test_case(LIB, "#[cfg(test)]\nmod tests;\n", &[LIB], true ; "a cfg test module no dep-info names")]
#[test_case(LIB, "#[cfg(feature = \"x\")]\nmod x;\n", &[LIB, "crates/b/src/x.rs"], true ; "a feature module whose file another crate's dep-info names")]
#[test_case("crates/a/src/foo.rs", "#[cfg(test)]\nmod tests;\n", &["crates/a/src/foo.rs", "crates/a/src/tests.rs"], true ; "a module declared in a non-root file is not proven by a sibling file")]
#[test_case("crates/a/src/foo.rs", "#[cfg(test)]\nmod tests;\n", &["crates/a/src/foo.rs", "crates/a/src/foo/tests.rs"], false ; "a module declared in a non-root file found under the file's stem")]
#[test_case(LIB, "#[cfg(test)]\n#[path = \"tests/x.rs\"]\nmod tests;\n", &[LIB, "crates/a/src/tests/x.rs"], false ; "a plain path test module named by dep-info")]
#[test_case("crates/a/src/foo.rs", "#[cfg(test)]\n#[path = \"tests/x.rs\"]\nmod tests;\n", &["crates/a/src/foo.rs", "crates/a/src/tests/x.rs"], false ; "a path is relative to the declaring directory not its stem")]
#[test_case("crates/a/src/foo.rs", "#[cfg(test)]\n#[path = \"../tests/x.rs\"]\nmod tests;\n", &["crates/a/src/foo.rs", "crates/a/tests/x.rs"], false ; "a path containing parent components is normalized")]
#[test_case(LIB, "#[cfg(test)]\n#[path = r#\"tests/x.rs\"#]\nmod tests;\n", &[LIB, "crates/a/src/tests/x.rs"], false ; "a raw path string")]
#[test_case(LIB, "#[cfg(test)]\n#[path = \"tests/\\x78.rs\"]\nmod tests;\n", &[LIB, "crates/a/src/tests/x.rs"], false ; "an escaped path string")]
#[test_case("crates/a/src/build.rs", "#[cfg(test)]\nmod tests;\n", &["crates/a/src/build.rs", "crates/a/src/tests.rs"], true ; "a module named build is not a build-script root")]
#[test_case("crates/a/src/build.rs", "#[cfg(test)]\nmod tests;\n", &["crates/a/src/build.rs", "crates/a/src/build/tests.rs"], false ; "a module named build resolves under its stem")]
#[test_case("crates/a/src/tests/foo.rs", "#[cfg(test)]\nmod tests;\n", &["crates/a/src/tests/foo.rs", "crates/a/src/tests/tests.rs"], true ; "a unit-test directory is not an integration-test root")]
#[test_case("crates/a/src/tests/foo.rs", "#[cfg(test)]\nmod tests;\n", &["crates/a/src/tests/foo.rs", "crates/a/src/tests/foo/tests.rs"], false ; "a unit-test module resolves under its stem")]
#[test_case(LIB, "#[cfg(test)]\n#[cfg_attr(unix, path = \"a.rs\")]\n#[path = \"b.rs\"]\nmod tests;\n", &[LIB, "crates/a/src/b.rs"], true ; "a conditional path remains unproven beside a plain path")]
#[test_case(LIB, "#[cfg(windows)]\n#[cfg_attr(unix, path = \"x.rs\")]\nmod imp;\n", &[LIB, "crates/a/src/x.rs"], true ; "a conditional path target in dep-info is not proof")]
#[test_case("crates/a/build.rs", "#[cfg(test)]\nmod tests;\n", &["crates/a/build.rs", "crates/a/tests.rs"], false ; "a build script is a crate root")]
#[test_case("crates/a/tests/a.rs", "#[cfg(test)]\nmod tests;\n", &["crates/a/tests/a.rs", "crates/a/tests/tests.rs"], false ; "an integration test is a crate root")]
#[test_case("crates/a/src/bin/a.rs", "#[cfg(test)]\nmod tests;\n", &["crates/a/src/bin/a.rs", "crates/a/src/bin/tests.rs"], false ; "a binary is a crate root")]
#[test_case(LIB, "#[cfg(windows)]\n#[path = \"win.rs\"]\nmod imp;\n", &[LIB, "crates/a/src/imp.rs"], true ; "a module whose path attribute names another file")]
#[test_case(LIB, "#[cfg(windows)]\n#[cfg_attr(unix, path = \"x.rs\")]\nmod imp;\n", &[LIB, "crates/a/src/imp.rs"], true ; "a module whose cfg_attr path names another file")]
#[test_case(LIB, "mod outer {\n    #[cfg(feature = \"x\")]\n    mod tests;\n}\n", &[LIB, "crates/a/src/tests.rs"], true ; "a module declared inside an inline module is not proven by a top-level file")]
fn module_declarations(path: &str, src: &str, built: &[&str], left_out: bool) {
    let want: &[&str] = if left_out { &[path] } else { &[] };
    assert_eq!(
        not_built_one(path, src, &[(1, 3)], &sources(built), &[]),
        want
    );
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
        &sources(&["crates/a/src/lib.rs"]),
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
        &sources(&[LIB]),
        &Measured::new(),
        &linux_host(),
    )
    .unwrap_err();
    assert_eq!(chain(&error), format!("read {LIB}: entity not found"));
}
