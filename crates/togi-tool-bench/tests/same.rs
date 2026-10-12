//! `same` over fake simulators: scripts that leave a recorded journal and exit as the test says.
#![expect(
    clippy::unwrap_used,
    clippy::disallowed_methods,
    reason = "the tests run the built command and panic on a broken fixture"
)]

mod session;

use std::fs;
use std::num::{NonZeroU32, NonZeroUsize};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use togi_sim::MachineDefinition;
use togi_tool_bench::Fleet;
use togi_tool_bench::same::{Options, SameError, execute};

struct Trees {
    dir: tempfile::TempDir,
}

fn recorded(replace: Option<(&str, &str)>) -> String {
    let events = session::session(&MachineDefinition::default(), 1, None, 2).events;
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

    /// A simulator that records its arguments, leaves `journal` in its state directory and exits with `exit`.
    fn simulator(&self, name: &str, journal: &str, exit: i32) -> PathBuf {
        let journal_path = self.path(&format!("{name}.jsonl"));
        fs::write(&journal_path, journal).unwrap();
        let path = self.path(name);
        let script = format!(
            r#"#!/bin/sh
echo "$@" >> '{args}'
dir=
while [ $# -gt 0 ]; do
  if [ "$1" = --state-dir ]; then dir="$2"; fi
  shift
done
cp '{journal}' "$dir/events.jsonl"
exit {exit}
"#,
            args = self.path(&format!("{name}.args")).display(),
            journal = journal_path.display(),
        );
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn launches(&self, name: &str) -> usize {
        fs::read_to_string(self.path(&format!("{name}.args")))
            .map_or(0, |args| args.lines().count())
    }

    fn options(&self, base: PathBuf, head: PathBuf) -> Options {
        Options {
            base: self.path("base"),
            head: self.path("head"),
            suite: PathBuf::from("suite.json"),
            smoke: false,
            keep_going: false,
            no_cache: false,
            keep: None,
            jobs: NonZeroUsize::new(2).unwrap(),
            timeout: Duration::from_secs(60),
            max_boots: NonZeroU32::new(7).unwrap(),
            cache: self.path("cache"),
            simulators: [Some(base), Some(head)],
        }
    }
}

const SUITE: &str = r#"{"scenarios":[{"name":"default","dev":[1,2],"holdout":[101],"smoke":[2]},{"name":"late","dev":[1],"smoke":[1]}]}"#;

/// The informational lines about simulator time, which vary with the machine's load.
fn is_timing(line: &str) -> bool {
    line.starts_with("same: simulator time")
        || line.contains("took more than")
        || (line.starts_with("  ") && line.contains(": head ") && line.contains(", base "))
        || line.starts_with("  and ")
}

fn report(report: &togi_tool_bench::same::Report) -> String {
    let mut out = Vec::new();
    report.write(&mut out).unwrap();
    let text = String::from_utf8(out).unwrap();
    text.lines()
        .filter(|line| !is_timing(line))
        .flat_map(|line| [line, "\n"])
        .collect()
}

#[test]
fn two_trees_that_build_the_same_simulator_from_the_same_inputs_run_nothing() {
    let trees = Trees::new(SUITE, SUITE);
    let sim = trees.simulator("sim", &recorded(None), 0);
    let options = trees.options(sim.clone(), sim);
    let done = execute(&options, &Fleet::new()).unwrap();
    assert!(done.identical && !done.different());
    assert_eq!(
        report(&done),
        "same: both trees build the same simulator and read the same inputs; 0 of 4 sessions differ\n"
    );
    assert_eq!(trees.launches("sim"), 0);
}

#[test]
fn identical_simulators_still_run_when_asked_to_keep_or_ignore_the_cache() {
    for (no_cache, keep) in [(true, false), (false, true)] {
        let trees = Trees::new(SUITE, SUITE);
        let sim = trees.simulator("sim", &recorded(None), 0);
        let mut options = trees.options(sim.clone(), sim);
        options.no_cache = no_cache;
        options.keep = keep.then(|| trees.path("kept"));
        let done = execute(&options, &Fleet::new()).unwrap();
        assert!(!done.identical && !done.different());
        assert_eq!(trees.launches("sim"), 8, "no_cache={no_cache} keep={keep}");
        assert_eq!(done.kept.is_some(), keep);
    }
}

#[test]
fn a_build_stamp_alone_is_not_a_difference_and_a_decision_is() {
    let trees = Trees::new(SUITE, SUITE);
    let journal = recorded(None);
    let base = trees.simulator("base-sim", &journal, 0);
    let head = trees.simulator("head-sim", &stamped_by_another_build(&journal), 0);
    let done = execute(&trees.options(base, head), &Fleet::new()).unwrap();
    assert!(!done.identical && !done.different());
    assert_eq!(
        report(&done).lines().last(),
        Some("same: 0 of 4 sessions differ")
    );
    assert_eq!(
        (trees.launches("base-sim"), trees.launches("head-sim")),
        (4, 4)
    );

    let other = Trees::new(SUITE, SUITE);
    let base = other.simulator("base-sim", &journal, 0);
    let decided = journal.replacen("\"seq\":3,", "\"seq\":3000,", 1);
    assert_ne!(decided, journal);
    let head = other.simulator("head-sim", &decided, 0);
    let mut options = other.options(base, head);
    options.jobs = NonZeroUsize::MIN;
    let done = execute(&options, &Fleet::new()).unwrap();
    assert!(done.different());
    let text = report(&done);
    assert!(
        text.starts_with("same: default/dev-1\n  events.jsonl:3\n"),
        "{text}"
    );
    assert!(text.ends_with(
        "same: stopped at the first difference; --keep-going reports every differing session\n"
    ));
    let kept = done.kept.unwrap();
    assert!(kept.join("base/default/dev-1/events.jsonl").is_file());
    assert!(kept.join("head/default/dev-1/events.jsonl").is_file());
}

#[test]
fn a_second_comparison_of_the_same_binaries_runs_nothing_again() {
    let trees = Trees::new(SUITE, SUITE);
    let journal = recorded(None);
    let base = trees.simulator("base-sim", &journal, 0);
    let head = trees.simulator("head-sim", &stamped_by_another_build(&journal), 0);
    let options = trees.options(base, head);
    execute(&options, &Fleet::new()).unwrap();
    let done = execute(&options, &Fleet::new()).unwrap();
    assert_eq!(done.outcome.cached, [4, 4]);
    assert_eq!(done.outcome.scheduled, [0, 0]);
    assert_eq!(
        (trees.launches("base-sim"), trees.launches("head-sim")),
        (4, 4)
    );
}

#[test]
fn a_session_one_suite_lacks_is_a_difference() {
    let head_suite =
        r#"{"scenarios":[{"name":"default","dev":[1,2],"holdout":[101],"smoke":[2]}]}"#;
    let trees = Trees::new(SUITE, head_suite);
    let journal = recorded(None);
    let base = trees.simulator("base-sim", &journal, 0);
    let head = trees.simulator("head-sim", &stamped_by_another_build(&journal), 0);
    let mut options = trees.options(base, head);
    options.keep_going = true;
    let done = execute(&options, &Fleet::new()).unwrap();
    assert_eq!(
        report(&done),
        "same: late/dev-1\n  <session>\n  base: <present>\n  head: <missing>\nsame: 1 of 4 sessions differ\n"
    );
}

#[test]
fn smoke_compares_only_the_sessions_the_head_suite_lists() {
    let trees = Trees::new(SUITE, SUITE);
    let journal = recorded(None);
    let base = trees.simulator("base-sim", &journal, 0);
    let head = trees.simulator("head-sim", &stamped_by_another_build(&journal), 0);
    let mut options = trees.options(base, head);
    options.smoke = true;
    let done = execute(&options, &Fleet::new()).unwrap();
    assert_eq!(
        report(&done).lines().last(),
        Some("same: 0 of 2 sessions differ")
    );
    assert_eq!(trees.launches("head-sim"), 2);
}

#[test]
fn smoke_without_smoke_sessions_is_an_error() {
    let suite = r#"{"scenarios":[{"name":"default","dev":[1]}]}"#;
    let trees = Trees::new(suite, suite);
    let sim = trees.simulator("sim", &recorded(None), 0);
    let mut options = trees.options(sim.clone(), sim);
    options.smoke = true;
    let failed = execute(&options, &Fleet::new()).unwrap_err();
    assert!(matches!(*failed.error, SameError::NoSmoke), "{failed}");
}

#[test]
fn a_tree_whose_suite_cannot_be_read_is_an_error_naming_it() {
    let trees = Trees::new(SUITE, "not json");
    let sim = trees.simulator("sim", &recorded(None), 0);
    let failed = execute(&trees.options(sim.clone(), sim), &Fleet::new()).unwrap_err();
    let message = failed.to_string();
    assert!(message.starts_with("load the head suite in "), "{message}");
}

#[test]
fn a_simulator_that_leaves_no_journal_fails_the_comparison_and_keeps_the_runs() {
    let trees = Trees::new(SUITE, SUITE);
    let base = trees.simulator("base-sim", &recorded(None), 0);
    let head = trees.simulator("head-sim", "", 1);
    let mut options = trees.options(base, head);
    options.jobs = NonZeroUsize::MIN;
    let failed = execute(&options, &Fleet::new()).unwrap_err();
    assert!(
        failed.to_string().contains("journals contain no events"),
        "{failed}"
    );
    let kept = failed.kept.unwrap();
    assert!(kept.is_dir());
}

/// A tree that holds only a `togi-tool-sim` package whose simulator writes `decision` as the whole journal.
fn buildable_tree(dir: &Path, suite: &str, decision: u32) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("suite.json"), suite).unwrap();
    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"togi-tool-sim\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("Cargo.lock"),
        "version = 4\n\n[[package]]\nname = \"togi-tool-sim\"\nversion = \"0.0.0\"\n",
    )
    .unwrap();
    fs::write(
        dir.join("src/main.rs"),
        format!(
            "fn main() {{\n    let mut args = std::env::args();\n    let mut dir = String::new();\n    while let Some(arg) = args.next() {{\n        if arg == \"--state-dir\" {{\n            dir = args.next().unwrap();\n        }}\n    }}\n    std::fs::write(format!(\"{{dir}}/events.jsonl\"), \"{{\\\"decision\\\":{decision}}}\\n\").unwrap();\n}}\n"
        ),
    )
    .unwrap();
}

