#![expect(clippy::unwrap_used, reason = "helpers panic on a broken fixture")]

mod common;

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;

use common::record;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use togi_tool_bench::gate::{self, Criterion, Measure, Resolved, Verdict, quantile_change};
use togi_tool_bench::{Record, Selection, Status, Suite, read_records};

type Shift = fn(usize) -> f64;
type Mutate = fn(&mut Vec<Record>, &mut Vec<Record>);

const SCENARIO: &str = "adv";
const SEEDS: usize = 24;

fn gate_object() -> Value {
    json!({
        "id": "test",
        "baseline": {"ruleset": 10, "commits": ["abc"]},
        "split": "dev",
        "gated": [SCENARIO],
        "pooled": [SCENARIO],
        "conclude": ["hand"],
        "quantiles": [0.5, 0.9],
        "confidence": 0.95,
        "resamples": 10000,
        "bootstrap_seed": 1,
        "max_time_ratio": 2.0,
    })
}

fn suite_of(gate: &Value) -> Suite {
    let dev: Vec<usize> = (1..=SEEDS).collect();
    let suite = json!({
        "gate": gate,
        "scenarios": [
            {"name": SCENARIO, "dev": dev, "holdout": [101]},
            {"name": "hand", "dev": [1, 2]},
        ],
    });
    Suite::parse(suite.to_string().as_bytes(), ".".into()).unwrap()
}

fn resolved(gate: &Value) -> Resolved {
    gate::load(&suite_of(gate)).unwrap().unwrap()
}

fn test_gate() -> Resolved {
    resolved(&gate_object())
}

fn gate_error(edit: impl FnOnce(&mut Value)) -> String {
    let mut gate = gate_object();
    edit(&mut gate);
    gate::load(&suite_of(&gate)).unwrap_err().to_string()
}

fn run(scenario: &str, seed: u64, hours: f64, hazard: Option<f64>, baseline: bool) -> Record {
    Record {
        sim_hours: hours,
        worst_r7_hazard_per_h: hazard,
        ruleset: if baseline { 10 } else { 11 },
        commit: if baseline { "abc" } else { "cand" }.to_owned(),
        ..record(scenario, seed)
    }
}

/// A baseline of 1..=24 and a candidate adding `shift(i)` to the i-th seed's value.
fn gate_runs(
    shift: impl Fn(usize) -> f64,
    hours: impl Fn(usize) -> (f64, f64),
) -> (Vec<Record>, Vec<Record>) {
    let (mut candidate, mut baseline) = (Vec::new(), Vec::new());
    for i in 0..SEEDS {
        let value = num(i + 1);
        let (ch, bh) = hours(i);
        let seed = u64::try_from(i).unwrap() + 1;
        candidate.push(run(SCENARIO, seed, ch, Some(value + shift(i)), false));
        baseline.push(run(SCENARIO, seed, bh, Some(value), true));
    }
    for seed in 1..=2 {
        candidate.push(run("hand", seed, 1.0, None, false));
        baseline.push(run("hand", seed, 1.0, None, true));
    }
    (candidate, baseline)
}

fn num(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap())
}

fn same_hours(_: usize) -> (f64, f64) {
    (10.0, 10.0)
}

fn none(_: usize) -> f64 {
    0.0
}

fn scored(verdict: Verdict) -> Vec<Criterion> {
    match verdict {
        Verdict::Scored(criteria) => criteria,
        Verdict::Refused(reason) => panic!("refused: {reason}"),
    }
}

fn find(criteria: &[Criterion], kind: &str, scenario: &str) -> Criterion {
    criteria
        .iter()
        .find(|c| {
            c.scenario == scenario
                && match &c.measure {
                    Measure::Conclusion { .. } => kind == "conclusion",
                    Measure::Quantile { label, .. } => kind == label,
                    Measure::Time { .. } => kind == "time",
                    Measure::PooledMedian { .. } => kind == "pooled_median",
                }
        })
        .unwrap_or_else(|| panic!("no {kind} criterion for {scenario}"))
        .clone()
}

fn quantile(c: &Criterion) -> (f64, f64, f64, f64) {
    match c.measure {
        Measure::Quantile {
            baseline,
            candidate,
            lo,
            hi,
            ..
        } => (baseline, candidate, lo, hi),
        _ => panic!("not a quantile: {c:?}"),
    }
}

