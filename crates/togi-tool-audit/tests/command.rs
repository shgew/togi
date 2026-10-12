//! The `togi-tool-audit` command, run as a process over a fake bench.
#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    clippy::needless_pass_by_value,
    reason = "the tests run the built command, panic on a broken fixture and share fixtures that take JSON literals by value"
)]

#[expect(dead_code, reason = "each test crate uses part of the shared fixtures")]
#[path = "audit/fixtures.rs"]
mod fixtures;

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use fixtures::{applied, header, phase, stop, write_state_dir};
use togi_journal::Event;

const RECORDS: &str = r#"{"scenario":"s","seed":1,"split":"dev","status":"concluded","wall_s":1.0}
{"scenario":"s","seed":2,"split":"dev","status":"error","wall_s":1.0}
"#;

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_togi-tool-audit"))
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

fn dirty() -> Vec<Event> {
    vec![header(), phase(2, 0, Some(-30)), applied(3, [-30, 0])]
}

/// What a finding says without the temporary paths: the member values that identify it.
fn brief(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(|line| {
            let finding: serde_json::Value = serde_json::from_str(line).unwrap();
            let member = |name: &str| finding.get(name).map_or("-".into(), ToString::to_string);
            format!(
                "{} {} journal={} seq={} {}: {}",
                member("scenario"),
                member("seed"),
                member("journal"),
                member("seq"),
                member("check"),
                member("reason")
            )
        })
        .collect()
}

struct Fake {
    dir: tempfile::TempDir,
}

