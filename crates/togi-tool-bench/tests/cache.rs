#![expect(clippy::unwrap_used, reason = "helpers panic on a broken fixture")]

mod common;

use std::fs::{self, File};
use std::path::Path;
use std::time::{Duration, SystemTime};

use common::record;
use togi_tool_bench::cache::{
    Costs, RunCache, current_exe_digest, inputs, longest_first, tree_key,
};
use togi_tool_bench::{RunSpec, Selection, Suite};

fn plan(dir: &Path, suite: &str) -> (String, Vec<RunSpec>) {
    let suite = Suite::parse(suite.as_bytes(), dir.to_path_buf()).unwrap();
    let runs = suite.plan(Selection::All).unwrap().runs;
    (suite.digest().to_owned(), runs)
}

const SUITE: &str = r#"{"scenarios":[{"name":"x","machine":"m.json","dev":[1,2],"holdout":[3]},{"name":"y","dev":[1]}]}"#;

fn machine_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("m.json"), r#"{"cores": 2}"#).unwrap();
    dir
}

#[test]
fn the_tree_key_binds_both_binaries_and_the_boot_cap() {
    let key = tree_key("sim", "bench", 1000);
    assert_eq!(key, tree_key("sim", "bench", 1000));
    for other in [
        tree_key("sim2", "bench", 1000),
        tree_key("sim", "bench2", 1000),
        tree_key("sim", "bench", 999),
        tree_key("bench", "sim", 1000),
    ] {
        assert_ne!(key, other);
    }
    assert_eq!(key.len(), 64);
}

#[test]
fn the_bench_hashes_its_own_executable() {
    let digest = current_exe_digest().unwrap();
    assert_eq!(digest.len(), 64);
    assert_eq!(digest, current_exe_digest().unwrap());
}

#[test]
fn a_runs_inputs_bind_the_suite_the_run_and_every_file_of_its_machine() {
    let dir = machine_dir();
    let (suite, runs) = plan(dir.path(), SUITE);
    let base = inputs(&suite, &runs[0]);
    assert_eq!(base, inputs(&suite, &runs[0]));
    assert_ne!(base, inputs(&suite, &runs[1]), "another seed");
    assert_ne!(base, inputs(&suite, &runs[2]), "another split");
    assert_ne!(
        base,
        inputs(&suite, &runs[3]),
        "another scenario and machine"
    );
    assert_ne!(base, inputs("another suite", &runs[0]));

    fs::write(dir.path().join("m.json"), r#"{"cores": 4}"#).unwrap();
    let (_, changed) = plan(dir.path(), SUITE);
    assert_ne!(
        base,
        inputs(&suite, &changed[0]),
        "the machine file changed"
    );
    assert_eq!(
        inputs(&suite, &runs[3]),
        inputs(&suite, &changed[3]),
        "the default machine did not"
    );
}

fn committed_tree(machines: &[(&str, Option<(&str, &str)>)]) -> tempfile::TempDir {
    let bench = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/bench");
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("machines")).unwrap();
    for (name, repoint) in machines {
        let mut text = fs::read_to_string(bench.join("machines").join(name)).unwrap();
        if let Some((from, to)) = repoint {
            let extends = format!("\"extends\": \"{from}\"");
            assert_eq!(text.matches(&extends).count(), 1, "{name}");
            text = text.replace(&extends, &format!("\"extends\": \"{to}\""));
        }
        fs::write(dir.path().join("machines").join(name), text).unwrap();
    }
    fs::create_dir_all(dir.path().join("facts")).unwrap();
    fs::copy(
        bench.join("facts/target.jsonl.gz"),
        dir.path().join("facts/target.jsonl.gz"),
    )
    .unwrap();
    dir
}

fn edit_notes(path: &Path) {
    let text = fs::read_to_string(path).unwrap();
    assert!(text.contains("\"notes\": ["));
    fs::write(
        path,
        text.replacen("\"notes\": [", "\"notes\": [\"edited\", ", 1),
    )
    .unwrap();
}

#[test]
fn every_file_an_overlay_extends_is_an_input() {
    let dir = committed_tree(&[
        ("target-shared-voltage.json", None),
        ("target-r7-vf-boost.json", None),
    ]);
    let suite =
        r#"{"scenarios":[{"name":"x","machine":"machines/target-r7-vf-boost.json","dev":[1]}]}"#;
    let (digest, runs) = plan(dir.path(), suite);
    let base = inputs(&digest, &runs[0]);
    edit_notes(&dir.path().join("machines/target-shared-voltage.json"));
    let (_, changed) = plan(dir.path(), suite);
    assert_ne!(base, inputs(&digest, &changed[0]));
}

