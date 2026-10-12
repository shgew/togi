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
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use fixtures::{applied, header, phase, stop, write_state_dir};
use rustix::process::{Pid, Signal, kill_process, test_kill_process};
use serde_json::{Value, json};
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

/// The line `event` is written as, to edit into one this build does not wholly know.
fn line(event: &Event) -> Value {
    serde_json::from_str(&event.encode().unwrap()).unwrap()
}

/// The session's `session.start` with a field this build does not know, nested in its first core.
fn header_with_unknown_field() -> Value {
    let mut header = line(&header());
    header["data"]["cores"][0]["wild"] = json!(2);
    header
}

fn future_event(seq: u64) -> Value {
    json!({"seq": seq, "time": "2026-01-01T00:00:00Z", "boot": "b", "kind": "future.thing", "msg": "m", "data": {}})
}

fn write_journal(dir: &Path, lines: &[Value]) {
    fs::create_dir_all(dir).unwrap();
    let journal: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(dir.join("events.jsonl"), journal).unwrap();
}

/// What the command says a lenient read of `dir`'s live journal left out of the journals this file writes.
fn skip_report(dir: &Path, kind_line: u64) -> String {
    format!(
        "togi-tool-audit: {:?} live: skipped 1 event of a kind this build does not know (future.thing at line \
         {kind_line}); ignored unknown fields: session.start data.cores[0].wild (line 1)",
        dir.to_string_lossy()
    )
}

#[test]
fn a_given_directory_reports_what_its_lenient_read_left_out_and_is_still_audited() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("bad");
    write_journal(
        &dir,
        &[
            header_with_unknown_field(),
            future_event(2),
            line(&phase(3, 0, Some(-30))),
            line(&applied(4, [-30, 0])),
        ],
    );
    let output = run(command().arg(&dir));
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert_eq!(
        brief(&text(&output.stdout)),
        [
            "- - journal=\"live\" seq=4 \"avoidance\": \"core 0 offset -30 reaches failure point -30 recorded at seq 3 \
             (tuner.md: P[c] <= fail[c])\""
        ]
    );
    assert!(
        stderr.lines().any(|line| line == skip_report(&dir, 2)),
        "{stderr}"
    );
    assert!(
        stderr.contains("directories=1 journals=1 violations=1"),
        "{stderr}"
    );
}

#[test]
fn a_recorded_run_reports_what_its_lenient_read_left_out_and_is_still_audited() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("bench-1");
    let dir = root.join("s/dev-1");
    write_state_dir(&dir, &[header(), stop(2)]);
    write_journal(
        &dir,
        &[header_with_unknown_field(), line(&stop(2)), future_event(3)],
    );
    let records = tmp.path().join("records.jsonl");
    fs::write(
        &records,
        r#"{"scenario":"s","seed":1,"split":"dev","status":"concluded","wall_s":1.0}"#,
    )
    .unwrap();
    let output = run(command()
        .arg("--records")
        .arg(&records)
        .arg("--root")
        .arg(&root));
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert!(text(&output.stdout).is_empty());
    assert!(
        stderr.lines().any(|line| line == skip_report(&dir, 3)),
        "{stderr}"
    );
    assert!(
        stderr.contains("directories=1 journals=1 violations=0"),
        "{stderr}"
    );
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
    let kept = format!(
        "bench sweep (evidence {}/sweep-",
        fake.path("evidence").display()
    );
    assert!(stderr.contains(&kept), "{stderr}");
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

    let lonely = fake.path("lonely");
    fs::copy(env!("CARGO_BIN_EXE_togi-tool-audit"), &lonely).unwrap();
    let missing = run(Command::new(&lonely)
        .arg("--suite")
        .arg(fake.path("suite.json")));
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

    let failed = sweep(&fake.bench(1));
    assert_eq!(failed.status.code(), Some(1));
    let stderr = text(&failed.stderr);
    assert!(
        stderr.contains("bench sweep (evidence removed; rerun with --keep DIR to retain it)"),
        "{stderr}"
    );
    let failure = stderr
        .lines()
        .find(|line| line.contains("bench sweep"))
        .unwrap();
    assert!(!failure.contains("togi-audit-"), "{failure}");
    assert_eq!(fs::read_dir(&scratch).unwrap().count(), 0);
}