fn text(gate: &Resolved, verdict: &Verdict) -> String {
    let mut out = Vec::new();
    gate::report(&mut out, gate.gate(), verdict).unwrap();
    String::from_utf8(out).unwrap()
}

#[test]
fn a_quantile_fails_only_when_its_interval_lies_above_zero() {
    let gate = test_gate();
    let cases: [(&str, Shift, bool, bool); 4] = [
        ("unchanged", none, true, false),
        (
            "noise crossing zero",
            |i| if i % 2 == 0 { 1.0 } else { -1.0 },
            true,
            false,
        ),
        ("every seed worse", |_| 1.0, false, true),
        ("every seed better", |_| -1.0, true, false),
    ];
    for (name, shift, pass, above) in cases {
        let (candidate, baseline) = gate_runs(shift, same_hours);
        let criteria = scored(gate.judge(&candidate, &baseline));
        for kind in ["median", "p90"] {
            let c = find(&criteria, kind, SCENARIO);
            let (_, _, lo, hi) = quantile(&c);
            assert_eq!(c.pass, pass, "{name} {kind}: interval [{lo}, {hi}]");
            assert_eq!(lo > 0.0, above, "{name} {kind}: interval [{lo}, {hi}]");
        }
    }
}

#[test]
fn the_change_is_the_difference_of_quantiles_not_their_pairwise_quantile() {
    let gate = test_gate();
    let (candidate, baseline) = gate_runs(|i| if i == SEEDS - 1 { -23.0 } else { 1.0 }, same_hours);
    let criteria = scored(gate.judge(&candidate, &baseline));
    let (base, cand, _, _) = quantile(&find(&criteria, "median", SCENARIO));
    assert!((base - 12.5).abs() < 1e-12 && (cand - 12.5).abs() < 1e-12);
}

#[test]
fn the_pooled_median_must_fall_strictly() {
    let gate = test_gate();
    for (name, shift, pass) in [
        ("equal", 0.0, false),
        ("higher", 1.0, false),
        ("lower", -1.0, true),
    ] {
        let (candidate, baseline) = gate_runs(|_| shift, same_hours);
        let criteria = scored(gate.judge(&candidate, &baseline));
        assert_eq!(
            find(&criteria, "pooled_median", SCENARIO).pass,
            pass,
            "{name}"
        );
    }
}

#[test]
fn a_lost_conclusion_fails_and_a_baseline_without_one_cannot_lose_it() {
    let gate = test_gate();
    let cases = [
        (
            "lost seed",
            SCENARIO,
            3,
            Status::DeadEnd,
            Status::Concluded,
            false,
        ),
        (
            "lost hand-set seed",
            "hand",
            25,
            Status::Censored,
            Status::Concluded,
            false,
        ),
        (
            "baseline never concluded",
            SCENARIO,
            3,
            Status::DeadEnd,
            Status::DeadEnd,
            true,
        ),
        (
            "newly concluded",
            SCENARIO,
            3,
            Status::Concluded,
            Status::DeadEnd,
            true,
        ),
    ];
    for (name, scenario, index, candidate_status, baseline_status, pass) in cases {
        let (mut candidate, mut baseline) = gate_runs(none, same_hours);
        candidate[index].status = candidate_status;
        baseline[index].status = baseline_status;
        let criteria = scored(gate.judge(&candidate, &baseline));
        let c = find(&criteria, "conclusion", scenario);
        assert_eq!(c.pass, pass, "{name}: {c:?}");
        if !pass {
            let Measure::Conclusion { lost, .. } = c.measure else {
                panic!("not a conclusion");
            };
            assert_eq!(lost, [candidate[index].seed], "{name}");
        }
    }
}

#[test]
fn the_time_ratio_may_equal_but_not_exceed_its_bound() {
    let gate = test_gate();
    for (name, hours, pass) in [
        ("exactly twice", 20.0, true),
        ("above twice", 20.0001, false),
        ("faster", 5.0, true),
    ] {
        let (candidate, baseline) = gate_runs(none, |_| (hours, 10.0));
        let criteria = scored(gate.judge(&candidate, &baseline));
        assert_eq!(find(&criteria, "time", SCENARIO).pass, pass, "{name}");
    }
}

