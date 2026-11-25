use std::sync::Arc;

use togi_journal::MemFs;
use togi_tool_bench::cache::{Costs, longest_first};

#[test]
fn costs_are_kept_per_scope_with_the_latest_run_as_the_fallback() {
    let costs = Costs::with_fs("/cache/costs.json".into(), Arc::new(MemFs::new()));
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

#[test_case::test_case(0.0; "0 0")]
#[test_case::test_case(-1.0; "1 0")]
#[test_case::test_case(f64::NAN; "f64 NAN")]
#[test_case::test_case(f64::INFINITY; "f64 INFINITY")]
fn a_cost_that_is_not_a_positive_finite_time_is_not_kept(case: f64) {
    let wall = case;
    let costs = Costs::with_fs("/cache/costs.json".into(), Arc::new(MemFs::new()));

    costs.set("a", "x/dev-1", wall);

    assert_eq!(costs.get("a", "x/dev-1"), None);
}

#[test]
fn costs_survive_a_scope_that_has_none_and_keep_the_eight_latest_scopes() {
    let costs = Costs::with_fs("/cache/costs.json".into(), Arc::new(MemFs::new()));
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

#[test_case::test_case(("positional file", r#"[{"x/dev-1":5.0},[]]"#); "positional file")]
#[test_case::test_case((
            "positional scope",
            r#"{"latest":{"x/dev-1":5.0},"scopes":[["a",{"x/dev-1":5.0}]]}"#,
        ); "positional scope")]
#[test_case::test_case((
            "unknown key",
            r#"{"latest":{"x/dev-1":5.0},"scopes":[],"extra":1}"#,
        ); "unknown key")]
fn costs_round_trip_through_their_file_and_a_damaged_file_is_empty(case: (&str, &str)) {
    let (name, text) = case;
    let fs = Arc::new(MemFs::new());
    let path = std::path::PathBuf::from("/cache/sub/costs.json");
    let costs = Costs::with_fs(path.clone(), fs.clone());
    costs.set("a", "x/dev-1", 3.5);
    costs.save().unwrap();
    assert_eq!(
        Costs::with_fs(path.clone(), fs.clone()).get("a", "x/dev-1"),
        Some(3.5)
    );
    fs.put(&path, "not json");
    assert_eq!(
        Costs::with_fs(path.clone(), fs.clone()).get("a", "x/dev-1"),
        None
    );

    fs.put(&path, text);
    assert_eq!(Costs::with_fs(path, fs).get("a", "x/dev-1"), None, "{name}");
}

#[test]
fn sessions_start_longest_first_and_the_unknown_ones_before_them() {
    let known = [Some(1.0), None, Some(5.0), Some(1.0), None, Some(3.0)];
    assert_eq!(longest_first(known.len(), |i| known[i]), [1, 4, 2, 5, 0, 3]);
    assert!(longest_first(0, |_| None).is_empty());
}
