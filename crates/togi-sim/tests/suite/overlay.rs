// Machine overlays.

use std::path::{Path, PathBuf};
use togi_sim::file::{self, LoadError};
use togi_sim::{MachineDefinition, Model};

use test_case::test_case;

use crate::common::machine_dir_text;

const AVX2: &str = "mprime-avx2-36k-248k-allcore";
const AVX512: &str = "mprime-avx512-36k-248k-allcore";

fn real_write(path: &Path, content: &str) -> PathBuf {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("create the directory");
    std::fs::write(path, content).expect("write the machine");
    path.to_owned()
}

/// The shared-voltage example machine with extra members spliced in after its opening brace.
fn write_parent(dir: &crate::common::MemoryFiles, path: &Path, extra: &str) -> PathBuf {
    let text = machine_dir_text("shared-voltage.json");
    let text = if extra.is_empty() {
        text
    } else {
        format!(
            "{{{extra}{}",
            text.trim_start().strip_prefix('{').expect("an object")
        )
    };
    dir.write(path, &text)
}

fn overlay(extends: &str, entries: &str) -> String {
    format!(
        r#"{{"description": "fixture overlay", "extends": {extends:?}, "shared_voltage_override": [{entries}]}}"#
    )
}

fn load(dir: &crate::common::MemoryFiles, path: &Path) -> MachineDefinition {
    file::load_from(path, dir).expect("loads").0
}