#[test]
fn the_time_ratio_is_the_geometric_mean_over_pairs() {
    let gate = test_gate();
    let (candidate, baseline) = gate_runs(none, |i| (if i % 2 == 0 { 40.0 } else { 10.0 }, 10.0));
    let criteria = scored(gate.judge(&candidate, &baseline));
    let c = find(&criteria, "time", SCENARIO);
    assert!(c.pass, "ratios 4 and 1 average to 2: {c:?}");
    let Measure::Time { ratio, timed } = c.measure else {
        panic!("not a time criterion");
    };
    assert_eq!(timed, SEEDS);
    assert!((ratio.unwrap() - 2.0).abs() < 1e-9);
}

#[test]
fn a_pair_that_did_not_conclude_on_both_sides_has_no_time_ratio() {
    let gate = test_gate();
    let (mut candidate, baseline) = gate_runs(none, same_hours);
    for run in candidate.iter_mut().filter(|run| run.scenario == SCENARIO) {
        run.status = Status::Censored;
    }
    let criteria = scored(gate.judge(&candidate, &baseline));
    let c = find(&criteria, "time", SCENARIO);
    assert!(!c.pass);
    assert_eq!(
        c.measure,
        Measure::Time {
            ratio: None,
            timed: 0
        }
    );
    assert!(
        text(&gate, &Verdict::Scored(vec![c]))
            .contains("ratio=none timed=0 threshold=ratio<=2 FAIL")
    );
}

#[test]
fn the_gate_refuses_what_it_cannot_judge_and_scores_nothing() {
    let gate = test_gate();
    let cases: [(&str, Mutate, &str); 5] = [
        (
            "candidate seed missing",
            |c, _| drop(c.remove(0)),
            "incomplete pairing: adv seed 1 has no candidate run",
        ),
        (
            "baseline seed missing",
            |_, b| drop(b.remove(0)),
            "incomplete pairing: adv seed 1 has no baseline run",
        ),
        (
            "hand-set seed missing",
            |c, _| drop(c.pop()),
            "incomplete pairing: hand seed 2",
        ),
        (
            "other baseline",
            |_, b| b[0].commit = "def".into(),
            "not ruleset 10 at abc",
        ),
        (
            "metric missing",
            |c, _| c[0].worst_r7_hazard_per_h = None,
            "adv seed 1 lacks worst_r7_hazard_per_h",
        ),
    ];
    for (name, mutate, want) in cases {
        let (mut candidate, mut baseline) = gate_runs(none, same_hours);
        mutate(&mut candidate, &mut baseline);
        let verdict = gate.judge(&candidate, &baseline);
        let Verdict::Refused(reason) = &verdict else {
            panic!("{name}: judged {verdict:?}");
        };
        assert!(reason.contains(want), "{name}: {reason}");
        assert!(!verdict.pass(), "{name}");
    }
}

#[test]
fn a_missing_run_on_both_sides_is_named_as_both() {
    let gate = test_gate();
    let (mut candidate, mut baseline) = gate_runs(none, same_hours);
    candidate.remove(0);
    baseline.remove(0);
    let Verdict::Refused(reason) = gate.judge(&candidate, &baseline) else {
        panic!("judged");
    };
    assert!(
        reason.contains("adv seed 1 has no candidate or baseline run"),
        "{reason}"
    );
}

#[test]
fn the_verdict_passes_only_when_every_criterion_does() {
    let gate = test_gate();
    for (name, shift, drop, pass, last) in [
        ("pass", -1.0, 0, true, "gate test: PASS 6 criteria"),
        ("fail", 1.0, 0, false, "gate test: FAIL 3 of 6 criteria"),
        (
            "refused",
            -1.0,
            1,
            false,
            "gate test: FAIL refused: incomplete pairing",
        ),
    ] {
        let (candidate, baseline) = gate_runs(|_| shift, same_hours);
        let verdict = gate.judge(&candidate[drop..], &baseline);
        assert_eq!(verdict.pass(), pass, "{name}");
        let report = text(&gate, &verdict);
        assert!(report.contains(last), "{name}: {report}");
    }
}