#[test]
fn a_sweep_passes_the_simulator_to_the_bench() {
    let fake = Fake::new();
    let bench = fake.bench(0);
    run(fake.sweep(&bench).args(["--sim", "/fake/sim"]));
    let args = fs::read_to_string(fake.path("args.txt")).unwrap();
    assert!(args.trim_end().ends_with("--sim /fake/sim"), "{args}");
}

#[test]
fn a_missing_suite_file_fails_before_running_the_bench() {
    let fake = Fake::new();
    let bench = fake.bench(0);
    let output = run(command()
        .arg("--bench")
        .arg(&bench)
        .arg("--suite")
        .arg(fake.path("absent.json")));
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stdout).is_empty());
    assert!(text(&output.stderr).contains("read suite"));
    assert!(!fake.path("args.txt").exists());
}

#[test]
fn missing_run_records_fail_with_the_open_error() {
    let fake = Fake::new();
    let output = run(command()
        .arg("--records")
        .arg(fake.path("absent.jsonl"))
        .arg("--root")
        .arg(fake.path("root")));
    assert_eq!(output.status.code(), Some(1));
    assert!(text(&output.stdout).is_empty());
    let stderr = text(&output.stderr);
    assert!(stderr.contains("open run records"), "{stderr}");
    assert!(stderr.contains("absent.jsonl"), "{stderr}");
}

#[test]
fn a_run_directory_is_found_whatever_dots_its_root_is_spelled_with() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("bench-1");
    let work = tmp.path().join("work");
    fs::create_dir(&work).unwrap();
    write_state_dir(&root.join("s/dev-2"), &[header(), phase(2, 0, None)]);
    fs::write(
        tmp.path().join("records.jsonl"),
        r#"{"scenario":"s","seed":2,"split":"dev","status":"concluded","wall_s":1.0}"#,
    )
    .unwrap();
    let output = run(command()
        .current_dir(&work)
        .args(["--records", "../records.jsonl", "--root", "../bench-1"])
        .arg(root.join("s/dev-2")));
    assert_eq!(output.status.code(), Some(1));
    let found = brief(&text(&output.stdout));
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].starts_with("\"s\" 2 journal=\"live\" seq=2 \"termination\": "),
        "{found:?}"
    );
}