#[test]
fn an_equal_overlay_chain_in_another_tree_hits_and_only_its_grandparent_changing_misses() {
    let chain = [
        ("target-shared-voltage.json", None),
        ("target-r7-vf-boost.json", None),
        (
            "target-r7-request-gap.json",
            Some(("target-shared-voltage.json", "target-r7-vf-boost.json")),
        ),
    ];
    let suite = r#"{"scenarios":[{"name":"chain","machine":"machines/target-r7-request-gap.json","dev":[1,2],"holdout":[101]}]}"#;
    let first = committed_tree(&chain);
    let second = committed_tree(&chain);
    let (digest, runs) = plan(first.path(), suite);
    let (other_digest, other_runs) = plan(second.path(), suite);
    assert_eq!(digest, other_digest);
    let machine = runs[0].machine.as_ref().unwrap();
    assert_eq!(machine.inputs.len(), 4, "leaf, parent, root and the facts");

    let cache = RunCache::open(&first.path().join("cache"), "key").unwrap();
    cache
        .put(&runs[0], &inputs(&digest, &runs[0]), &record("chain", 1))
        .unwrap();
    for (run, other) in runs.iter().zip(&other_runs) {
        assert_eq!(inputs(&digest, run), inputs(&other_digest, other));
    }
    assert_eq!(
        cache.get(&other_runs[0], &inputs(&other_digest, &other_runs[0])),
        Some(record("chain", 1)),
        "an equal chain in another tree shares the record"
    );

    edit_notes(&second.path().join("machines/target-shared-voltage.json"));
    let (_, edited) = plan(second.path(), suite);
    assert_ne!(inputs(&digest, &runs[0]), inputs(&digest, &edited[0]));
    assert!(
        cache
            .get(&edited[0], &inputs(&digest, &edited[0]))
            .is_none(),
        "editing only the grandparent misses"
    );
}

#[test]
fn equal_files_in_another_directory_give_the_same_inputs() {
    let first = machine_dir();
    let second = machine_dir();
    let (digest, runs) = plan(first.path(), SUITE);
    let (other_digest, other_runs) = plan(second.path(), SUITE);
    assert_eq!(digest, other_digest);
    for (run, other) in runs.iter().zip(&other_runs) {
        assert_eq!(inputs(&digest, run), inputs(&other_digest, other));
    }
}

#[test]
fn flipping_replay_changes_the_inputs() {
    let dir = committed_tree(&[("target-shared-voltage.json", None)]);
    let machine = r#""machine":"machines/target-shared-voltage.json","dev":[1]"#;
    let (digest, plain) = plan(
        dir.path(),
        &format!(r#"{{"scenarios":[{{"name":"x",{machine}}}]}}"#),
    );
    let (_, replayed) = plan(
        dir.path(),
        &format!(r#"{{"scenarios":[{{"name":"x","replay":true,{machine}}}]}}"#),
    );
    assert!(plain[0].replay.is_none() && replayed[0].replay.is_some());
    assert_ne!(
        inputs(&digest, &plain[0]),
        inputs(&digest, &replayed[0]),
        "same suite digest, machine and facts"
    );
}

#[test]
fn a_declared_facts_extract_is_an_input() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("facts.gz"), "one").unwrap();
    fs::write(
        dir.path().join("m.json"),
        r#"{"cores": 2, "facts": "facts.gz"}"#,
    )
    .unwrap();
    let suite = r#"{"scenarios":[{"name":"x","machine":"m.json","dev":[1]}]}"#;
    let (digest, runs) = plan(dir.path(), suite);
    let base = inputs(&digest, &runs[0]);
    fs::write(dir.path().join("facts.gz"), "two").unwrap();
    let (_, changed) = plan(dir.path(), suite);
    assert_ne!(base, inputs(&digest, &changed[0]));
}

#[test]
fn a_record_comes_back_only_for_the_inputs_it_was_made_from() {
    let dir = machine_dir();
    let (suite, runs) = plan(dir.path(), SUITE);
    let cache = RunCache::open(&dir.path().join("cache"), "key").unwrap();
    let digest = inputs(&suite, &runs[0]);
    let mut stored = record("x", 1);
    stored.sim_hours = 12.5;
    assert!(cache.get(&runs[0], &digest).is_none());
    cache.put(&runs[0], &digest, &stored).unwrap();
    assert_eq!(cache.get(&runs[0], &digest), Some(stored));
    assert!(cache.get(&runs[0], "other inputs").is_none());
    assert!(
        cache.get(&runs[1], &digest).is_none(),
        "another run has its own entry"
    );
}

#[test]
fn an_entry_for_another_run_or_a_damaged_one_is_a_miss() {
    let dir = machine_dir();
    let (suite, runs) = plan(dir.path(), SUITE);
    let root = dir.path().join("cache");
    let cache = RunCache::open(&root, "key").unwrap();
    let digest = inputs(&suite, &runs[0]);
    cache.put(&runs[0], &digest, &record("x", 2)).unwrap();
    assert!(
        cache.get(&runs[0], &digest).is_none(),
        "the stored record is of seed 2"
    );
    cache.put(&runs[0], &digest, &record("x", 1)).unwrap();
    let file = root.join("runs/key/x/dev-1.json");
    fs::write(&file, "{").unwrap();
    assert!(cache.get(&runs[0], &digest).is_none());
    cache.put(&runs[0], &digest, &record("x", 1)).unwrap();
    let stored: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
    let positional = serde_json::json!([stored["inputs"], stored["record"]]);
    fs::write(&file, positional.to_string()).unwrap();
    assert!(cache.get(&runs[0], &digest).is_none(), "a positional entry");
    let mut unknown = stored;
    unknown["extra"] = serde_json::json!(1);
    fs::write(&file, unknown.to_string()).unwrap();
    assert!(cache.get(&runs[0], &digest).is_none(), "an unknown key");
}