#[test]
fn a_baseline_may_mix_the_commits_the_gate_names_and_no_others() {
    let mut object = gate_object();
    object["baseline"]["commits"] = json!(["abc", "def"]);
    let gate = resolved(&object);
    let (candidate, mut baseline) = gate_runs(none, same_hours);
    baseline[0].commit = "def".into();
    assert!(!matches!(
        gate.judge(&candidate, &baseline),
        Verdict::Refused(_)
    ));
    baseline[1].ruleset = 9;
    let Verdict::Refused(reason) = gate.judge(&candidate, &baseline) else {
        panic!("judged");
    };
    assert!(
        reason.contains("is ruleset 9 at abc, not ruleset 10 at abc or def"),
        "{reason}"
    );
}

fn pairs() -> Vec<(f64, f64)> {
    (0..SEEDS)
        .map(|i| (num(i + 1) + num(i % 5) - 1.0, num(i + 1)))
        .collect()
}

#[test]
fn the_bootstrap_interval_is_reproducible_and_pinned() {
    let first = quantile_change(&pairs(), 0.5, 0.95, 10_000, 1);
    for _ in 0..3 {
        assert_eq!(first, quantile_change(&pairs(), 0.5, 0.95, 10_000, 1));
    }
    assert!(
        (first.lo - -0.5).abs() < 1e-9 && (first.hi - 2.5).abs() < 1e-9,
        "{first:?}"
    );
    let spread: Vec<(f64, f64)> = (0..SEEDS)
        .map(|i| (num((i * 7919) % 31) / 3.0, num(i % 11)))
        .collect();
    let draws: std::collections::BTreeSet<_> = (1..=4)
        .map(|seed| quantile_change(&spread, 0.9, 0.95, 500, seed).lo.to_bits())
        .collect();
    assert!(draws.len() > 1, "the seed must change the draws");
}

#[test]
fn the_bootstrap_interval_does_not_depend_on_the_number_of_threads() {
    let alone = quantile_change(&pairs(), 0.9, 0.95, 2_000, 7);
    let many: Vec<_> = thread::scope(|scope| {
        let handles: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| quantile_change(&pairs(), 0.9, 0.95, 2_000, 7)))
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert!(many.iter().all(|change| *change == alone));
}

#[test]
fn a_single_pair_has_a_point_interval_and_no_pairs_a_zero_one() {
    let one = quantile_change(&[(3.0, 1.0)], 0.5, 0.95, 100, 1);
    assert_eq!(
        (one.baseline, one.candidate, one.lo, one.hi),
        (1.0, 3.0, 2.0, 2.0)
    );
    let none = quantile_change(&[], 0.5, 0.95, 100, 1);
    assert_eq!((none.lo, none.hi), (0.0, 0.0));
}

#[test]
fn the_reports_of_a_passing_failing_and_refused_gate() {
    let gate = test_gate();
    let mut out = String::new();
    for (name, shift, drop) in [
        ("pass", (|_| -1.0) as fn(usize) -> f64, 0),
        ("fail", |i| num(i % 2) + 1.0, 0),
        ("refused", none, 1),
    ] {
        let (mut candidate, baseline) = gate_runs(shift, same_hours);
        candidate[5].status = if name == "fail" {
            Status::DeadEnd
        } else {
            Status::Concluded
        };
        out += &text(&gate, &gate.judge(&candidate[drop..], &baseline));
        out.push('\n');
    }
    insta::assert_snapshot!(out);
}

#[test]
fn the_report_prints_the_gates_note_after_its_header() {
    let mut object = gate_object();
    object["note"] = json!("why this gate");
    let gate = resolved(&object);
    let (candidate, baseline) = gate_runs(|_| -1.0, same_hours);
    let report = text(&gate, &gate.judge(&candidate, &baseline));
    let mut lines = report.lines();
    assert!(
        lines
            .next()
            .unwrap()
            .starts_with("gate test: judged against ruleset 10 at abc on every dev seed")
    );
    assert_eq!(lines.next(), Some("gate test: why this gate"));
}

#[test]
fn quantile_labels_keep_their_decimals() {
    let labels: Vec<_> = [0.5, 0.9, 0.95, 0.995, 0.999]
        .map(gate::quantile_label)
        .to_vec();
    assert_eq!(labels, ["median", "p90", "p95", "p99.5", "p99.9"]);
}

