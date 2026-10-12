use togi_journal::{DeadEndCondition, ShutdownReason};
use togi_tool_audit::{Check, Violation};
use togi_tool_sim::{
    CoverageError, ExpectedShard, Outcome, Problem, RunId, RunResult, ShardManifest, check,
    encode_manifest, parse_manifest,
};

fn id(text: &str) -> RunId {
    serde_json::from_value(serde_json::Value::String(text.to_owned())).unwrap()
}

fn result(text: &str, outcome: Outcome) -> RunResult {
    RunResult {
        id: id(text),
        outcome,
    }
}

fn clean(text: &str) -> RunResult {
    result(text, Outcome::Clean)
}

fn manifest(shard: usize, results: Vec<RunResult>) -> ShardManifest {
    ShardManifest { shard, results }
}

fn owned(shard: usize, runs: &[&str]) -> ExpectedShard {
    ExpectedShard {
        shard,
        runs: runs.iter().copied().map(id).collect(),
    }
}

fn expected(shards: usize) -> Vec<ExpectedShard> {
    let mut expected = vec![
        owned(0, &["a/dev-1", "b/dev-1"]),
        owned(1, &["a/dev-2", "b/holdout-9"]),
    ];
    expected.extend((2..shards).map(|shard| owned(shard, &[])));
    expected
}

fn complete() -> Vec<ShardManifest> {
    vec![
        manifest(0, vec![clean("a/dev-1"), clean("b/dev-1")]),
        manifest(1, vec![clean("a/dev-2"), clean("b/holdout-9")]),
    ]
}

fn problems(manifests: &[ShardManifest], shards: usize) -> Vec<Problem> {
    check(&expected(shards), manifests).unwrap_err().problems
}

#[test]
fn a_complete_clean_suite_passes() {
    check(&expected(2), &complete()).unwrap();
}

#[test]
fn manifests_may_arrive_in_any_order() {
    let mut manifests = complete();
    manifests.reverse();
    check(&expected(2), &manifests).unwrap();
}

#[test]
fn a_shard_without_a_manifest_is_missing_with_its_runs() {
    let manifests = vec![complete().remove(0)];
    assert_eq!(
        problems(&manifests, 2),
        vec![
            Problem::MissingShard { shard: 1 },
            Problem::MissingRun { id: id("a/dev-2") },
            Problem::MissingRun {
                id: id("b/holdout-9")
            },
        ]
    );
}

#[test]
fn a_shard_with_no_runs_still_needs_its_manifest() {
    let manifests = complete();
    assert_eq!(
        problems(&manifests, 3),
        vec![Problem::MissingShard { shard: 2 }]
    );
    let mut with_empty = complete();
    with_empty.push(manifest(2, Vec::new()));
    check(&expected(3), &with_empty).unwrap();
}

#[test]
fn a_manifest_for_a_shard_outside_the_count_is_unexpected() {
    let mut manifests = complete();
    manifests.push(manifest(5, Vec::new()));
    assert_eq!(
        problems(&manifests, 2),
        vec![Problem::UnexpectedShard { shard: 5 }]
    );
}

#[test]
fn a_single_shard_is_checked_against_its_own_runs_alone() {
    let ids = [owned(1, &["a/dev-2", "b/holdout-9"])];
    let own = complete().remove(1);
    check(&ids, std::slice::from_ref(&own)).unwrap();
    let mut short = own.clone();
    short.results.pop();
    assert_eq!(
        check(&ids, &[short]).unwrap_err().problems,
        vec![Problem::MissingRun {
            id: id("b/holdout-9")
        }]
    );
    let mut extra = own.clone();
    extra.results.push(clean("a/dev-1"));
    assert_eq!(
        check(&ids, &[extra]).unwrap_err().problems,
        vec![Problem::UnexpectedRun {
            id: id("a/dev-1"),
            shard: 1
        }]
    );
    let mut renumbered = own;
    renumbered.shard = 0;
    assert_eq!(
        check(&ids, &[renumbered]).unwrap_err().problems[..2],
        [
            Problem::UnexpectedShard { shard: 0 },
            Problem::MissingShard { shard: 1 }
        ]
    );
}

#[test]
fn each_run_must_come_from_the_shard_that_owns_it() {
    let swapped = vec![
        manifest(0, vec![clean("a/dev-2"), clean("b/holdout-9")]),
        manifest(1, vec![clean("a/dev-1"), clean("b/dev-1")]),
    ];
    let wrong = |text: &str, shard, owner| Problem::WrongShard {
        id: id(text),
        shard,
        owner,
    };
    assert_eq!(
        problems(&swapped, 2),
        vec![
            wrong("a/dev-1", 1, 0),
            wrong("a/dev-2", 0, 1),
            wrong("b/dev-1", 1, 0),
            wrong("b/holdout-9", 0, 1),
        ]
    );
    let crowded = vec![
        manifest(
            0,
            vec![
                clean("a/dev-1"),
                clean("b/dev-1"),
                clean("a/dev-2"),
                clean("b/holdout-9"),
            ],
        ),
        manifest(1, Vec::new()),
    ];
    assert_eq!(
        problems(&crowded, 2),
        vec![wrong("a/dev-2", 0, 1), wrong("b/holdout-9", 0, 1)]
    );
}

