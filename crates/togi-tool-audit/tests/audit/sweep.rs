use std::fs;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use togi_journal::Event;
use togi_tool_audit::{
    Finding, Kind, Record, Split, Status, audit_directories, check_records, read_records,
    run_directory, sweep_suite, timing,
};

use crate::fixtures::*;

fn record(scenario: &str, status: Status, wall_s: f64) -> Record {
    Record {
        scenario: scenario.into(),
        seed: 1,
        split: Split::Dev,
        status,
        wall_s,
    }
}

fn kinds(findings: &[Finding]) -> Vec<Kind> {
    findings.iter().map(|finding| finding.check).collect()
}

#[test]
fn timing_flags_a_run_over_ten_times_its_scenario_median() {
    let concluded = |wall| record("a", Status::Concluded, wall);
    for (name, records, want) in [
        (
            "passing",
            vec![concluded(1.0), record("a", Status::DeadEnd, 10.0)],
            vec![],
        ),
        (
            "tenfold boundary is strict",
            vec![concluded(1.0), concluded(1.0), concluded(10.0)],
            vec![],
        ),
        (
            "outlier",
            vec![concluded(1.0), concluded(1.0), concluded(11.0)],
            vec![Kind::WallTime],
        ),
        (
            "scenario medians are separate",
            vec![concluded(1.0), record("b", Status::Concluded, 100.0)],
            vec![],
        ),
        (
            "execution error",
            vec![record("a", Status::Error, 1.0)],
            vec![Kind::BenchStatus],
        ),
        (
            "a boot-capped run is not a conclusion",
            vec![record("a", Status::Censored, 1.0)],
            vec![Kind::BenchStatus],
        ),
    ] {
        assert_eq!(kinds(&timing(&records, Path::new("root"))), want, "{name}");
    }
}

#[test]
fn timing_names_the_run_directory_and_its_median() {
    let records = [
        record("a", Status::Concluded, 1.0),
        record("a", Status::Concluded, 1.0),
        record("a", Status::Concluded, 11.0),
    ];
    let found = timing(&records, Path::new("root"));
    assert_eq!(found[0].directory, PathBuf::from("root/a/dev-1"));
    insta::assert_snapshot!(found[0].reason, @"wall time 11.000s exceeds 10x scenario median 1.000s");
}

fn sweep(input: &str, seeds: u64) -> Result<String, String> {
    sweep_suite(
        input.as_bytes(),
        Path::new("/suites"),
        NonZeroU64::new(seeds).unwrap(),
    )
    .map(|value| serde_json::to_string_pretty(&value).unwrap())
    .map_err(|error| error.to_string())
}

#[test]
fn the_sweep_suite_is_dev_only_over_seeds_one_to_n_with_absolute_machines() {
    let suite = sweep(
        r#"{"gate":{"id":"ruleset-11"},"scenarios":[
            {"name":"default","dev":[100],"holdout":[200],"smoke":[300]},
            {"name":"single","machine":"machines/one.json","holdout":[9]},
            {"name":"absolute","machine":"/elsewhere/abs.json","dev":[7]},
            {"name":"ensemble","machines":["","machines/two.json","/elsewhere/abs.json"],"replay":true,"dev":[5],"smoke":[6]}
        ]}"#,
        3,
    )
    .unwrap();
    insta::assert_snapshot!(suite);
}

#[test]
fn a_suite_that_cannot_be_swept_is_refused() {
    for (input, want) in [
        ("{\"scenarios\":[", "decode suite"),
        ("[]", "decode suite"),
        (r#"{"scenarios":[]}"#, "suite has no scenarios"),
        (r#"{"gate":{}}"#, "suite has no scenarios"),
    ] {
        let error = sweep(input, 3).unwrap_err();
        assert!(error.contains(want), "{input}: {error}");
    }
}

fn clean() -> Vec<Event> {
    vec![header(), stop(2)]
}

fn dirty() -> Vec<Event> {
    vec![header(), phase(2, 0, Some(-30)), applied(3, [-30, 0])]
}

#[test]
fn a_directory_is_audited_with_the_run_it_belongs_to_whatever_its_spelling() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("bench");
    let respelled = tmp.path().join("bench/./a//dev-1");
    let rec = record("a", Status::Concluded, 1.0);
    write_state_dir(&run_directory(&root, &rec), &dirty());
    let report = audit_directories(&[respelled], &[rec], &root).unwrap();
    let found: Vec<_> = report
        .findings
        .iter()
        .map(|finding| {
            (
                finding.scenario.as_deref(),
                finding.seed,
                finding.journal.as_deref(),
                finding.seq.map(togi_journal::Seq::get),
                finding.check.as_str(),
            )
        })
        .collect();
    insta::assert_debug_snapshot!(found);
    assert_eq!((report.directories, report.journals), (1, 1));
}

#[test]
fn a_directory_is_audited_with_its_run_whatever_dots_its_path_has() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("bench");
    let rec = record("a", Status::Concluded, 1.0);
    write_state_dir(&run_directory(&root, &rec), &[header(), phase(2, 0, None)]);
    fs::create_dir(tmp.path().join("elsewhere")).unwrap();
    let respelled = tmp.path().join("elsewhere/../bench/a/./dev-1");
    let report = audit_directories(&[respelled], &[rec], &root).unwrap();
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].scenario.as_deref(), Some("a"));
    assert_eq!(report.findings[0].seed, Some(1));
    assert_eq!(
        report.findings[0].check,
        Kind::Audit(togi_tool_audit::Check::Termination)
    );
}