#[test]
fn a_valid_gate_resolves_the_seeds_of_its_split() {
    let gate = test_gate();
    assert_eq!(gate.seeds(SCENARIO).unwrap().len(), SEEDS);
    assert_eq!(gate.seeds("hand").unwrap(), [1, 2]);
    assert!(gate.seeds("other").is_none());
    let mut all = gate_object();
    all["split"] = json!("all");
    assert_eq!(resolved(&all).seeds(SCENARIO).unwrap().len(), SEEDS + 1);
    let mut holdout = gate_object();
    holdout["split"] = json!("holdout");
    holdout["conclude"] = json!([]);
    assert_eq!(resolved(&holdout).seeds(SCENARIO).unwrap(), [101]);
}

#[test]
fn a_suite_without_a_gate_registers_none() {
    let suite = Suite::parse(br#"{"scenarios":[{"name":"a","dev":[1]}]}"#, ".".into()).unwrap();
    assert!(gate::load(&suite).unwrap().is_none());
}

#[test]
fn an_invalid_gate_is_refused_with_its_reason() {
    type Edit = fn(&mut Value);
    let cases: Vec<(&str, Edit, &str)> = vec![
        (
            "unknown gate key",
            |g| g["extra"] = json!(1),
            "unknown field `extra`",
        ),
        (
            "unknown baseline key",
            |g| g["baseline"]["extra"] = json!(1),
            "unknown field `extra`",
        ),
        (
            "unknown scenario",
            |g| {
                g["gated"] = json!(["nope"]);
                g["pooled"] = json!(["nope"]);
            },
            "unknown scenario \"nope\"",
        ),
        (
            "unknown concluded scenario",
            |g| g["conclude"] = json!(["nope"]),
            "unknown scenario \"nope\"",
        ),
        (
            "pooled but not gated",
            |g| g["pooled"] = json!(["hand"]),
            "pooled scenario \"hand\" is not gated",
        ),
        (
            "quantile of one",
            |g| g["quantiles"] = json!([1]),
            "quantile 1 outside",
        ),
        (
            "quantile of zero",
            |g| g["quantiles"] = json!([0]),
            "quantile 0 outside",
        ),
        (
            "no quantiles",
            |g| g["quantiles"] = json!([]),
            "needs gated scenarios and quantiles",
        ),
        (
            "no gated",
            |g| g["gated"] = json!([]),
            "needs gated scenarios and quantiles",
        ),
        (
            "zero confidence",
            |g| g["confidence"] = json!(0.0),
            "needs confidence in (0, 1)",
        ),
        (
            "unit confidence",
            |g| g["confidence"] = json!(1.0),
            "needs confidence in (0, 1)",
        ),
        (
            "zero resamples",
            |g| g["resamples"] = json!(0),
            "resamples from 1 to",
        ),
        (
            "too many resamples",
            |g| g["resamples"] = json!(10_000_001),
            "resamples from 1 to",
        ),
        (
            "negative resamples",
            |g| g["resamples"] = json!(-1),
            "invalid value",
        ),
        (
            "zero time ratio",
            |g| g["max_time_ratio"] = json!(0.0),
            "positive max_time_ratio",
        ),
        (
            "bad split",
            |g| g["split"] = json!("x"),
            "unknown variant `x`",
        ),
        (
            "no baseline commits",
            |g| g["baseline"]["commits"] = json!([]),
            "needs an id and a baseline",
        ),
        (
            "empty commit",
            |g| g["baseline"]["commits"] = json!([""]),
            "needs an id and a baseline",
        ),
        (
            "no baseline ruleset",
            |g| g["baseline"]["ruleset"] = json!(0),
            "needs an id and a baseline",
        ),
        (
            "no id",
            |g| g["id"] = json!(""),
            "needs an id and a baseline",
        ),
        (
            "missing field",
            |g| drop(g.as_object_mut().unwrap().remove("max_time_ratio")),
            "missing field `max_time_ratio`",
        ),
        (
            "repeated gated",
            |g| g["gated"] = json!([SCENARIO, SCENARIO]),
            "gated scenario \"adv\" is listed twice",
        ),
        (
            "repeated quantile",
            |g| g["quantiles"] = json!([0.9, 0.9]),
            "quantile \"p90\" is listed twice",
        ),
        (
            "gated and concluded",
            |g| g["conclude"] = json!([SCENARIO]),
            "both gated and concluded",
        ),
    ];
    for (name, edit, want) in cases {
        let error = gate_error(edit);
        assert!(error.contains(want), "{name}: {error}");
    }
}

#[test]
fn a_gate_that_leaves_a_scenario_without_seeds_is_refused() {
    let suite = Suite::parse(
        json!({"gate": {
            "id": "g", "split": "holdout", "gated": ["a"], "quantiles": [0.5], "confidence": 0.95,
            "resamples": 10, "bootstrap_seed": 1, "max_time_ratio": 2,
            "baseline": {"ruleset": 1, "commits": ["x"]},
        }, "scenarios": [{"name": "a", "dev": [1]}]})
        .to_string()
        .as_bytes(),
        ".".into(),
    )
    .unwrap();
    let error = gate::load(&suite).unwrap_err().to_string();
    assert!(error.contains("scenario a has no holdout seeds"), "{error}");
}

fn bench() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/bench")
}