#[test]
fn a_shard_reported_twice_is_a_duplicate_and_so_are_its_runs() {
    let mut manifests = complete();
    manifests.push(manifest(0, vec![clean("a/dev-1"), clean("b/dev-1")]));
    assert_eq!(
        problems(&manifests, 2),
        vec![
            Problem::DuplicateShard { shard: 0 },
            Problem::DuplicateRun {
                id: id("a/dev-1"),
                shards: vec![0, 0]
            },
            Problem::DuplicateRun {
                id: id("b/dev-1"),
                shards: vec![0, 0]
            },
        ]
    );
}

#[test]
fn a_run_reported_by_two_shards_is_a_duplicate() {
    let mut manifests = complete();
    manifests[1].results.push(clean("a/dev-1"));
    assert_eq!(
        problems(&manifests, 2),
        vec![
            Problem::DuplicateRun {
                id: id("a/dev-1"),
                shards: vec![0, 1]
            },
            Problem::WrongShard {
                id: id("a/dev-1"),
                shard: 1,
                owner: 0
            },
        ]
    );
}

#[test]
fn a_run_the_suite_does_not_have_is_unexpected() {
    let mut manifests = complete();
    manifests[0].results.push(clean("ghost/dev-1"));
    assert_eq!(
        problems(&manifests, 2),
        vec![Problem::UnexpectedRun {
            id: id("ghost/dev-1"),
            shard: 0
        }]
    );
}

#[test]
fn a_run_missing_from_its_manifest_is_named() {
    let mut manifests = complete();
    manifests[1].results.remove(0);
    assert_eq!(
        problems(&manifests, 2),
        vec![Problem::MissingRun { id: id("a/dev-2") }]
    );
}

#[test]
fn only_a_clean_outcome_passes() {
    let violation = Violation {
        seq: togi_journal::Seq::FIRST,
        check: Check::Avoidance,
        reason: "reason".to_owned(),
    };
    let unclean = vec![
        Outcome::Failed {
            detail: "d".to_owned(),
        },
        Outcome::Capped { boots: 1000 },
        Outcome::DeadEnd {
            condition: DeadEndCondition::FailureAtZero,
            detail: "d".to_owned(),
        },
        Outcome::Stopped {
            reason: ShutdownReason::Signal,
        },
        Outcome::VerifyFailed {
            detail: "d".to_owned(),
        },
        Outcome::ReplayFailed {
            detail: "d".to_owned(),
        },
        Outcome::AuditViolations {
            violations: vec![violation],
        },
    ];
    for outcome in unclean {
        let manifests = vec![
            manifest(
                0,
                vec![result("a/dev-1", outcome.clone()), clean("b/dev-1")],
            ),
            manifest(1, vec![clean("a/dev-2"), clean("b/holdout-9")]),
        ];
        assert_eq!(
            problems(&manifests, 2),
            vec![Problem::Unclean {
                id: id("a/dev-1"),
                shard: 0,
                outcome: outcome.clone()
            }],
            "{outcome}"
        );
    }
}

#[test]
fn every_offender_is_reported_at_once() {
    let manifests = vec![manifest(
        0,
        vec![
            result("a/dev-1", Outcome::Capped { boots: 2 }),
            clean("ghost/dev-1"),
        ],
    )];
    let found = problems(&manifests, 2);
    assert_eq!(found.len(), 6, "{found:?}");
    let error = CoverageError { problems: found };
    let text = error.to_string();
    assert!(text.starts_with("6 suite problems"), "{text}");
    for needle in [
        "shard 1 has no manifest",
        "a/dev-2",
        "b/dev-1",
        "b/holdout-9",
        "ghost/dev-1",
        "reached the cap of 2 boots",
    ] {
        assert!(text.contains(needle), "{needle} not in {text}");
    }
}

#[test]
fn a_manifest_round_trips_one_result_per_line() {
    let results = vec![
        clean("a/dev-1"),
        result("a/dev-2", Outcome::Capped { boots: 3 }),
        result(
            "b/dev-1",
            Outcome::DeadEnd {
                condition: DeadEndCondition::FailureAtZero,
                detail: "x".to_owned(),
            },
        ),
        result(
            "b/holdout-9",
            Outcome::Stopped {
                reason: ShutdownReason::Cycles,
            },
        ),
    ];
    let text = encode_manifest(&results).unwrap();
    assert_eq!(text.lines().count(), 4);
    assert_eq!(
        text.lines().next().unwrap(),
        r#"{"id":"a/dev-1","outcome":{"kind":"clean"}}"#
    );
    assert_eq!(parse_manifest(&text).unwrap(), results);
}

#[test]
fn a_damaged_manifest_is_refused_with_its_line() {
    let good = r#"{"id":"a/dev-1","outcome":{"kind":"clean"}}"#;
    let cases = [
        (format!("{good}\n{{\"id\":\"a/dev"), 2),
        (format!("{good}\n{{\"id\":\"a/dev-2\"}}"), 2),
        (
            r#"{"id":"a/dev-1","outcome":{"kind":"clean"},"extra":1}"#.to_owned(),
            1,
        ),
        (
            r#"{"id":"a/dev-1","outcome":{"kind":"unheard_of"}}"#.to_owned(),
            1,
        ),
    ];
    for (text, line) in cases {
        let error = parse_manifest(&text).unwrap_err();
        assert_eq!(error.line, line, "{text}");
    }
}

#[test]
fn blank_lines_in_a_manifest_are_skipped() {
    let text = "\n{\"id\":\"a/dev-1\",\"outcome\":{\"kind\":\"clean\"}}\n\n";
    assert_eq!(parse_manifest(text).unwrap(), vec![clean("a/dev-1")]);
    assert_eq!(parse_manifest("").unwrap(), Vec::new());
}
