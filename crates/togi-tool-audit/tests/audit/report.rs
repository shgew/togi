use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use togi_journal::fs::MemFs;
use togi_journal::{Event, StateDir};
use togi_tool_audit::{
    Check, Completion, DirectoryAudit, Finding, Kind, Report, audit_directory_with, audit_with,
};

use crate::fixtures::*;

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
        serde_json::to_vec(&state_document(events)).unwrap(),
    );
    fs
}

#[derive(Default)]
struct Directories(BTreeMap<PathBuf, MemFs>);

impl Directories {
    fn put(&mut self, path: &Path, events: &[Event]) {
        self.0.insert(path.to_path_buf(), memory(events));
    }

    fn inspect(&self, path: &Path, completion: Completion) -> DirectoryAudit {
        audit_directory_with(path, completion, |path| {
            let fs = self
                .0
                .get(path)
                .ok_or(togi_tool_audit::OpenError::NotADirectory)?;
            #[expect(
                clippy::disallowed_methods,
                reason = "report content fixtures open only the injected in-memory journal"
            )]
            let state = StateDir::simulated(fs.clone());
            Ok(state?)
        })
    }

    fn audit(&self, dirs: &[PathBuf]) -> Report {
        audit_with(dirs, |path| self.inspect(path, Completion::InProgress))
    }
}

fn clean() -> Vec<Event> {
    vec![header(), stop(2)]
}

fn dirty() -> Vec<Event> {
    vec![header(), phase(2, 0, Some(-30)), applied(3, [-30, 0])]
}

#[test]
fn a_finished_run_must_conclude_but_one_in_progress_need_not() {
    let mut dirs = Directories::default();
    let path = Path::new("/fixture/a/dev-1");
    dirs.put(path, &[header(), phase(2, 0, None)]);
    let finished = dirs.inspect(path, Completion::Finished);
    assert_eq!(kinds(&finished.findings), [Kind::Audit(Check::Termination)]);
    assert!(
        dirs.inspect(path, Completion::InProgress)
            .findings
            .is_empty()
    );
}

#[test]
fn an_unreadable_or_missing_directory_is_a_finding_not_a_crash() {
    let root = Path::new("/fixture");
    let mut dirs = Directories::default();
    let broken = MemFs::new();
    broken.put("/state/events.jsonl", b"not json\n".to_vec());
    dirs.0.insert(root.join("broken"), broken);
    dirs.0.insert(root.to_path_buf(), MemFs::new());
    let report = dirs.audit(&[
        root.join("missing"),
        root.join("broken"),
        root.to_path_buf(),
    ]);
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
    let report = dirs.audit(&[root.to_path_buf()]);
    assert_eq!((report.journals, report.findings.len()), (1, 0));
}

#[test]
fn the_reported_check_spellings_are_pinned() {
    let mut dirs = Directories::default();
    let root = Path::new("/fixture");
    dirs.put(&root.join("dirty"), &dirty());
    let report = dirs.audit(&[root.join("dirty"), root.join("missing")]);
    let spelled: Vec<String> = report
        .findings
        .iter()
        .map(|finding| {
            let value = serde_json::to_value(finding).unwrap();
            value["check"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(spelled, ["avoidance", "unreadable"]);
}

#[test]
fn real_fs_auditing_a_directory_creates_its_lock_file_and_sets_its_mode() {
    use std::os::unix::fs::PermissionsExt as _;

    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("state");
    write_state_dir(&dir, &clean());
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!dir.join("lock").exists());
    let report = audit_with(std::slice::from_ref(&dir), |dir| {
        disk_inspect(dir, Completion::Finished)
    });
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
    let audited = togi_tool_audit::audit_directory(&dir, Completion::InProgress);
    assert_eq!(audited.journals, 0);
    assert!(audited.skipped.is_empty());
    let findings = audited.findings;
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
    let report = dirs.audit(&[dir.to_path_buf()]);
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
    assert_eq!(report.findings[0].check, Kind::Audit(Check::Avoidance));
}