fn sha256(path: &Path) -> String {
    Sha256::digest(fs::read(path).unwrap())
        .iter()
        .fold(String::new(), |mut hex, byte| {
            write!(hex, "{byte:02x}").unwrap();
            hex
        })
}

#[test]
fn the_archived_go_gate_is_kept_unchanged_and_never_re_scored() {
    let archive = bench().join("gates/ruleset-11");
    for (name, want) in [
        (
            "suite.json",
            "afcd7feacbd6a5ddbaf30f1d69b2750749d92f30d52bde95816168ef2f2c895b",
        ),
        (
            "baseline.jsonl",
            "4cb9921ec9f31bba9899e3c4cd6dc0ebe4cbead1039fd5947f3f39ec8fb1f78c",
        ),
        (
            "candidate.jsonl",
            "6aaa3781211f46b207af2ef8b6810c7cd398b9641ea227ddfedb1bdf7352fc05",
        ),
        (
            "report.txt",
            "68a1af81c8de041ca863ac004dbac4de83c2d854ada2588558eed9040516e7c6",
        ),
    ] {
        assert_eq!(
            sha256(&archive.join(name)),
            want,
            "{name}: the archive records what was scored and never changes"
        );
    }
    let suite = Suite::load(&archive.join("suite.json")).unwrap();
    assert_eq!(
        gate::load(&suite).unwrap_err().to_string(),
        "gate: bootstrap_seed is a pair, the seed of the Go bench's PCG; archived Go gates are kept as records and are not re-scored"
    );
    for name in ["baseline.jsonl", "candidate.jsonl"] {
        let error = read_records(&archive.join(name), "records")
            .unwrap_err()
            .to_string();
        assert!(error.contains("line 1"), "{name}: {error}");
    }
}

#[test]
fn the_committed_suite_registers_no_gate() {
    let suite = Suite::load(&bench().join("suite.json")).unwrap();
    assert!(gate::load(&suite).unwrap().is_none());
}

#[test]
fn the_committed_go_baseline_covers_every_run_of_the_suite() {
    let suite = Suite::load(&bench().join("suite.json")).unwrap();
    let plan = suite.plan(Selection::All).unwrap();
    let text = fs::read_to_string(bench().join("baseline.jsonl")).unwrap();
    let have: std::collections::BTreeSet<(String, u64)> = text
        .lines()
        .map(|line| {
            let run: Value = serde_json::from_str(line).unwrap();
            (
                run["scenario"].as_str().unwrap().to_owned(),
                run["seed"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(have.len(), text.lines().count(), "no run twice");
    assert_eq!(have.len(), plan.runs.len());
    for run in &plan.runs {
        assert!(
            have.contains(&(run.scenario.clone(), run.seed)),
            "{}",
            run.name()
        );
    }
}

#[test]
fn the_header_names_the_split_the_gate_is_judged_on() {
    for split in ["dev", "holdout", "all"] {
        let mut object = gate_object();
        object["split"] = json!(split);
        object["conclude"] = json!([]);
        let gate = resolved(&object);
        let report = text(&gate, &Verdict::Refused("x".to_owned()));
        assert!(
            report.contains(&format!("at abc on every {split} seed;")),
            "{report}"
        );
    }
}