#[test]
fn opening_a_cache_keeps_the_seven_most_recently_used_others() {
    let dir = tempfile::tempdir().unwrap();
    let runs = dir.path().join("runs");
    for index in 0..10_u64 {
        let tree = runs.join(format!("old{index}"));
        fs::create_dir_all(&tree).unwrap();
        File::open(&tree)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000 + index))
            .unwrap();
    }
    RunCache::open(dir.path(), "new").unwrap();
    let mut left: Vec<String> = fs::read_dir(&runs)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    left.sort();
    assert_eq!(
        left,
        [
            "new", "old3", "old4", "old5", "old6", "old7", "old8", "old9"
        ]
    );
}

#[test]
fn reopening_a_cache_marks_it_recently_used() {
    let dir = tempfile::tempdir().unwrap();
    let runs = dir.path().join("runs");
    RunCache::open(dir.path(), "kept").unwrap();
    File::open(runs.join("kept"))
        .unwrap()
        .set_modified(SystemTime::UNIX_EPOCH)
        .unwrap();
    for index in 0..7_u64 {
        let tree = runs.join(format!("old{index}"));
        fs::create_dir_all(&tree).unwrap();
        File::open(&tree)
            .unwrap()
            .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(10 + index))
            .unwrap();
    }
    RunCache::open(dir.path(), "kept").unwrap();
    assert!(runs.join("kept").is_dir());
    assert_eq!(fs::read_dir(&runs).unwrap().count(), 8);
}

#[test]
fn costs_are_kept_per_scope_with_the_latest_run_as_the_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let costs = Costs::load(dir.path().join("costs.json"));
    costs.set("a", "x/dev-1", 2.0);
    costs.set("b", "x/dev-1", 5.0);
    costs.set("b", "y/dev-1", 7.0);
    assert_eq!(costs.get("a", "x/dev-1"), Some(2.0));
    assert_eq!(costs.get("b", "x/dev-1"), Some(5.0));
    assert_eq!(
        costs.get("a", "y/dev-1"),
        Some(7.0),
        "never run by a: the latest by any"
    );
    assert_eq!(costs.get("a", "z/dev-1"), None);
}

#[test]
fn a_cost_that_is_not_a_positive_finite_time_is_not_kept() {
    let dir = tempfile::tempdir().unwrap();
    let costs = Costs::load(dir.path().join("costs.json"));
    for wall in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        costs.set("a", "x/dev-1", wall);
    }
    assert_eq!(costs.get("a", "x/dev-1"), None);
}

#[test]
fn costs_survive_a_scope_that_has_none_and_keep_the_eight_latest_scopes() {
    let dir = tempfile::tempdir().unwrap();
    let costs = Costs::load(dir.path().join("costs.json"));
    for scope in 0..10 {
        costs.set(&format!("s{scope}"), "x/dev-1", 1.0 + f64::from(scope));
    }
    assert_eq!(costs.get("s9", "x/dev-1"), Some(10.0));
    assert_eq!(costs.get("s2", "x/dev-1"), Some(3.0), "the eighth latest");
    assert_eq!(
        costs.get("s1", "x/dev-1"),
        Some(10.0),
        "an evicted scope falls back to the latest"
    );
}

#[test]
fn costs_round_trip_through_their_file_and_a_damaged_file_is_empty() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sub/costs.json");
    let costs = Costs::load(path.clone());
    costs.set("a", "x/dev-1", 3.5);
    costs.save().unwrap();
    assert_eq!(Costs::load(path.clone()).get("a", "x/dev-1"), Some(3.5));
    fs::write(&path, "not json").unwrap();
    assert_eq!(Costs::load(path.clone()).get("a", "x/dev-1"), None);
    for (name, text) in [
        ("positional file", r#"[{"x/dev-1":5.0},[]]"#),
        (
            "positional scope",
            r#"{"latest":{"x/dev-1":5.0},"scopes":[["a",{"x/dev-1":5.0}]]}"#,
        ),
        (
            "unknown key",
            r#"{"latest":{"x/dev-1":5.0},"scopes":[],"extra":1}"#,
        ),
    ] {
        fs::write(&path, text).unwrap();
        assert_eq!(
            Costs::load(path.clone()).get("a", "x/dev-1"),
            None,
            "{name}"
        );
    }
}

#[test]
fn sessions_start_longest_first_and_the_unknown_ones_before_them() {
    let known = [Some(1.0), None, Some(5.0), Some(1.0), None, Some(3.0)];
    assert_eq!(longest_first(known.len(), |i| known[i]), [1, 4, 2, 5, 0, 3]);
    assert!(longest_first(0, |_| None).is_empty());
}
