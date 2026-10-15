use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A git invocation that reads neither the user's nor the system's configuration.
pub(crate) fn isolate_git(command: &mut Command, home: &Path) {
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.fsync")
        .env("GIT_CONFIG_VALUE_0", "none")
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
