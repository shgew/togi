//! `same` over fake simulators: scripts that leave a recorded journal and exit as the test says.
#![expect(clippy::disallowed_methods, reason = "the tests run the built command")]

use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::session;

struct Trees {
    dir: tempfile::TempDir,
}

fn recorded(replace: Option<(&str, &str)>) -> String {
    let events = session::unfinished();
    let text: Vec<String> = events
        .iter()
        .map(|event| serde_json::to_string(event).unwrap())
        .collect();
    let text = text.join("\n") + "\n";
    match replace {
        Some((from, to)) => text.replace(from, to),
        None => text,
    }
}

fn stamped_by_another_build(journal: &str) -> String {
    journal
        .replace("togi 0.0.0", "togi 9.9.9+abc")
        .replace(r#""version":"0.0.0""#, r#""version":"9.9.9","rev":"abc""#)
}

impl Trees {
    fn new(base_suite: &str, head_suite: &str) -> Self {
        let trees = Self {
            dir: tempfile::tempdir().unwrap(),
        };
        for (tree, suite) in [("base", base_suite), ("head", head_suite)] {
            fs::create_dir_all(trees.path(tree)).unwrap();
            fs::write(trees.path(tree).join("suite.json"), suite).unwrap();
        }
        trees
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// A simulator that leaves `journal` in its state directory and exits with `exit`.
    fn simulator(&self, name: &str, journal: &str, exit: i32) -> PathBuf {
        let journal_path = self.path(&format!("{name}.jsonl"));
        fs::write(&journal_path, journal).unwrap();
        let path = self.path(name);
        let script = format!(
            r#"#!/bin/sh
dir=
while [ $# -gt 0 ]; do
  if [ "$1" = --state-dir ]; then dir="$2"; fi
  shift
done
cp '{journal}' "$dir/events.jsonl"
exit {exit}
"#,
            journal = journal_path.display(),
        );
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }
}

const SUITE: &str = r#"{"scenarios":[{"name":"default","dev":[1,2],"holdout":[101],"smoke":[2]},{"name":"late","dev":[1],"smoke":[1]}]}"#;

#[test]
fn real_process_each_tree_is_built_and_its_own_simulator_run() {
    let suite = r#"{"scenarios":[{"name":"s","dev":[1,2]}]}"#;
    let trees = Trees::new(suite, suite);
    fs::create_dir_all(trees.path("same")).unwrap();
    fs::write(trees.path("same/suite.json"), suite).unwrap();
    for (name, decision) in [("base", 0), ("same", 0), ("head", 1)] {
        trees.simulator(
            &format!("{name}/simulator"),
            &format!("{{\"decision\":{decision}}}\n"),
            0,
        );
    }
    // Inject compiler results rather than invoking cargo/rustc: compiler output publication may sync.
    let cargo = trees.path("cargo");
    fs::write(&cargo, format!(
        "#!/bin/sh\necho \"$PWD $@\" >> '{log}'\nprintf '{{\"reason\":\"compiler-artifact\",\"target\":{{\"name\":\"togi-tool-sim\",\"kind\":[\"bin\"]}},\"executable\":\"%s/simulator\"}}\\n' \"$PWD\"\n",
        log = trees.path("cargo.log").display(),
    )).unwrap();
    fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
    let run = |head: &str| {
        let output = Command::new(env!("CARGO_BIN_EXE_togi-tool-bench"))
            .arg("same")
            .arg(trees.path("base"))
            .arg("--head")
            .arg(trees.path(head))
            .args(["--suite", "suite.json", "--keep-going"])
            .arg("--cache")
            .arg(trees.path("cache"))
            .env("CARGO", &cargo)
            .output()
            .unwrap();
        (output.status, text(&output.stdout), text(&output.stderr))
    };
    let (status, text, stderr) = run("head");
    assert_eq!(status.code(), Some(1), "{stderr}");
    assert!(
        text.contains(
            "same: s/dev-1\n  events.jsonl:1\n  base: {\"decision\":0}\n  head: {\"decision\":1}\n"
        ),
        "{text}"
    );
    assert!(text.ends_with("same: 2 of 2 sessions differ\n"), "{text}");
    let (status, text, stderr) = run("same");
    assert!(status.success(), "{stderr}");
    assert!(text.ends_with("same: 0 of 2 sessions differ\n"), "{text}");
    assert_eq!(
        cargo_runs(&trees).len(),
        4,
        "each tree gets its own build result"
    );
}

fn command(trees: &Trees) -> Command {
    command_with_cache(trees, &trees.path("cache"))
}

fn command_with_cache(trees: &Trees, cache: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_togi-tool-bench"));
    command
        .arg("same")
        .arg(trees.path("base"))
        .arg("--head")
        .arg(trees.path("head"))
        .arg("--suite")
        .arg("suite.json")
        .arg("--cache")
        .arg(cache);
    command
}

/// A `cargo` that logs its directory and arguments, reports `simulator` as the binary it built, then runs `after`.
fn fake_cargo(trees: &Trees, simulator: &Path, after: &str) -> PathBuf {
    let path = trees.path("cargo");
    let script = format!(
        "#!/bin/sh\necho \"$PWD $@\" >> '{log}'\nprintf '%s\\n' '{{\"reason\":\"compiler-artifact\",\"target\":{{\"name\":\"togi-tool-sim\",\"kind\":[\"bin\"]}},\"executable\":\"{simulator}\"}}'\n{after}\n",
        log = trees.path("cargo.log").display(),
        simulator = simulator.display(),
    );
    fs::write(&path, script).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn cargo_runs(trees: &Trees) -> Vec<String> {
    fs::read_to_string(trees.path("cargo.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[test]
fn real_process_the_command_exits_zero_for_equal_trees_and_one_for_a_difference() {
    let trees = Trees::new(SUITE, SUITE);
    let journal = recorded(None);
    let base = trees.simulator("base-sim", &journal, 0);
    let head = trees.simulator("head-sim", &stamped_by_another_build(&journal), 0);
    let output = command(&trees)
        .arg("--base-sim")
        .arg(&base)
        .arg("--head-sim")
        .arg(&head)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    assert!(text(&output.stdout).ends_with("same: 0 of 4 sessions differ\n"));

    let changed = trees.simulator(
        "changed-sim",
        &journal.replacen("\"seq\":3,", "\"seq\":3000,", 1),
        0,
    );
    let output = command(&trees)
        .arg("--base-sim")
        .arg(&base)
        .arg("--head-sim")
        .arg(&changed)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{}", text(&output.stderr));
    let stdout = text(&output.stdout);
    assert!(
        stdout.contains("  events.jsonl:3\n"),
        "{stdout}{}",
        text(&output.stderr)
    );
    assert!(text(&output.stderr).contains("kept the runs in"));
}

#[test_case::test_case("--split=dev"; "split dev")]
#[test_case::test_case("--baseline=b.jsonl"; "baseline b jsonl")]
#[test_case::test_case("--out=o.jsonl"; "out o jsonl")]
fn real_process_bench_flags_that_judge_a_run_are_usage_errors_with_same(case: &str) {
    let flag = case;

    let output = Command::new(env!("CARGO_BIN_EXE_togi-tool-bench"))
        .args(["same", ".", flag])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{flag}");
    assert!(output.stdout.is_empty(), "{flag}");
}

#[test]
fn real_process_a_missing_base_argument_is_a_usage_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_togi-tool-bench"))
        .arg("same")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test_case::test_case(false; "false")]
#[test_case::test_case(true; "true")]
fn real_process_a_full_run_leaves_no_run_directory_and_no_copy_of_a_simulator_behind(case: bool) {
    let smoke = case;
    let trees = Trees::new(SUITE, SUITE);
    let journal = recorded(None);
    let base = trees.simulator("base-sim", &journal, 0);
    let head = trees.simulator("head-sim", &stamped_by_another_build(&journal), 0);
    let temporary = trees.path("tmp");
    fs::create_dir_all(&temporary).unwrap();

    let mut run = command(&trees);
    run.env("TMPDIR", &temporary)
        .arg("--no-cache")
        .arg("--base-sim")
        .arg(&base)
        .arg("--head-sim")
        .arg(&head);
    if smoke {
        run.arg("--smoke");
    }
    let output = run.output().unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    let left: Vec<_> = fs::read_dir(&temporary)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert!(left.is_empty(), "smoke={smoke}: {left:?}");
    assert!(!trees.path("cache/runs").exists(), "smoke={smoke}");
}

#[test]
fn real_process_a_termination_during_the_build_fails_the_command_instead_of_reporting_equality() {
    let trees = Trees::new(SUITE, SUITE);
    let sim = trees.simulator("sim", &recorded(None), 0);
    let cargo = fake_cargo(&trees, &sim, "kill -TERM $PPID\nsleep 60");
    let temporary = trees.path("tmp");
    fs::create_dir_all(&temporary).unwrap();
    let output = command(&trees)
        .env("CARGO", &cargo)
        .env("TMPDIR", &temporary)
        .output()
        .unwrap();
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("interrupted"), "{stderr}");
    assert!(output.stdout.is_empty(), "{}", text(&output.stdout));
    assert_eq!(
        cargo_runs(&trees).len(),
        1,
        "the head built after the interrupt"
    );
    let left: Vec<_> = fs::read_dir(&temporary).unwrap().collect();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn real_process_a_relative_cache_directory_holds_the_base_target_directory_of_the_bench_not_of_the_tree()
 {
    let trees = Trees::new(SUITE, SUITE);
    let sim = trees.simulator("sim", &recorded(None), 0);
    let cargo = fake_cargo(&trees, &sim, "");
    let output = command_with_cache(&trees, Path::new("relative-cache"))
        .current_dir(trees.dir.path())
        .env("CARGO", &cargo)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", text(&output.stderr));
    let target = trees
        .dir
        .path()
        .canonicalize()
        .unwrap()
        .join("relative-cache/same-target");
    let runs = cargo_runs(&trees);
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert!(
        runs[0].contains(&format!("--target-dir {}", target.display())),
        "{runs:?}"
    );
}

#[test]
fn real_process_a_failed_session_exits_one_and_names_the_only_directory_it_left_in_the_temporary_directory()
 {
    let trees = Trees::new(SUITE, SUITE);
    let base = trees.simulator("base-sim", &recorded(None), 0);
    let head = trees.simulator("head-sim", "", 1);
    let temporary = trees.path("tmp");
    fs::create_dir_all(&temporary).unwrap();
    let output = command(&trees)
        .env("TMPDIR", &temporary)
        .arg("--base-sim")
        .arg(&base)
        .arg("--head-sim")
        .arg(&head)
        .output()
        .unwrap();
    let stderr = text(&output.stderr);
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(output.stdout.is_empty(), "{}", text(&output.stdout));
    let kept = stderr
        .lines()
        .find_map(|line| line.strip_prefix("togi-tool-bench: kept the runs in "))
        .unwrap_or_else(|| panic!("no kept-runs line: {stderr}"));
    let kept = PathBuf::from(kept);
    assert!(kept.is_dir(), "{kept:?}");
    let left: Vec<_> = fs::read_dir(&temporary)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(left, [kept]);
}
