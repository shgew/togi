#![allow(dead_code, reason = "each test crate uses part of this shared module")]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use togi_machine::PerCore;
use togi_tool_bench::{Record, Split, Status};

/// A concluded dev run of `scenario` with every measurement zero but its ten simulated hours.
pub(crate) fn record(scenario: &str, seed: u64) -> Record {
    Record {
        scenario: scenario.to_owned(),
        machine: None,
        seed,
        split: Split::Dev,
        commit: "abc1234".to_owned(),
        dirty: false,
        ruleset: 1,
        evidence: 1,
        status: Status::Concluded,
        model_check: None,
        exit_code: 0,
        wall_s: 0.0,
        sim_hours: 10.0,
        first_passed_cycle_h: None,
        passed_cycles: 0,
        crashes: 0,
        trials: 0,
        real_answers: 0,
        real_answer_share: 0.0,
        scenario_real_answer_share: 0.0,
        trial_hours: 0.0,
        hunts: 0,
        combinations: 0,
        final_profile: PerCore::default(),
        depth: 0,
        hazard_per_h: BTreeMap::new(),
        hazard_max_per_h: 0.0,
        worst_r7_hazard_per_h: None,
    }
}

/// A git invocation that reads neither the user's nor the system's configuration.
pub(crate) fn isolate_git(command: &mut Command, home: &Path) {
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("HOME", home)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
}

#[expect(
    clippy::disallowed_methods,
    reason = "the test helper runs git to build its fixture repository"
)]
fn git(dir: &Path, home: &Path, args: &[&str]) {
    let mut command = Command::new("git");
    isolate_git(&mut command, home);
    let output = command.current_dir(dir).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A clean repository with one commit, inside the test's temporary directory `base` but apart from its
/// files, so the tests do not depend on the checkout they run from.
pub(crate) fn repo(base: &Path) -> PathBuf {
    let base = fs::canonicalize(base).unwrap();
    let dir = base.join("repo");
    if !dir.join(".git").exists() {
        fs::create_dir(&dir).unwrap();
        let home = base.join("githome");
        git(&dir, &home, &["init", "--quiet"]);
        git(
            &dir,
            &home,
            &[
                "-c",
                "user.name=bench",
                "-c",
                "user.email=bench@example.invalid",
                "commit",
                "--allow-empty",
                "--quiet",
                "--message",
                "init",
            ],
        );
    }
    dir
}