fn entry(workload: &str, core: i64, rest: &str) -> String {
    format!(r#"{{"workload": {workload:?}, "core": {core}{rest}}}"#)
}

#[test]
fn an_overlay_is_its_parent_plus_the_named_fields() {
    let dir = crate::common::MemoryFiles::default();
    let parent = write_parent(
        &dir,
        &dir.path().join("parent.json"),
        r#""facts": "facts.gz","#,
    );
    let leaf = dir.write(
        &dir.path().join("leaf.json"),
        &overlay(
            "parent.json",
            &format!(
                "{}, {}, {}",
                entry(AVX2, 5, r#", "threshold_clock_v_per_100mhz": 0.06"#),
                entry(AVX512, 9, r#", "threshold_v": 1.18"#),
                entry(
                    AVX512,
                    10,
                    r#", "threshold_v": 1.22, "threshold_clock_v_per_100mhz": 0.05"#
                ),
            ),
        ),
    );
    let mut want = load(&dir, &parent);
    {
        let rail = want.shared_voltage.as_mut().expect("a rail");
        rail.workload.get_mut(AVX2).expect("avx2").core[5].threshold_clock_v_per_100mhz = 0.06;
        let avx512 = rail.workload.get_mut(AVX512).expect("avx512");
        avx512.core[9].threshold_v = 1.18;
        avx512.core[10].threshold_v = 1.22;
        avx512.core[10].threshold_clock_v_per_100mhz = 0.05;
    }
    want.description = "fixture overlay".to_owned();
    want.notes.clear();

    let (got, files) = file::load_from(&leaf, &dir).expect("loads");
    assert_eq!(got, want);
    assert_eq!(files, [leaf.clone(), parent.clone()], "leaf first");
    assert_eq!(got, load(&dir, &leaf), "repeated loads agree");
    assert!(Model::new(&got, 1).is_ok());
    let untouched = load(&dir, &parent);
    assert_ne!(
        untouched.shared_voltage.expect("a rail").workload[AVX2].core[5]
            .threshold_clock_v_per_100mhz,
        0.06,
        "loading the overlay left its parent's machine alone"
    );
}

#[test]
fn an_overlay_follows_its_parent() {
    let dir = crate::common::MemoryFiles::default();
    let parent = dir.path().join("parent.json");
    write_parent(&dir, &parent, "");
    let leaf = dir.write(
        &dir.path().join("leaf.json"),
        &overlay("parent.json", &entry(AVX2, 5, r#", "threshold_v": 1.1"#)),
    );
    assert!(!load(&dir, &leaf).old_kernel);
    write_parent(&dir, &parent, r#""old_kernel": true,"#);
    let got = load(&dir, &leaf);
    assert!(got.old_kernel, "the parent's later change flows through");
    assert_eq!(
        got.shared_voltage.expect("a rail").workload[AVX2].core[5].threshold_v,
        1.1
    );
}

#[test]
fn a_chain_loads_leaf_first_and_the_last_override_wins() {
    let dir = crate::common::MemoryFiles::default();
    let root = write_parent(&dir, &dir.path().join("root.json"), "");
    let mid = dir.write(
        &dir.path().join("mid.json"),
        &overlay(
            "root.json",
            &format!(
                "{}, {}",
                entry(
                    AVX2,
                    5,
                    r#", "threshold_v": 1.05, "threshold_clock_v_per_100mhz": 0.03"#
                ),
                entry(AVX2, 6, r#", "threshold_v": 1.06"#),
            ),
        ),
    );
    let leaf = dir.write(
        &dir.path().join("leaf.json"),
        &overlay(
            "mid.json",
            &entry(AVX2, 5, r#", "threshold_clock_v_per_100mhz": 0.06"#),
        ),
    );
    let (got, files) = file::load_from(&leaf, &dir).expect("loads");
    assert_eq!(files, [leaf, mid, root]);
    let cores = &got.shared_voltage.expect("a rail").workload[AVX2].core;
    assert_eq!(cores[5].threshold_v, 1.05, "the middle overlay's threshold");
    assert_eq!(
        cores[5].threshold_clock_v_per_100mhz, 0.06,
        "the leaf's coefficient"
    );
    assert_eq!(
        cores[6].threshold_v, 1.06,
        "the leaf keeps the middle overlay's override"
    );
}

#[test_case("leaf.json", "shared/parent.json"; "child directory")]
#[test_case("other/leaf.json", "../shared/parent.json"; "sibling directory")]
#[test_case("a/b/leaf.json", "./../../shared/parent.json"; "dot segments")]
fn the_parent_is_looked_up_relative_to_the_overlay(leaf: &str, extends: &str) {
    let dir = crate::common::MemoryFiles::default();
    let parent = write_parent(&dir, &dir.path().join("shared/parent.json"), "");
    let one = entry(AVX2, 5, r#", "threshold_v": 1.1"#);
    let leaf = dir.write(&dir.path().join(leaf), &overlay(extends, &one));
    let (_, files) = file::load_from(&leaf, &dir).expect("loads");
    assert_eq!(files, [leaf, parent]);
}

fn facts_parent(dir: &crate::common::MemoryFiles) {
    write_parent(
        dir,
        &dir.path().join("machines/parent.json"),
        r#""facts": "../facts/extract.jsonl.gz","#,
    );
}

#[test_case("machines/leaf.json", "parent.json", "../facts/extract.jsonl.gz"; "same directory keeps the value")]
#[test_case("leaf.json", "machines/parent.json", "facts/extract.jsonl.gz"; "parent directory")]
#[test_case("overlays/leaf.json", "../machines/parent.json", "../facts/extract.jsonl.gz"; "sibling directory")]
#[test_case("x/y/leaf.json", "../../machines/parent.json", "../../facts/extract.jsonl.gz"; "deeper directory")]
fn inherited_facts_resolve_to_the_same_extract_from_the_overlay(
    leaf: &str,
    extends: &str,
    facts: &str,
) {
    let dir = crate::common::MemoryFiles::default();
    facts_parent(&dir);
    let one = entry(AVX2, 5, r#", "threshold_v": 1.1"#);
    let leaf = dir.write(&dir.path().join(leaf), &overlay(extends, &one));
    let got = load(&dir, &leaf).facts.expect("facts");
    assert_eq!(got, facts);
    let resolved = leaf.parent().expect("a directory").join(&got);
    assert_eq!(
        normalized(&resolved),
        dir.path().join("facts/extract.jsonl.gz")
    );
}

#[test]
fn inherited_facts_resolve_through_a_chain_of_overlays() {
    let dir = crate::common::MemoryFiles::default();
    facts_parent(&dir);
    let one = entry(AVX2, 5, r#", "threshold_v": 1.1"#);
    let mid = dir.write(
        &dir.path().join("mid/mid.json"),
        &overlay("../machines/parent.json", &one),
    );
    let leaf = dir.write(
        &dir.path().join("a/b/leaf.json"),
        &overlay(
            "../../mid/mid.json",
            &entry(
                AVX2,
                5,
                r#", "threshold_v": 1.1, "threshold_clock_v_per_100mhz": 0.02"#,
            ),
        ),
    );
    assert_eq!(
        load(&dir, &mid).facts.as_deref(),
        Some("../facts/extract.jsonl.gz")
    );
    assert_eq!(
        load(&dir, &leaf).facts.as_deref(),
        Some("../../facts/extract.jsonl.gz")
    );
}

fn normalized(path: &Path) -> PathBuf {
    let mut parts: Vec<std::path::Component<'_>> = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::CurDir => {}
            other @ (std::path::Component::Prefix(_)
            | std::path::Component::RootDir
            | std::path::Component::Normal(_)) => parts.push(other),
        }
    }
    parts.into_iter().collect()
}

#[cfg(unix)]
#[test]
fn real_fs_inherited_facts_through_a_symlinked_directory_still_name_the_same_extract() {
    let dir = tempfile::tempdir().expect("temporary directory");
    std::fs::create_dir_all(dir.path().join("elsewhere/deep")).expect("create the link target");
    let text = machine_dir_text("shared-voltage.json").replacen(
        '{',
        r#"{"facts": "alias/../extract.jsonl.gz","#,
        1,
    );
    real_write(&dir.path().join("machines/parent.json"), &text);
    std::os::unix::fs::symlink(
        dir.path().join("elsewhere/deep"),
        dir.path().join("machines/alias"),
    )
    .expect("link a directory");
    let leaf = real_write(
        &dir.path().join("overlays/leaf.json"),
        &overlay(
            "../machines/parent.json",
            &entry(AVX2, 5, r#", "threshold_v": 1.1"#),
        ),
    );
    let got = file::load(&leaf).expect("loads").facts.expect("facts");
    // The file system resolves `alias/..` to the link target's parent, not to `machines`.
    let directory_of = |base: &Path, facts: &str| {
        let path = base.join(facts);
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("extract.jsonl.gz")
        );
        std::fs::canonicalize(path.parent().expect("a directory")).expect("an existing directory")
    };
    assert_eq!(
        directory_of(&dir.path().join("overlays"), &got),
        directory_of(&dir.path().join("machines"), "alias/../extract.jsonl.gz"),
    );
    assert_eq!(
        directory_of(&dir.path().join("overlays"), &got),
        std::fs::canonicalize(dir.path().join("elsewhere")).expect("the link target's parent"),
    );
}

#[test]
fn an_overlay_of_a_parent_without_facts_has_none() {
    let dir = crate::common::MemoryFiles::default();
    write_parent(&dir, &dir.path().join("parent.json"), "");
    let leaf = dir.write(
        &dir.path().join("x/leaf.json"),
        &overlay("../parent.json", &entry(AVX2, 5, r#", "threshold_v": 1.1"#)),
    );
    assert_eq!(load(&dir, &leaf).facts, None);
}

#[test]
fn a_full_machine_lists_only_itself_and_a_missing_one_is_an_error() {
    let dir = crate::common::MemoryFiles::default();
    let path = write_parent(&dir, &dir.path().join("full.json"), "");
    let (machine, files) = file::load_from(&path, &dir).expect("loads");
    assert_eq!(files, vec![path.clone()]);
    assert_eq!(machine, load(&dir, &path));
    let missing = file::load_from(dir.path().join("missing.json"), &dir);
    assert!(matches!(missing, Err(LoadError::Read { .. })));
}

#[test_case("self.json"; "self")]
#[test_case("a.json"; "two files")]
#[test_case("c.json"; "overlay of a cycle")]
fn a_cycle_is_an_error_however_it_is_spelled(name: &str) {
    let dir = crate::common::MemoryFiles::default();
    let one = entry(AVX2, 5, r#", "threshold_v": 1.1"#);
    let at = |name: &str, extends: &str| dir.write(&dir.path().join(name), &overlay(extends, &one));
    at("self.json", "self.json");
    at("a.json", "b.json");
    at("b.json", "sub/../a.json");
    at("c.json", "a.json");
    let error = file::load_from(dir.path().join(name), &dir).expect_err("a cycle");
    let message = togi_machine::error_chain(&error);
    assert!(message.contains("cycle"), "{message}");
}

#[cfg(unix)]
#[test_case("leaf.json"; "through one link")]
#[test_case("deep.json"; "through a link twice")]
fn real_fs_cycles_through_symlinked_directories_are_refused(name: &str) {
    let dir = tempfile::tempdir().expect("temporary directory");
    let loop_dir = dir.path().join("loop");
    std::fs::create_dir_all(&loop_dir).expect("create the directory");
    std::os::unix::fs::symlink(".", loop_dir.join("alias")).expect("a symlink");
    let one = entry(AVX2, 5, r#", "threshold_v": 1.1"#);
    real_write(
        &loop_dir.join("leaf.json"),
        &overlay("alias/leaf.json", &one),
    );
    real_write(
        &loop_dir.join("deep.json"),
        &overlay("alias/alias/leaf.json", &one),
    );
    let error = file::load_with_files(loop_dir.join(name)).expect_err("symlink cycle");
    let message = togi_machine::error_chain(&error);
    assert!(message.contains("cycle"), "{message}");
}

fn valid() -> String {
    entry(AVX2, 5, r#", "threshold_v": 1.1"#)
}

fn with(entries: &str) -> String {
    overlay("parent.json", entries)
}

fn mixed(member: &str) -> String {
    format!(
        r#"{{{member}, "extends": "parent.json", "shared_voltage_override": [{}]}}"#,
        valid()
    )
}

#[test_case(&mixed(r#""cores": 16"#), "unknown field"; "mixed with full machine fields")]
#[test_case(&mixed(r#""cores": 0"#), "unknown field"; "mixed with explicit zero cores")]
#[test_case(&mixed(r#""old_kernel": false"#), "unknown field"; "mixed with explicit false old_kernel")]
#[test_case(&mixed(r#""model": {}"#), "unknown field"; "mixed with empty model")]
#[test_case(&mixed(r#""facts": null"#), "unknown field"; "mixed with null member")]
#[test_case(
    &format!(r#"{{"extends": "parent.json", "shared_voltage": {{}}, "shared_voltage_override": [{}]}}"#, valid()),
    "unknown field";
    "mixed with shared_voltage"
)]
#[test_case(&format!(r#"{{"shared_voltage_override": [{}]}}"#, valid()), "extends is missing"; "override without extends")]
#[test_case(&format!(r#"{{"extends": "", "shared_voltage_override": [{}]}}"#, valid()), "extends is missing"; "empty extends")]
#[test_case(&format!(r#"{{"extends": null, "shared_voltage_override": [{}]}}"#, valid()), "extends is missing"; "null extends")]
#[test_case(r#"{"extends": ""}"#, "extends is missing"; "empty extends without override")]
#[test_case(r#"{"extends": null}"#, "extends is missing"; "null extends without override")]
#[test_case(r#"{"extends": "parent.json"}"#, "changes nothing"; "extends without override")]
#[test_case(r#"{"extends": "parent.json", "shared_voltage_override": null}"#, "changes nothing"; "null override")]
#[test_case(r#"{"shared_voltage_override": null}"#, "extends is missing"; "null override without extends")]
#[test_case(r#"{"extends": "parent.json", "shared_voltage_override": []}"#, "changes nothing"; "empty override")]
#[test_case(
    &format!(r#"{{"extends": "/fixture/parent.json", "shared_voltage_override": [{}]}}"#, valid()),
    "must be relative";
    "absolute extends"
)]
#[test_case(&overlay("absent.json", &valid()), "absent.json"; "missing parent")]
#[test_case(&overlay("broken.json", &valid()), "broken.json"; "broken parent")]
#[test_case(&overlay("plain.json", &valid()), "no shared_voltage"; "parent without shared_voltage")]
#[test_case(&with(&entry("nope", 5, r#", "threshold_v": 1.1"#)), "unknown workload \"nope\""; "unknown workload")]
#[test_case(&with(r#"{"core": 5, "threshold_v": 1.1}"#), "unknown workload \"\""; "missing workload")]
#[test_case(&with(&entry(AVX2, 16, r#", "threshold_v": 1.1"#)), "unknown core 16"; "core past the end")]
#[test_case(&with(&entry(AVX2, -1, r#", "threshold_v": 1.1"#)), "unknown core -1"; "negative core")]
#[test_case(&with(&format!(r#"{{"workload": {AVX2:?}, "threshold_v": 1.1}}"#)), "needs core"; "missing core")]
#[test_case(
    &with(&format!(r#"{{"workload": {AVX2:?}, "core": 5.5, "threshold_v": 1.1}}"#)),
    "invalid type";
    "fractional core"
)]
#[test_case(&with(&entry(AVX2, 5, "")), "sets neither"; "no effective field")]
#[test_case(
    &with(&format!("{}, {}", valid(), entry(AVX2, 5, r#", "threshold_clock_v_per_100mhz": 0.06"#))),
    "core 5";
    "duplicate target"
)]
#[test_case(&with(&entry(AVX2, 5, r#", "threshold_v": 0"#)), "threshold_v"; "zero threshold")]
#[test_case(&with(&entry(AVX2, 5, r#", "threshold_v": -1"#)), "threshold_v"; "negative threshold")]
#[test_case(
    &with(&entry(AVX2, 5, r#", "threshold_clock_v_per_100mhz": -0.01"#)),
    "threshold_clock_v_per_100mhz";
    "negative clock coefficient"
)]
#[test_case(&with(&entry(AVX2, 5, r#", "base_v": 1.1"#)), "unknown field"; "unknown entry member")]
#[test_case(
    &format!(r#"{{"extends": "parent.json", "patch": [], "shared_voltage_override": [{}]}}"#, valid()),
    "unknown field";
    "unknown overlay member"
)]
#[test_case(&with(&entry(AVX2, 5, r#", "threshold_v": null"#)), "sets neither"; "null threshold only")]
fn malformed_overlays_are_refused_with_the_file_named(content: &str, want: &str) {
    let dir = crate::common::MemoryFiles::default();
    let parent = write_parent(&dir, &dir.path().join("parent.json"), "");
    assert_eq!(parent, Path::new("/fixture/parent.json"));
    dir.write(&dir.path().join("plain.json"), r#"{"cores": 16}"#);
    dir.write(&dir.path().join("broken.json"), r#"{"cores": ["#);
    let leaf = dir.write(&dir.path().join("leaf.json"), content);
    let error = togi_machine::error_chain(&file::load_from(&leaf, &dir).expect_err("refused"));
    assert!(error.contains(leaf.to_str().expect("utf-8")), "{error}");
    assert!(error.contains(want), "{error} lacks {want:?}");
}