fn wait_for(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn exit_of(child: &mut Child) -> Output {
    wait_for("the audit to exit", || child.try_wait().unwrap().is_some());
    let mut stderr = String::new();
    std::io::Read::read_to_string(child.stderr.as_mut().unwrap(), &mut stderr).unwrap();
    Output {
        status: child.try_wait().unwrap().unwrap(),
        stdout: Vec::new(),
        stderr: stderr.into_bytes(),
    }
}

#[test]
fn real_process_a_signal_stops_the_bench_and_removes_the_scratch_without_blocking_its_signals() {
    for (signal, keep) in [
        (Signal::TERM, false),
        (Signal::INT, false),
        (Signal::HUP, false),
        (Signal::TERM, true),
    ] {
        let fake = Fake::new();
        let scratch = fake.path("tmp");
        fs::create_dir(&scratch).unwrap();
        let pids = fake.path("pids.txt");
        let bench = fake.path("bench");
        fs::write(
            &bench,
            format!(
                "#!/bin/sh\nif [ -r /proc/$$/status ]; then while read -r line; do case $line in SigBlk*) echo \"$line\";; esac; done < /proc/$$/status > '{sigblk}'; fi\necho $$ >> '{pids}'\nexec sleep 60\n",
                sigblk = fake.path("sigblk").display(),
                pids = pids.display(),
            ),
        )
        .unwrap();
        fs::set_permissions(&bench, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            fake.path("suite.json"),
            r#"{"scenarios":[{"name":"s","machine":"m.json"}]}"#,
        )
        .unwrap();
        let mut audit = command();
        audit
            .env("TMPDIR", &scratch)
            .arg("--bench")
            .arg(&bench)
            .arg("--suite")
            .arg(fake.path("suite.json"))
            .args(["--seeds", "2"])
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if keep {
            audit.arg("--keep").arg(fake.path("evidence"));
        }
        let mut child = audit.spawn().unwrap();
        wait_for("the bench to start", || {
            fs::read_to_string(&pids).is_ok_and(|started| started.ends_with('\n'))
        });
        kill_process(
            Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap(),
            signal,
        )
        .unwrap();
        let output = exit_of(&mut child);
        let stderr = text(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{signal:?}: {stderr}");
        assert!(stderr.contains("interrupted"), "{stderr}");
        let pid =
            Pid::from_raw(fs::read_to_string(&pids).unwrap().trim().parse().unwrap()).unwrap();
        assert!(
            test_kill_process(pid).is_err(),
            "the bench is still running"
        );
        if cfg!(target_os = "linux") {
            let blocked = fs::read_to_string(fake.path("sigblk")).unwrap();
            assert_eq!(blocked.trim(), "SigBlk:\t0000000000000000");
        }
        assert_eq!(fs::read_dir(&scratch).unwrap().count(), 0, "{signal:?}");
        assert_eq!(
            fake.path("evidence").exists(),
            keep,
            "the kept evidence stays: {signal:?}"
        );
    }
}

#[test]
fn real_process_a_signal_while_the_audit_reads_the_evidence_removes_the_scratch_and_exits() {
    use std::os::unix::fs::FileTypeExt as _;

    for (signal, keep) in [
        (Signal::TERM, false),
        (Signal::HUP, false),
        (Signal::INT, false),
        (Signal::TERM, true),
    ] {
        let fake = Fake::new();
        let scratch = fake.path("tmp");
        fs::create_dir(&scratch).unwrap();
        let bench = fake.path("bench");
        fs::write(
            &bench,
            "#!/bin/sh\nwhile [ $# -gt 0 ]; do case \"$1\" in --keep) keep=\"$2\";; --out) out=\"$2\";; esac; shift; done\nmkdir -p \"$keep/bench-1\"\nmkfifo \"$out\"\n",
        )
        .unwrap();
        fs::set_permissions(&bench, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(
            fake.path("suite.json"),
            r#"{"scenarios":[{"name":"s","machine":"m.json"}]}"#,
        )
        .unwrap();
        let evidence = fake.path("evidence");
        let mut audit = command();
        audit
            .env("TMPDIR", &scratch)
            .arg("--bench")
            .arg(&bench)
            .arg("--suite")
            .arg(fake.path("suite.json"))
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if keep {
            audit.arg("--keep").arg(&evidence);
        }
        let mut child = audit.spawn().unwrap();
        let base = if keep { &evidence } else { &scratch };
        let mut pipe = None;
        wait_for("the bench to make the records pipe", || {
            pipe = fs::read_dir(base)
                .ok()
                .and_then(|mut entries| entries.next())
                .map(|entry| entry.unwrap().path().join("records.jsonl"))
                .filter(|path| {
                    fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_fifo())
                });
            pipe.is_some()
        });
        let pipe = pipe.unwrap();
        let writer = std::thread::spawn(move || fs::OpenOptions::new().write(true).open(pipe));
        wait_for("the audit to open the records", || writer.is_finished());
        let held = writer.join().unwrap().unwrap();
        kill_process(
            Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap(),
            signal,
        )
        .unwrap();
        let output = exit_of(&mut child);
        drop(held);
        let stderr = text(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{signal:?}: {stderr}");
        assert!(stderr.contains("interrupted"), "{stderr}");
        assert!(!stderr.contains("violations="), "{stderr}");
        assert_eq!(
            fs::read_dir(&scratch).unwrap().count(),
            0,
            "{signal:?} keep={keep}"
        );
        assert_eq!(evidence.exists(), keep, "{signal:?} keep={keep}");
    }
}

const ARGS: &str = "while [ $# -gt 0 ]; do case \"$1\" in --keep) keep=\"$2\";; --out) out=\"$2\";; esac; shift; done\n";

fn scripted_bench(fake: &Fake, body: &str) -> PathBuf {
    let bench = fake.path("bench");
    fs::write(&bench, format!("#!/bin/sh\n{ARGS}{body}")).unwrap();
    fs::set_permissions(&bench, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        fake.path("suite.json"),
        r#"{"scenarios":[{"name":"s","machine":"m.json"}]}"#,
    )
    .unwrap();
    bench
}

fn sweep_in(fake: &Fake, bench: &Path, keep: bool) -> Command {
    let mut audit = command();
    audit
        .env("TMPDIR", fake.path("tmp"))
        .arg("--bench")
        .arg(bench)
        .arg("--suite")
        .arg(fake.path("suite.json"))
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if keep {
        audit.arg("--keep").arg(fake.path("evidence"));
    }
    fs::create_dir_all(fake.path("tmp")).unwrap();
    audit
}

fn signal(child: &Child, signal: Signal) {
    kill_process(
        Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap(),
        signal,
    )
    .unwrap();
}

#[test]
fn real_process_a_signal_before_the_bench_starts_stops_it_as_soon_as_it_does() {
    let fake = Fake::new();
    let pids = fake.path("pids.txt");
    let bench = scripted_bench(
        &fake,
        &format!("echo $$ > '{}'\nexec sleep 60\n", pids.display()),
    );
    fs::remove_file(fake.path("suite.json")).unwrap();
    let made = Command::new("mkfifo")
        .arg(fake.path("suite.json"))
        .status()
        .unwrap();
    assert!(made.success());
    let mut child = sweep_in(&fake, &bench, false).spawn().unwrap();
    let suite = fake.path("suite.json");
    let writer = std::thread::spawn(move || fs::OpenOptions::new().write(true).open(suite));
    wait_for("the audit to open the suite", || writer.is_finished());
    let mut held = writer.join().unwrap().unwrap();
    signal(&child, Signal::TERM);
    std::thread::sleep(Duration::from_millis(300));
    std::io::Write::write_all(
        &mut held,
        br#"{"scenarios":[{"name":"s","machine":"m.json"}]}"#,
    )
    .unwrap();
    drop(held);
    let output = exit_of(&mut child);
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("interrupted"), "{stderr}");
    if let Some(started) = fs::read_to_string(&pids)
        .ok()
        .and_then(|started| started.trim().parse().ok())
    {
        let pid = Pid::from_raw(started).unwrap();
        assert!(
            test_kill_process(pid).is_err(),
            "the bench is still running"
        );
    }
    assert_eq!(fs::read_dir(fake.path("tmp")).unwrap().count(), 0);
}

#[test]
fn real_process_an_interrupted_sweep_is_interrupted_even_when_the_bench_exits_cleanly() {
    let fake = Fake::new();
    let ready = fake.path("ready");
    let bench = scripted_bench(
        &fake,
        &format!(
            "sleep 60 &\nchild=$!\ntrap 'kill $child; mkdir -p \"$keep/bench-1\"; : > \"$out\"; exit 0' TERM\necho ready > '{}'\nwait $child\n",
            ready.display()
        ),
    );
    let mut child = sweep_in(&fake, &bench, false).spawn().unwrap();
    wait_for("the bench to trap TERM", || ready.exists());
    signal(&child, Signal::TERM);
    let output = exit_of(&mut child);
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("interrupted"), "{stderr}");
    assert!(!stderr.contains("violations="), "{stderr}");
    assert_eq!(fs::read_dir(fake.path("tmp")).unwrap().count(), 0);
}

#[test]
fn a_bench_that_keeps_no_run_root_fails_the_sweep() {
    let fake = Fake::new();
    let bench = scripted_bench(&fake, ": > \"$out\"\n");
    let output = run(&mut sweep_in(&fake, &bench, false));
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    let failure = stderr
        .lines()
        .find(|line| line.contains("find the retained bench root"))
        .unwrap_or_else(|| panic!("{stderr}"));
    assert!(
        failure.ends_with(
            "find the retained bench root (evidence removed; rerun with --keep DIR to retain it): got 0 roots"
        ),
        "{failure}"
    );
    assert!(!failure.contains("togi-audit-"), "{failure}");
    assert_eq!(fs::read_dir(fake.path("tmp")).unwrap().count(), 0);

    let kept = run(&mut sweep_in(&fake, &bench, true));
    let stderr = text(&kept.stderr);
    assert_eq!(kept.status.code(), Some(1), "{stderr}");
    let want = format!(
        "find the retained bench root (evidence {}/sweep-",
        fake.path("evidence").display()
    );
    assert!(stderr.contains(&want), "{stderr}");
    assert!(stderr.contains("got 0 roots"), "{stderr}");
}