#[test]
fn each_tree_is_built_and_its_own_simulator_run() {
    let dir = tempfile::tempdir().unwrap();
    let suite = r#"{"scenarios":[{"name":"s","dev":[1,2]}]}"#;
    for (name, decision) in [("base", 0), ("same", 0), ("head", 1)] {
        buildable_tree(&dir.path().join(name), suite, decision);
    }
    let options = |base: &str, head: &str| Options {
        base: dir.path().join(base),
        head: dir.path().join(head),
        suite: PathBuf::from("suite.json"),
        smoke: false,
        keep_going: true,
        no_cache: false,
        keep: None,
        jobs: NonZeroUsize::new(2).unwrap(),
        timeout: Duration::from_secs(60),
        max_boots: NonZeroU32::new(7).unwrap(),
        cache: dir.path().join("cache"),
        simulators: [None, None],
    };
    let done = execute(&options("base", "head"), &Fleet::new()).unwrap();
    assert!(done.different());
    let text = report(&done);
    assert!(
        text.contains(
            "same: s/dev-1\n  events.jsonl:1\n  base: {\"decision\":0}\n  head: {\"decision\":1}\n"
        ),
        "{text}"
    );
    assert!(text.ends_with("same: 2 of 2 sessions differ\n"), "{text}");
    let done = execute(&options("base", "same"), &Fleet::new()).unwrap();
    assert!(!done.different());
    assert!(
        done.identical,
        "the same source built in another directory is the same simulator"
    );
}

fn command(trees: &Trees) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_togi-tool-bench"));
    command
        .arg("same")
        .arg(trees.path("base"))
        .arg("--head")
        .arg(trees.path("head"))
        .arg("--suite")
        .arg("suite.json")
        .arg("--cache")
        .arg(trees.path("cache"));
    command
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[test]
fn the_command_exits_zero_for_equal_trees_and_one_for_a_difference() {
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

#[test]
fn bench_flags_that_judge_a_run_are_usage_errors_with_same() {
    for flag in ["--split=dev", "--baseline=b.jsonl", "--out=o.jsonl"] {
        let output = Command::new(env!("CARGO_BIN_EXE_togi-tool-bench"))
            .args(["same", ".", flag])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{flag}");
        assert!(output.stdout.is_empty(), "{flag}");
    }
}

#[test]
fn a_missing_base_argument_is_a_usage_error() {
    let output = Command::new(env!("CARGO_BIN_EXE_togi-tool-bench"))
        .arg("same")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}