impl Fake {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// A bench that records its arguments and suite, keeps two runs of scenario `s` (the second violating) and
    /// writes the records, then exits with `exit`.
    fn bench(&self, exit: i32) -> PathBuf {
        write_state_dir(&self.path("clean"), &[header(), stop(2)]);
        write_state_dir(&self.path("dirty"), &dirty());
        fs::write(self.path("records.jsonl"), RECORDS).unwrap();
        fs::write(
            self.path("suite.json"),
            r#"{"gate":{"id":"x"},"scenarios":[{"name":"s","machine":"m.json","dev":[100],"holdout":[200]}]}"#,
        )
        .unwrap();
        let path = self.path("bench");
        let script = format!(
            r#"#!/bin/sh
echo "$@" > '{args}'
echo "bench says hello"
while [ $# -gt 0 ]; do
  case "$1" in
    --keep) keep="$2" ;;
    --out) out="$2" ;;
    --suite) suite="$2" ;;
  esac
  shift
done
cp "$suite" '{copy}'
for n in 1 2; do
  mkdir -p "$keep/bench-1/s/dev-$n"
done
cp '{clean}'/* "$keep/bench-1/s/dev-1/"
cp '{dirty}'/* "$keep/bench-1/s/dev-2/"
cp '{records}' "$out"
exit {exit}
"#,
            args = self.path("args.txt").display(),
            copy = self.path("suite-copy.json").display(),
            clean = self.path("clean").display(),
            dirty = self.path("dirty").display(),
            records = self.path("records.jsonl").display(),
        );
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn sweep(&self, bench: &Path) -> Command {
        let mut command = command();
        command
            .arg("--bench")
            .arg(bench)
            .arg("--suite")
            .arg(self.path("suite.json"))
            .arg("--keep")
            .arg(self.path("evidence"))
            .args(["--seeds", "2", "--jobs", "3", "--timeout", "60"]);
        command
    }
}

fn run(command: &mut Command) -> Output {
    command.output().unwrap()
}

#[test]
fn help_goes_to_stdout_and_a_bad_flag_exits_two() {
    let help = run(command().arg("--help"));
    assert!(help.status.success());
    assert!(text(&help.stdout).contains("STATE-DIR"));
    for args in [
        &["--seeds", "0"][..],
        &["--jobs", "0"],
        &["--timeout", "0"],
        &["--records", "file"],
        &["--root", "dir"],
        &["--bogus"],
    ] {
        assert_eq!(run(command().args(args)).status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn given_directories_are_audited_without_a_sweep() {
    let tmp = tempfile::tempdir().unwrap();
    write_state_dir(&tmp.path().join("ok"), &[header(), stop(2)]);
    write_state_dir(&tmp.path().join("bad"), &dirty());
    let clean = run(command().arg(tmp.path().join("ok")));
    assert_eq!(clean.status.code(), Some(0));
    assert!(text(&clean.stderr).contains("directories=1 journals=1 violations=0"));
    let both = run(command()
        .arg(tmp.path().join("ok"))
        .arg(tmp.path().join("bad")));
    assert_eq!(both.status.code(), Some(1));
    assert!(text(&both.stderr).contains("directories=2 journals=2 violations=1"));
    insta::assert_debug_snapshot!(brief(&text(&both.stdout)));
}

#[test]
fn a_sweep_runs_the_bench_keeps_its_runs_and_audits_them() {
    let fake = Fake::new();
    let bench = fake.bench(0);
    let output = run(&mut fake.sweep(&bench));
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("bench says hello"), "{stderr}");
    assert!(stderr.contains("scenarios=1 seeds=2 runs=2"), "{stderr}");
    assert!(
        stderr.contains("directories=2 journals=2 violations=3"),
        "{stderr}"
    );
    insta::assert_debug_snapshot!(brief(&text(&output.stdout)));

    let args = fs::read_to_string(fake.path("args.txt")).unwrap();
    let words: Vec<&str> = args.split_whitespace().collect();
    assert_eq!(words[0], "--suite");
    assert_eq!(&words[2..5], ["--split", "dev", "--keep"], "{args}");
    assert_eq!(
        &words[6..],
        ["--out", words[7], "--jobs", "3", "--timeout", "60"]
    );
    let evidence = words[5];
    assert!(Path::new(words[7]).starts_with(evidence));
    assert!(
        fs::read_to_string(Path::new(evidence).join("bench.log"))
            .unwrap()
            .contains("bench says hello")
    );
    let swept = fs::read_to_string(fake.path("suite-copy.json")).unwrap();
    let mut machine = fake.dir.path().to_path_buf();
    machine.push("m.json");
    let swept: serde_json::Value = serde_json::from_str(&swept).unwrap();
    assert_eq!(
        swept,
        serde_json::json!({"scenarios":[{"name":"s","machine":machine,"dev":[1,2]}]})
    );
}

#[test]
fn a_failed_bench_names_the_evidence_and_audits_nothing() {
    let fake = Fake::new();
    let bench = fake.bench(1);
    let output = run(&mut fake.sweep(&bench));
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stdout).is_empty());
    let stderr = text(&output.stderr);
    assert!(stderr.contains("bench sweep (evidence"), "{stderr}");
    assert!(stderr.contains("exit status: 1"), "{stderr}");
}

#[test]
fn an_unsweepable_suite_or_missing_bench_fails_before_running() {
    let fake = Fake::new();
    let bench = fake.bench(0);
    fs::write(fake.path("suite.json"), r#"{"scenarios":[]}"#).unwrap();
    let output = run(&mut fake.sweep(&bench));
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stderr).contains("suite has no scenarios"));
    assert!(!fake.path("args.txt").exists());

    let missing = run(command().arg("--suite").arg(fake.path("suite.json")));
    assert_eq!(missing.status.code(), Some(1));
    assert!(text(&missing.stderr).contains("locate the bench"));
}

#[test]
fn a_retained_sweep_is_audited_again_from_its_records() {
    let fake = Fake::new();
    let bench = fake.bench(0);
    run(&mut fake.sweep(&bench));
    let args = fs::read_to_string(fake.path("args.txt")).unwrap();
    let words: Vec<&str> = args.split_whitespace().collect();
    let root = fs::read_dir(words[5])
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("bench-")
        })
        .unwrap();
    let again = run(command().args(["--records", words[7], "--root"]).arg(&root));
    assert_eq!(again.status.code(), Some(1));
    insta::assert_debug_snapshot!(brief(&text(&again.stdout)));
    let one = run(command()
        .args(["--records", words[7], "--root"])
        .arg(&root)
        .arg(root.join("s/dev-1")));
    assert_eq!(one.status.code(), Some(1), "the status error remains");
    assert!(text(&one.stderr).contains("directories=1 journals=1 violations=1"));
}

#[test]
fn a_sweep_without_keep_leaves_nothing_behind() {
    let fake = Fake::new();
    let scratch = fake.path("tmp");
    fs::create_dir(&scratch).unwrap();
    let sweep = |bench: &Path| {
        let mut command = command();
        command
            .env("TMPDIR", &scratch)
            .arg("--bench")
            .arg(bench)
            .arg("--suite")
            .arg(fake.path("suite.json"))
            .args(["--seeds", "2"]);
        run(&mut command)
    };
    let output = sweep(&fake.bench(0));
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("directories=2 journals=2 violations=3"),
        "{stderr}"
    );
    assert_eq!(fs::read_dir(&scratch).unwrap().count(), 0);

    assert_eq!(sweep(&fake.bench(1)).status.code(), Some(1));
    assert_eq!(fs::read_dir(&scratch).unwrap().count(), 0);
}
