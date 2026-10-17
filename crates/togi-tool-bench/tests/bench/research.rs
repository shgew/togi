//! `togi-tool-bench research`, run as a process in a fixture repository with a fake `just`.
#![expect(clippy::disallowed_methods, reason = "the tests run the built command")]

use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use crate::git::{isolate_git, repo};
use flate2::Compression;
use flate2::write::GzEncoder;
use rustix::process::{Pid, Signal, kill_process};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

const SESSIONS: [&str; 7] = [
    "20260924T204352Z",
    "20260926T151414Z",
    "20260927T221954Z",
    "20260929T180308Z",
    "20261002T004254Z",
    "20261002T235548Z",
    "20261004T000701Z",
];
const FACTS: &str = "facts/original.jsonl.gz";
const FORWARD: [&str; 5] = ["forward", "--facts", FACTS, "--seal", "1"];

struct Fixture {
    _dir: tempfile::TempDir,
    base: PathBuf,
    repo: PathBuf,
    manifest: PathBuf,
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

fn git(fixture_base: &Path, repo: &Path, args: &[&str]) -> String {
    let mut command = Command::new("git");
    isolate_git(&mut command, &fixture_base.join("githome"));
    let output = command.current_dir(repo).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A repository holding a campaign's pinned files at one commit, a fake `just` that prints its arguments and
/// sleeps for `$SLEEP` seconds, and a valid #427 manifest outside the repository.
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(dir.path()).unwrap();
    let repo = repo(&base);
    let rows: String = SESSIONS.iter().fold(String::new(), |mut out, session| {
        let _ = writeln!(out, "{{\"session\":\"{session}\"}}");
        out
    });
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(rows.as_bytes()).unwrap();
    let files: [(&str, Vec<u8>); 6] = [
        (".gitignore", b"/runs/\n".to_vec()),
        ("Cargo.lock", b"lock".to_vec()),
        ("flake.lock", b"flake".to_vec()),
        ("rust-toolchain.toml", b"toolchain".to_vec()),
        ("tools/fit/research/427.md", b"brief".to_vec()),
        (FACTS, encoder.finish().unwrap()),
    ];
    for (path, bytes) in &files {
        let path = repo.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
    git(&base, &repo, &["add", "."]);
    git(
        &base,
        &repo,
        &[
            "-c",
            "user.name=bench",
            "-c",
            "user.email=bench@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "pins",
        ],
    );
    let head = git(&base, &repo, &["rev-parse", "HEAD"]);
    let bin = base.join("bin");
    fs::create_dir(&bin).unwrap();
    let just = bin.join("just");
    fs::write(
        &just,
        "#!/bin/sh\necho \"just $*\"\nexec sleep \"${SLEEP:-0}\"\n",
    )
    .unwrap();
    fs::set_permissions(&just, fs::Permissions::from_mode(0o755)).unwrap();
    let frozen: Vec<Value> = files[1..5]
        .iter()
        .map(|(path, bytes)| json!({"path": path, "sha256": hex(bytes)}))
        .collect();
    let manifest = json!({
        "issue": 427,
        "mode": "model-development",
        "hypotheses": [{"id": "h1", "statement": "a shared core term"}],
        "checkout": head,
        "reference": head,
        "inputs": [{
            "path": FACTS,
            "sha256": hex(rows.as_bytes()),
            "development": SESSIONS[..6],
            "confirmation": [SESSIONS[6]],
        }],
        "suite": null,
        "frozen": frozen,
        "writable": ["crates/togi-sim/src"],
        "output": base.join("record/t1"),
        "confirmations": 1,
    });
    let manifest_path = base.join("manifest.json");
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
    Fixture {
        _dir: dir,
        base,
        repo,
        manifest: manifest_path,
    }
}

impl Fixture {
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_togi-tool-bench"));
        isolate_git(&mut command, &self.base.join("githome"));
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![self.base.join("bin")];
        paths.extend(std::env::split_paths(&path));
        command
            .current_dir(&self.repo)
            .env("PATH", std::env::join_paths(paths).unwrap())
            .arg("research")
            .arg("427")
            .args(args)
            .stdin(Stdio::null());
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn run_dir(&self) -> PathBuf {
        self.base.join("record/t1")
    }

    fn exec(&self, role: &str, command: &[&str]) -> Command {
        let run = self.run_dir();
        let mut args = vec![
            "exec",
            "--run",
            run.to_str().unwrap(),
            "--candidate",
            "h1",
            "--role",
            role,
            "--",
        ];
        args.extend_from_slice(command);
        self.command(&args)
    }

    fn ledger(&self) -> Vec<Value> {
        fs::read_to_string(self.run_dir().join("experiments.jsonl"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn real_process_a_campaign_starts_runs_an_evaluator_stops_and_reports() {
    let fixture = fixture();
    let manifest = fixture.manifest.to_str().unwrap();
    let check = fixture.run(&["check", "--manifest", manifest]);
    assert!(check.status.success(), "{}", text(&check.stderr));
    assert!(!fixture.run_dir().exists());

    let start = fixture.run(&["start", "--manifest", manifest]);
    assert!(start.status.success(), "{}", text(&start.stderr));
    assert_eq!(
        fs::read(fixture.run_dir().join("manifest.json")).unwrap(),
        fs::read(manifest).unwrap()
    );
    let again = fixture.run(&["start", "--manifest", manifest]);
    assert!(
        text(&again.stderr).contains("already exists"),
        "{}",
        text(&again.stderr)
    );

    let reference = fixture.exec("reference", &FORWARD).output().unwrap();
    assert!(reference.status.success(), "{}", text(&reference.stderr));
    let raw = fs::read_to_string(fixture.run_dir().join("raw/0001-h1-reference.stdout")).unwrap();
    assert_eq!(
        raw,
        format!("just forward --facts {FACTS} --seal 1 --jobs 4\n")
    );

    let refused = fixture
        .exec(
            "candidate",
            &[
                "forward",
                "--facts",
                FACTS,
                "--seal",
                "1",
                "--reference-machine",
                "m",
            ],
        )
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(1));
    assert!(
        text(&refused.stderr).contains("--reference-machine is not allowed"),
        "{}",
        text(&refused.stderr)
    );

    let run = fixture.run_dir();
    let run = run.to_str().unwrap();
    let stop = fixture.run(&[
        "stop",
        "--run",
        run,
        "--reason",
        "queue-exhausted",
        "--detail",
        "one hypothesis",
    ]);
    assert!(stop.status.success(), "{}", text(&stop.stderr));
    let after = fixture.exec("candidate", &FORWARD).output().unwrap();
    assert!(
        text(&after.stderr).contains("already stopped"),
        "{}",
        text(&after.stderr)
    );

    let notes = fixture.base.join("notes.md");
    fs::write(&notes, "Nothing was kept.\n").unwrap();
    let report = fixture.run(&["report", "--run", run, "--notes", notes.to_str().unwrap()]);
    assert!(report.status.success(), "{}", text(&report.stderr));
    let report = fs::read_to_string(fixture.run_dir().join("report.md")).unwrap();
    assert!(
        report.contains("- Stop: queue exhausted (one hypothesis)"),
        "{report}"
    );
    assert!(report.contains("Nothing was kept."), "{report}");

    let ledger = fixture.ledger();
    let events: Vec<&str> = ledger
        .iter()
        .map(|record| record["event"].as_str().unwrap())
        .collect();
    assert_eq!(
        events,
        [
            "campaign_started",
            "experiment_started",
            "experiment_ended",
            "campaign_stopped"
        ]
    );
}

#[test]
fn real_process_a_changed_frozen_file_stops_the_campaign() {
    let fixture = fixture();
    let start = fixture.run(&["start", "--manifest", fixture.manifest.to_str().unwrap()]);
    assert!(start.status.success(), "{}", text(&start.stderr));
    fs::write(
        fixture.repo.join("tools/fit/research/427.md"),
        "a loosened brief",
    )
    .unwrap();
    let output = fixture.exec("reference", &FORWARD).output().unwrap();
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("evidence or a frozen input changed"),
        "{stderr}"
    );
    assert_eq!(
        fixture.ledger().last().unwrap()["reason"],
        "evidence-changed"
    );
}

#[test]
fn real_process_a_replaced_manifest_stops_the_campaign() {
    let fixture = fixture();
    let start = fixture.run(&["start", "--manifest", fixture.manifest.to_str().unwrap()]);
    assert!(start.status.success(), "{}", text(&start.stderr));
    let copy = fixture.run_dir().join("manifest.json");
    let mut manifest: Value = serde_json::from_slice(&fs::read(&copy).unwrap()).unwrap();
    manifest["limits"] = json!({"wall_minutes": 6000, "candidates": 12, "jobs": 4});
    fs::write(&copy, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let output = fixture.exec("reference", &FORWARD).output().unwrap();
    let stderr = text(&output.stderr);
    assert!(
        stderr.contains("not the manifest the campaign started with"),
        "{stderr}"
    );
    assert_eq!(
        fixture.ledger().last().unwrap()["reason"],
        "evidence-changed"
    );
}

#[test]
fn real_process_a_deleted_manifest_stops_the_campaign() {
    let fixture = fixture();
    let start = fixture.run(&["start", "--manifest", fixture.manifest.to_str().unwrap()]);
    assert!(start.status.success(), "{}", text(&start.stderr));
    fs::remove_file(fixture.run_dir().join("manifest.json")).unwrap();
    let output = fixture.exec("reference", &FORWARD).output().unwrap();
    let stderr = text(&output.stderr);
    assert!(stderr.contains("manifest.json is missing"), "{stderr}");
    assert_eq!(
        fixture.ledger().last().unwrap()["reason"],
        "evidence-changed"
    );
}

#[test]
fn real_process_an_outcome_records_its_metrics_file() {
    let fixture = fixture();
    let start = fixture.run(&["start", "--manifest", fixture.manifest.to_str().unwrap()]);
    assert!(start.status.success(), "{}", text(&start.stderr));
    let reference = fixture.exec("reference", &FORWARD).output().unwrap();
    assert!(reference.status.success(), "{}", text(&reference.stderr));
    let run = fixture.run_dir();
    let run = run.to_str().unwrap();
    let metrics = fixture.base.join("metrics.json");
    let metrics_arg = metrics.to_str().unwrap();
    let outcome = |content: &[u8]| {
        fs::write(&metrics, content).unwrap();
        fixture.run(&[
            "outcome",
            "--run",
            run,
            "--candidate",
            "h1",
            "--outcome",
            "rejected",
            "--reason",
            "worse than the reference",
            "--metrics",
            metrics_arg,
        ])
    };
    let positional = outcome(b"[0.25]");
    assert_eq!(positional.status.code(), Some(1));
    assert!(
        text(&positional.stderr).contains("decode the metrics"),
        "{}",
        text(&positional.stderr)
    );
    let recorded = outcome(br#"{"brier": 0.12345678901234566}"#);
    assert!(recorded.status.success(), "{}", text(&recorded.stderr));
    let last = fixture.ledger().last().unwrap().clone();
    assert_eq!(last["event"], "outcome");
    assert_eq!(last["outcome"], "rejected");
    assert_eq!(last["metrics"]["brier"], 0.123_456_789_012_345_66);
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn real_process_a_termination_interrupts_the_run_while_the_lock_refuses_a_second_dispatcher() {
    let fixture = fixture();
    let start = fixture.run(&["start", "--manifest", fixture.manifest.to_str().unwrap()]);
    assert!(start.status.success(), "{}", text(&start.stderr));
    let mut exec = fixture.exec("reference", &FORWARD);
    let child = exec
        .env("SLEEP", "60")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let raw = fixture.run_dir().join("raw/0001-h1-reference.stdout");
    wait_for("the evaluator", || {
        fs::read_to_string(&raw).is_ok_and(|out| out.starts_with("just"))
    });

    let run = fixture.run_dir();
    let busy = fixture.run(&[
        "stop",
        "--run",
        run.to_str().unwrap(),
        "--reason",
        "interrupted",
    ]);
    assert!(
        text(&busy.stderr).contains("another dispatcher holds"),
        "{}",
        text(&busy.stderr)
    );

    let started = Instant::now();
    kill_process(Pid::from_child(&child), Signal::TERM).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(output.status.code(), Some(1));
    assert!(
        text(&output.stderr).contains("run interrupted"),
        "{}",
        text(&output.stderr)
    );
    let last = fixture.ledger().last().unwrap().clone();
    assert_eq!(last["event"], "experiment_ended");
    assert_eq!(last["end"]["kind"], "interrupted");

    let resumed = fixture.run(&["resume", "--run", run.to_str().unwrap()]);
    assert!(resumed.status.success(), "{}", text(&resumed.stderr));
    assert_eq!(
        fixture.ledger().last().unwrap()["event"],
        "campaign_resumed"
    );
}
