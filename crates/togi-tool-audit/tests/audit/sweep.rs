use std::fs;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use std::collections::BTreeMap;
use togi_journal::StateDir;
use togi_journal::fs::MemFs;

use togi_journal::Event;
use togi_tool_audit::{
    Finding, Kind, Record, Split, Status, audit_directory_with, audit_with, check_records,
    read_records_from, run_directory, sweep_suite, timing,
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

fn memory(events: &[Event]) -> MemFs {
    let fs = MemFs::new();
    let journal: String = events
        .iter()
        .map(|event| event.encode().unwrap() + "\n")
        .collect();
    fs.put("/state/events.jsonl", journal.into_bytes());
    fs.put(
        "/state/state.json",
        serde_json::to_vec(&serde_json::json!({
            "facts": togi_journal::State::replay(events).unwrap(),
            "tuner": togi_tuner::replay(events).unwrap().project(),
        }))
        .unwrap(),
    );
    fs
}

fn lexical(path: &Path) -> PathBuf {
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                resolved.pop();
            }
            other @ (std::path::Component::Prefix(_)
            | std::path::Component::RootDir
            | std::path::Component::Normal(_)) => resolved.push(other),
        }
    }
    resolved
}

#[derive(Default)]
struct Directories(BTreeMap<PathBuf, MemFs>);

impl Directories {
    fn put(&mut self, path: &Path, events: &[Event]) {
        self.0.insert(lexical(path), memory(events));
    }

    fn inspect(
        &self,
        path: &Path,
        simulated: bool,
    ) -> (Vec<Finding>, usize, Vec<togi_tool_audit::JournalSkip>) {
        audit_directory_with(path, simulated, |path| {
            let fs = self
                .0
                .get(&lexical(path))
                .ok_or_else(|| "not a directory".to_owned())?;
            #[expect(
                clippy::disallowed_methods,
                reason = "report content fixtures open only the injected in-memory journal"
            )]
            let state = StateDir::simulated(fs.clone());
            state.map_err(|error| error.to_string())
        })
    }

    fn audit(&self, dirs: &[PathBuf], records: &[Record], root: &Path) -> togi_tool_audit::Report {
        audit_with(
            dirs,
            records,
            root,
            || (),
            |path, simulated| self.inspect(path, simulated),
        )
        .unwrap()
    }
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
    assert_eq!(
        found[0].reason,
        "wall time 11.000s exceeds 10x scenario median 1.000s"
    );
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
    assert_snapshot(
        &suite,
        include_str!(
            "snapshots/audit__sweep__the_sweep_suite_is_dev_only_over_seeds_one_to_n_with_absolute_machines.snap"
        ),
    );
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
    let mut dirs = Directories::default();
    let root = Path::new("/fixture/bench");
    let respelled = PathBuf::from("/fixture/bench/./a//dev-1");
    let rec = record("a", Status::Concluded, 1.0);
    dirs.put(&run_directory(root, &rec), &dirty());
    let report = dirs.audit(&[respelled], &[rec], root);
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
    assert_snapshot(
        &format!("{found:#?}"),
        include_str!(
            "snapshots/audit__sweep__a_directory_is_audited_with_the_run_it_belongs_to_whatever_its_spelling.snap"
        ),
    );
    assert_eq!((report.directories, report.journals), (1, 1));
}

#[test]
fn a_directory_is_audited_with_its_run_whatever_dots_its_path_has() {
    let mut dirs = Directories::default();
    let root = Path::new("/fixture/bench");
    let rec = record("a", Status::Concluded, 1.0);
    dirs.put(&run_directory(root, &rec), &[header(), phase(2, 0, None)]);
    let respelled = PathBuf::from("/fixture/elsewhere/../bench/a/./dev-1");
    let report = dirs.audit(&[respelled], &[rec], root);
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
    let mut dirs = Directories::default();
    let root = Path::new("/fixture");
    let path = root.join("a/dev-1");
    let open = vec![header(), phase(2, 0, None)];
    dirs.put(&path, &open);
    let rec = record("a", Status::Concluded, 1.0);
    let simulated = dirs.audit(std::slice::from_ref(&path), &[rec], root);
    assert_eq!(
        kinds(&simulated.findings),
        [Kind::Audit(togi_tool_audit::Check::Termination)]
    );
    let given = dirs.audit(&[path], &[], root);
    assert!(given.findings.is_empty());
}

#[test]
fn an_unreadable_or_missing_directory_is_a_finding_not_a_crash() {
    let root = Path::new("/fixture");
    let mut dirs = Directories::default();
    let broken = MemFs::new();
    broken.put("/state/events.jsonl", b"not json\n".to_vec());
    dirs.0.insert(root.join("broken"), broken);
    dirs.0.insert(root.to_path_buf(), MemFs::new());
    let report = dirs.audit(
        &[
            root.join("missing"),
            root.join("broken"),
            root.to_path_buf(),
        ],
        &[],
        root,
    );
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
    let mut dirs = Directories::default();
    let root = Path::new("/fixture");
    dirs.put(root, &clean());
    let report = dirs.audit(&[root.to_path_buf()], &[], root);
    assert_eq!((report.journals, report.findings.len()), (1, 0));
}

