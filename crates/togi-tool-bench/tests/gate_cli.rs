//! The `togi-tool-bench` command with a registered gate, run as a process over a fake simulator.
#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "the tests run the built command and panic on a broken fixture"
)]

mod common;
mod fake;
mod session;

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use common::{isolate_git, repo};
use fake::Bench;
use serde_json::json;
use togi_sim::file;
use togi_tool_bench::{Record, read_records, write_records};

struct Case<'a> {
    name: &'a str,
    baseline: Vec<Record>,
    gated: bool,
    exit: i32,
    generic: &'a str,
    criteria: Vec<String>,
    result: String,
}

struct Invoked {
    code: i32,
    stdout: String,
    stderr: String,
}

fn invoke(bench: &Bench, suite: &Path, baseline: Option<&Path>, out: &Path) -> Invoked {
    let mut command = Command::new(env!("CARGO_BIN_EXE_togi-tool-bench"));
    isolate_git(&mut command, &bench.path("githome"));
    command
        .current_dir(repo(bench.dir.path()))
        .arg("--suite")
        .arg(suite)
        .args(["--split", "dev", "--jobs", "1", "--sim"])
        .arg(bench.simulator("sim", 0, "", ""))
        .arg("--cache")
        .arg(bench.path("cache"))
        .arg("--out")
        .arg(out);
    if let Some(baseline) = baseline {
        command.arg("--baseline").arg(baseline);
    }
    let output: Output = command.output().unwrap();
    Invoked {
        code: output.status.code().unwrap(),
        stdout: String::from_utf8(output.stdout).unwrap(),
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

fn shared_voltage() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/bench/machines/shared-voltage.json")
}

#[test]
fn a_registered_gate_judges_the_run_against_the_baseline_and_sets_the_exit() {
    let bench = Bench::new();
    let machine = shared_voltage();
    let definition = file::load(&machine).unwrap();
    let journal = session::session(&definition, 1, None, 2).events;
    let lines: Vec<String> = journal
        .iter()
        .map(|event| serde_json::to_string(event).unwrap())
        .collect();
    fs::write(bench.path("journal.jsonl"), lines.join("\n") + "\n").unwrap();

    let scenarios = json!([{"name": "tiny", "machine": machine, "dev": [1]}]);
    let plain = bench.path("plain.json");
    fs::write(&plain, json!({"scenarios": scenarios}).to_string()).unwrap();
    let recorded = bench.path("recorded.jsonl");
    let first = invoke(&bench, &plain, None, &recorded);
    assert_eq!(first.code, 0, "{}", first.stderr);
    let original = read_records(&recorded, "records").unwrap().remove(0);
    let hazard = original.worst_r7_hazard_per_h.unwrap();

    let gated = bench.path("gated.json");
    let gate = json!({
        "id": "cli-integration",
        "baseline": {"ruleset": original.ruleset, "commits": [original.commit]},
        "split": "dev",
        "gated": ["tiny"],
        "pooled": ["tiny"],
        "quantiles": [0.5, 0.9],
        "confidence": 0.95,
        "resamples": 10000,
        "bootstrap_seed": 1,
        "max_time_ratio": 2.0,
    });
    fs::write(
        &gated,
        json!({"gate": gate, "scenarios": scenarios}).to_string(),
    )
    .unwrap();

    let with_hazard = |hazard: f64| Record {
        worst_r7_hazard_per_h: Some(hazard),
        ..original.clone()
    };
    let passing = with_hazard(hazard + 1.0);
    let slow = Record {
        sim_hours: passing.sim_hours / 3.0,
        ..passing.clone()
    };
    let wrong_ruleset = Record {
        ruleset: passing.ruleset + 1,
        ..passing.clone()
    };
    let wrong_commit = Record {
        commit: format!("{}-wrong", passing.commit),
        ..passing.clone()
    };

    let criteria = |baseline: &Record, time: &str, pooled: &str| {
        let base = baseline.worst_r7_hazard_per_h.unwrap();
        let change = hazard - base;
        let mut lines = vec![
            "gate conclusion tiny baseline_concluded=1 candidate_concluded=1 lost=0 lost_seeds=[] threshold=lost==0 PASS".to_owned(),
        ];
        for label in ["median", "p90"] {
            lines.push(format!(
                "gate {label} tiny baseline={base:.6} candidate={hazard:.6} change={change:+.6} ci=[{change:+.6},{change:+.6}] threshold=ci_low<=0 PASS"
            ));
        }
        lines.push(time.to_owned());
        lines.push(format!(
            "gate pooled_median tiny baseline={base:.6} candidate={hazard:.6} change={change:+.6} threshold=change<0 {pooled}"
        ));
        lines
    };
    let on_time = "gate time tiny ratio=1.0000 timed=1 threshold=ratio<=2 PASS";
    let pass_criteria = criteria(&passing, on_time, "PASS");
    let equal_criteria = criteria(&original, on_time, "FAIL");
    let mut slow_criteria = criteria(&slow, on_time, "PASS");
    slow_criteria[3] = "gate time tiny ratio=3.0000 timed=1 threshold=ratio<=2 FAIL".to_owned();
    let header = format!(
        "gate cli-integration: judged against ruleset {} at {} on every dev seed; quantiles of worst_r7_hazard_per_h fail only when the 95% interval of candidate minus baseline (10000 paired resamples, seed 1) lies above 0; time is the geometric mean ratio over pairs both concluded.",
        original.ruleset, original.commit
    );
    let refused = |record: &Record| {
        format!(
            "gate cli-integration: FAIL refused: baseline run tiny/1 is ruleset {} at {}, not ruleset {} at {}",
            record.ruleset, record.commit, original.ruleset, original.commit
        )
    };

    let cases = [
        Case { name: "pass", baseline: vec![passing.clone()], gated: true, exit: 0, generic: "NEUTRAL", criteria: pass_criteria, result: "gate cli-integration: PASS 5 criteria".into() },
        Case { name: "equality fails the pooled improvement", baseline: vec![original.clone()], gated: true, exit: 1, generic: "NEUTRAL", criteria: equal_criteria, result: "gate cli-integration: FAIL 1 of 5 criteria".into() },
        Case { name: "time fails", baseline: vec![slow.clone()], gated: true, exit: 1, generic: "REJECT", criteria: slow_criteria, result: "gate cli-integration: FAIL 1 of 5 criteria".into() },
        Case { name: "missing required seed refused", baseline: vec![], gated: true, exit: 1, generic: "NEUTRAL", criteria: vec![], result: "gate cli-integration: FAIL refused: incomplete pairing: tiny seed 1 has no baseline run; the gate needs every dev seed of its scenarios".into() },
        Case { name: "wrong ruleset refused", baseline: vec![wrong_ruleset.clone()], gated: true, exit: 1, generic: "NEUTRAL", criteria: vec![], result: refused(&wrong_ruleset) },
        Case { name: "wrong commit refused", baseline: vec![wrong_commit.clone()], gated: true, exit: 1, generic: "NEUTRAL", criteria: vec![], result: refused(&wrong_commit) },
        Case { name: "no gate neutral", baseline: vec![original.clone()], gated: false, exit: 0, generic: "NEUTRAL", criteria: vec![], result: String::new() },
        Case { name: "no gate reject remains diagnostic", baseline: vec![slow.clone()], gated: false, exit: 0, generic: "REJECT", criteria: vec![], result: String::new() },
    ];
    for case in cases {
        let baseline = bench.path("baseline.jsonl");
        write_records(&baseline, &case.baseline).unwrap();
        let out = bench.path("candidate.jsonl");
        let suite = if case.gated { &gated } else { &plain };
        let invoked = invoke(&bench, suite, Some(&baseline), &out);
        let name = case.name;
        let stdout = &invoked.stdout;
        assert_eq!(
            invoked.code, case.exit,
            "{name}\n{stdout}\n{}",
            invoked.stderr
        );
        let want_err = if case.exit == 0 {
            ""
        } else {
            "togi-tool-bench: gate failed\n"
        };
        assert_eq!(invoked.stderr, want_err, "{name}\n{stdout}");
        let candidate = read_records(&out, "records").unwrap();
        assert_eq!(candidate.len(), 1, "{name}");
        assert_eq!(candidate[0].worst_r7_hazard_per_h, Some(hazard), "{name}");
        let comparison = stdout
            .find("\ncomparison:")
            .unwrap_or_else(|| panic!("{name}: no comparison\n{stdout}"));
        let generic = stdout
            .find(&format!("\nverdict: {} ", case.generic))
            .unwrap_or_else(|| panic!("{name}: no {} verdict\n{stdout}", case.generic));
        assert!(generic > comparison, "{name}\n{stdout}");
        if !case.gated {
            assert!(
                !stdout.contains("\ngate "),
                "{name}: a suite without a gate printed one\n{stdout}"
            );
            continue;
        }
        let registered = stdout
            .find(&format!("\n{header}\n"))
            .unwrap_or_else(|| panic!("{name}: no header\n{stdout}"));
        assert!(
            registered > generic,
            "{name}: the gate follows the generic comparison\n{stdout}"
        );
        for line in case.criteria.iter().chain([&case.result]) {
            assert!(
                stdout.contains(&format!("\n{line}\n")),
                "{name}: missing {line:?}\n{stdout}"
            );
        }
        if case.criteria.is_empty() {
            assert!(
                !stdout.contains("\ngate conclusion "),
                "{name}: a refused gate scored\n{stdout}"
            );
        }
    }
}

#[test]
fn a_suite_with_a_gate_and_no_baseline_prints_no_verdict_and_exits_0() {
    let bench = Bench::new();
    let gated = bench.path("gated.json");
    let gate = json!({
        "id": "g", "baseline": {"ruleset": 1, "commits": ["x"]}, "split": "dev", "gated": ["default"],
        "quantiles": [0.5], "confidence": 0.95, "resamples": 10, "bootstrap_seed": 1, "max_time_ratio": 2.0,
    });
    fs::write(
        &gated,
        json!({"gate": gate, "scenarios": [{"name": "default", "dev": [1]}]}).to_string(),
    )
    .unwrap();
    let invoked = invoke(&bench, &gated, None, &bench.path("out.jsonl"));
    assert_eq!(invoked.code, 0, "{}", invoked.stderr);
    assert!(!invoked.stdout.contains("\ngate "), "{}", invoked.stdout);
}

#[test]
fn a_gate_of_the_go_bench_stops_the_run_before_any_session() {
    let bench = Bench::new();
    let archived =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/bench/gates/ruleset-11/suite.json");
    let invoked = invoke(&bench, &archived, None, &bench.path("out.jsonl"));
    assert_eq!(invoked.code, 1);
    assert!(
        invoked
            .stderr
            .contains("archived Go gates are kept as records and are not re-scored"),
        "{}",
        invoked.stderr
    );
    assert!(!bench.path("args.txt").exists(), "no simulator ran");
    assert!(!bench.path("out.jsonl").exists());
}