#[test]
fn a_run_directory_must_conclude_but_a_given_one_need_not() {
    let tmp = tempfile::tempdir().unwrap();
    let open = vec![header(), phase(2, 0, None)];
    write_state_dir(&tmp.path().join("a/dev-1"), &open);
    let rec = record("a", Status::Concluded, 1.0);
    let simulated = audit_directories(&[tmp.path().join("a/dev-1")], &[rec], tmp.path()).unwrap();
    assert_eq!(
        kinds(&simulated.findings),
        [Kind::Audit(togi_tool_audit::Check::Termination)]
    );
    let given = audit_directories(&[tmp.path().join("a/dev-1")], &[], tmp.path()).unwrap();
    assert!(given.findings.is_empty());
}

#[test]
fn an_unreadable_or_missing_directory_is_a_finding_not_a_crash() {
    let tmp = tempfile::tempdir().unwrap();
    let broken = tmp.path().join("broken");
    fs::create_dir(&broken).unwrap();
    fs::write(broken.join("events.jsonl"), "not json\n").unwrap();
    let report = audit_directories(
        &[tmp.path().join("missing"), broken, tmp.path().to_path_buf()],
        &[],
        tmp.path(),
    )
    .unwrap();
    assert_eq!(report.directories, 3);
    assert_eq!(report.journals, 0);
    assert_eq!(
        kinds(&report.findings),
        [Kind::Unreadable, Kind::Unreadable]
    );
    assert_eq!(report.findings[0].reason, "not a directory");
}

#[test]
fn a_clean_directory_reports_its_journals_and_nothing_else() {
    let tmp = tempfile::tempdir().unwrap();
    write_state_dir(tmp.path(), &clean());
    let report = audit_directories(&[tmp.path().to_path_buf()], &[], tmp.path()).unwrap();
    assert_eq!((report.journals, report.findings.len()), (1, 0));
}

fn records_file(text: &str) -> Result<Vec<Record>, String> {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("records.jsonl");
    fs::write(&path, text).unwrap();
    read_records(&path).map_err(|error| {
        error
            .to_string()
            .replace(tmp.path().to_str().unwrap(), "[tmp]")
    })
}

#[test]
fn records_are_read_by_the_members_the_audit_needs() {
    let records = records_file(
        "{\"scenario\":\"a\",\"seed\":3,\"split\":\"holdout\",\"status\":\"deadend\",\"wall_s\":1.5,\"crashes\":9}\n\n",
    )
    .unwrap();
    assert_eq!(
        records,
        [Record {
            scenario: "a".into(),
            seed: 3,
            split: Split::Holdout,
            status: Status::DeadEnd,
            wall_s: 1.5,
        }]
    );
}

#[test]
fn records_that_name_no_run_are_refused() {
    let line = |scenario: &str, wall: &str| {
        format!(
            "{{\"scenario\":\"{scenario}\",\"seed\":1,\"split\":\"dev\",\"status\":\"concluded\",\"wall_s\":{wall}}}\n"
        )
    };
    let mut errors = vec![
        records_file("").unwrap_err(),
        records_file("{\"scenario\":\"a\"}\n").unwrap_err(),
        records_file("[\"a\",1,\"dev\",\"concluded\",1.0]\n").unwrap_err(),
    ];
    for (scenario, wall) in [
        ("", "1"),
        ("..", "1"),
        (".", "1"),
        ("a/b", "1"),
        ("../a", "1"),
        ("a", "-1"),
    ] {
        errors.push(records_file(&line(scenario, wall)).unwrap_err());
    }
    insta::assert_debug_snapshot!(errors);
    assert!(check_records(&[]).is_err());
}

#[test]
fn the_reported_check_spellings_are_pinned() {
    let tmp = tempfile::tempdir().unwrap();
    let mut records = vec![
        record("a", Status::Concluded, 1.0),
        record("a", Status::Concluded, 1.0),
        record("a", Status::Error, 11.0),
    ];
    records[2].seed = 3;
    let mut findings = timing(&records, tmp.path());
    findings.extend(
        audit_directories(&[tmp.path().join("missing")], &[], tmp.path())
            .unwrap()
            .findings,
    );
    let spelled: Vec<String> = findings
        .iter()
        .map(|finding| {
            let value = serde_json::to_value(finding).unwrap();
            value["check"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(spelled, ["wall_time", "bench_status", "unreadable"]);
}