fn records_file(text: &str) -> Result<Vec<Record>, String> {
    read_records_from(Path::new("[tmp]/records.jsonl"), text.as_bytes())
        .map_err(|error| error.to_string())
}

#[test]
fn record_reader_errors_keep_the_source_path() {
    struct Broken;
    impl std::io::Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("record stream failed"))
        }
    }
    let error = read_records_from(Path::new("/fixture/records.jsonl"), Broken).unwrap_err();
    assert_eq!(
        error.to_string(),
        "open run records /fixture/records.jsonl: record stream failed",
    );
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
    assert_snapshot(
        &format!("{errors:#?}"),
        include_str!("snapshots/audit__sweep__records_that_name_no_run_are_refused.snap"),
    );
    assert!(check_records(&[]).is_err());
}

#[test]
fn the_reported_check_spellings_are_pinned() {
    let root = Path::new("/fixture");
    let mut records = vec![
        record("a", Status::Concluded, 1.0),
        record("a", Status::Concluded, 1.0),
        record("a", Status::Error, 11.0),
    ];
    records[2].seed = 3;
    let mut findings = timing(&records, root);
    findings.extend(
        Directories::default()
            .audit(&[root.join("missing")], &[], root)
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

#[test]
fn real_fs_auditing_a_directory_creates_its_lock_file_and_sets_its_mode() {
    use std::os::unix::fs::PermissionsExt as _;

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    write_state_dir(&dir, &clean());
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!dir.join("lock").exists());
    let report = audit_with(
        std::slice::from_ref(&dir),
        &[],
        Path::new(""),
        || (),
        disk_inspect,
    )
    .unwrap();
    assert!(report.findings.is_empty(), "{:?}", report.findings);
    assert!(dir.join("lock").is_file());
    assert_eq!(
        fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o711
    );
}

#[test]
fn real_fs_a_lock_failure_reports_its_operation_path_and_cause_once() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    fs::create_dir_all(dir.join("lock")).unwrap();
    let (findings, journals, skips) = togi_tool_audit::audit_directory(&dir, false);
    assert_eq!(journals, 0);
    assert!(skips.is_empty());
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].check, Kind::Unreadable);
    assert_eq!(
        findings[0].reason,
        format!(
            "lock {}: Is a directory (os error 21)",
            dir.join("lock").display()
        )
    );
}

#[test]
fn a_violation_in_an_archived_journal_names_the_archive() {
    let mut dirs = Directories::default();
    let dir = Path::new("/fixture/state");
    dirs.put(dir, &clean());
    let archive: String = dirty()
        .iter()
        .map(|event| event.encode().unwrap() + "\n")
        .collect();
    dirs.0.get(dir).unwrap().put(
        "/state/archive/20251231T000000Z.jsonl",
        archive.into_bytes(),
    );
    let report = dirs.audit(&[dir.to_path_buf()], &[], Path::new("/"));
    assert_eq!(report.journals, 2);
    let journals: Vec<_> = report
        .findings
        .iter()
        .map(|finding| finding.journal.as_deref())
        .collect();
    assert_eq!(
        journals,
        [Some("20251231T000000Z")],
        "{:?}",
        report.findings
    );
    assert_eq!(
        report.findings[0].check,
        Kind::Audit(togi_tool_audit::Check::Avoidance)
    );
}

#[test]
fn a_run_directory_is_named_as_the_bench_names_it() {
    let mut rec = record("a", Status::Concluded, 1.0);
    rec.split = Split::Holdout;
    rec.seed = 3;
    assert_eq!(
        run_directory(Path::new("root"), &rec),
        Path::new("root/a/holdout-3")
    );
    rec.split = Split::Dev;
    assert_eq!(
        run_directory(Path::new("root"), &rec),
        Path::new("root/a/dev-3")
    );
}

#[test]
fn each_directory_is_audited_while_the_hook_holds_its_guard() {
    use std::cell::{Cell, RefCell};

    struct Guard<'a> {
        audited: &'a Cell<usize>,
        released: &'a Cell<usize>,
        seen: &'a RefCell<Vec<usize>>,
    }
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            self.seen.borrow_mut().push(self.audited.get());
            self.released.set(self.released.get() + 1);
        }
    }

    let mut memory = Directories::default();
    let dirs: Vec<PathBuf> = ["one", "two", "three"]
        .iter()
        .map(|name| Path::new("/fixture").join(name))
        .collect();
    for dir in &dirs {
        memory.put(dir, &clean());
    }
    let released = Cell::new(0);
    let seen = RefCell::new(Vec::new());
    let taken = RefCell::new(Vec::new());
    let audited = Cell::new(0);
    let report = audit_with(
        &dirs,
        &[],
        Path::new("/"),
        || {
            taken.borrow_mut().push(audited.get());
            Guard {
                audited: &audited,
                released: &released,
                seen: &seen,
            }
        },
        |path, simulated| {
            audited.set(audited.get() + 1);
            memory.inspect(path, simulated)
        },
    )
    .unwrap();
    assert_eq!(report.directories, 3);
    assert_eq!(released.get(), 3);
    assert_eq!(*taken.borrow(), [0, 1, 2], "the guard precedes each audit");
    assert_eq!(*seen.borrow(), [1, 2, 3], "the guard outlives each audit");
}
