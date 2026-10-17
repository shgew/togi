//! The release against real git repositories: a bare remote and a checkout in the test's temporary directory.

use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use test_case::test_case;

use super::tests::{A_FIX, MANIFEST, RELEASED, UNRELEASED_ONLY, publish_api};
use super::*;
use crate::fakes::{FakeGates, repository};
use crate::git::ProcessGit;

/// Git in `dir` reading neither the user's nor the system's configuration, with sync disabled.
fn isolated(dir: &Path, home: &Path) -> ProcessGit {
    ProcessGit::new(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.fsync")
        .env("GIT_CONFIG_VALUE_0", "none")
        .env("HOME", &home.to_string_lossy())
        .env("GIT_AUTHOR_NAME", "Release Test")
        .env("GIT_AUTHOR_EMAIL", "release@example.com")
        .env("GIT_COMMITTER_NAME", "Release Test")
        .env("GIT_COMMITTER_EMAIL", "release@example.com")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
}

struct Repo {
    root: TempDir,
    dir: PathBuf,
    remote: PathBuf,
    git: ProcessGit,
}

impl Repo {
    /// A checkout whose `main`, pushed to the bare remote, released 0.1.0 with `changelog` and has `fragments`.
    fn new(changelog: &str, fragments: &[(&str, &str)]) -> Self {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("checkout");
        let remote = root.path().join("remote.git");
        fs::create_dir(&dir).unwrap();
        let git = isolated(&dir, &root.path().join("home"));
        let mut repo = Self {
            root,
            dir,
            remote,
            git,
        };
        let remote = repo.remote.to_string_lossy().into_owned();
        repo.run(&[
            "init",
            "--quiet",
            "--bare",
            "--initial-branch=main",
            &remote,
        ]);
        repo.run(&["init", "--quiet", "--initial-branch=main"]);
        repo.run(&["remote", "add", "origin", &remote]);
        repo.write("version.txt", "0.0.9\n");
        repo.write("CHANGELOG.md", changelog);
        repo.write("Cargo.toml", MANIFEST);
        fs::create_dir(repo.dir.join("changes")).unwrap();
        repo.write("changes/README.md", "# Changes\n");
        for (name, content) in fragments {
            repo.write(&format!("changes/{name}"), content);
        }
        repo.run(&["add", "."]);
        repo.run(&["commit", "--quiet", "-m", "Previous version"]);
        repo.write("version.txt", "0.1.0\n");
        repo.run(&["add", "version.txt"]);
        repo.run(&["commit", "--quiet", "-m", "Release 0.1.0"]);
        repo.run(&["push", "--quiet", "origin", "HEAD:refs/heads/main"]);
        repo
    }

    fn run(&mut self, args: &[&str]) -> String {
        self.git
            .run(GitCommand::new(args))
            .unwrap()
            .trim()
            .to_owned()
    }

    fn write(&self, path: &str, content: &str) {
        fs::write(self.dir.join(path), content).unwrap();
    }

    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.dir.join(path)).unwrap()
    }

    fn tag(&mut self, args: &[&str]) {
        self.run(&[&["tag"], args].concat());
        self.run(&[
            "push",
            "--quiet",
            "origin",
            "refs/tags/v0.1.0:refs/tags/v0.1.0",
        ]);
    }

    fn release(&mut self, gates: Option<&mut FakeGates>) -> String {
        let mut out = Vec::new();
        let mut runner = Runner {
            git: &mut self.git,
            today: "2026-09-26T00:00:00Z".parse().unwrap(),
            out: &mut out,
        };
        let mode = match gates {
            Some(gates) => Mode::Commit(gates),
            None => Mode::Preview,
        };
        runner.release(mode).unwrap();
        String::from_utf8(out).unwrap()
    }
}

fn publish(
    git: &mut ProcessGit,
    lookup: u16,
) -> (Result<(), PublishError>, Option<serde_json::Value>) {
    let (mut api, _, posted) = publish_api(lookup, 201);
    let mut out = Vec::new();
    let mut runner = Runner {
        git,
        today: "2026-09-26T00:00:00Z".parse().unwrap(),
        out: &mut out,
    };
    let result = runner.publish(&mut api, &repository());
    (result, posted.borrow().clone())
}

/// The `v0.1.0` tag on the remote before publishing, and the commit it names.
#[derive(Clone, Copy)]
enum Tag {
    Missing,
    Lightweight { wrong: bool },
    Annotated { wrong: bool },
}

#[test_case(Tag::Missing, false ; "no tag")]
#[test_case(Tag::Lightweight { wrong: false }, false ; "tag only")]
#[test_case(Tag::Annotated { wrong: false }, false ; "annotated tag only")]
#[test_case(Tag::Lightweight { wrong: false }, true ; "release present")]
#[test_case(Tag::Lightweight { wrong: true }, false ; "wrong target")]
#[test_case(Tag::Annotated { wrong: true }, false ; "wrong annotated target")]
#[test_case(Tag::Lightweight { wrong: true }, true ; "wrong target with release")]
fn real_process_publish_temporary_repository(tag: Tag, published: bool) {
    let mut repo = Repo::new(RELEASED, &[]);
    let commit = repo.run(&["rev-parse", "HEAD"]);
    let wrong = matches!(
        tag,
        Tag::Lightweight { wrong: true } | Tag::Annotated { wrong: true }
    );
    let target = if wrong {
        repo.run(&["rev-parse", "HEAD^"])
    } else {
        commit.clone()
    };
    match tag {
        Tag::Missing => {}
        Tag::Lightweight { .. } => repo.tag(&["v0.1.0", &target]),
        Tag::Annotated { .. } => repo.tag(&["-a", "v0.1.0", &target, "-m", "Release"]),
    }
    let (result, posted) = publish(&mut repo.git, if published { 200 } else { 404 });
    if wrong {
        assert_eq!(
            crate::chain(&result.unwrap_err()),
            format!("tag v0.1.0 points at {target}, but the release commit is {commit}")
        );
        assert_eq!(posted, None);
        return;
    }
    result.unwrap();
    if published {
        assert_eq!(posted, None);
        return;
    }
    assert_eq!(
        posted,
        Some(serde_json::json!({
            "tag_name": "v0.1.0",
            "target_commitish": commit,
            "name": "0.1.0",
            "body": "### Added\n\n- New option ([#34]).\n\n[#34]: https://forge.example/o/r/pull/34",
        }))
    );
}

#[test]
fn real_process_recovery_preview_preserves_checkout() {
    let mut repo = Repo::new(RELEASED, A_FIX);
    repo.run(&["switch", "--quiet", "-c", "work"]);
    repo.write("staged.txt", "staged\n");
    repo.run(&["add", "staged.txt"]);
    repo.write("staged.txt", "unstaged\n");
    repo.write("untracked.txt", "untracked\n");
    let snapshot = |repo: &mut Repo| {
        [
            repo.run(&["symbolic-ref", "HEAD"]),
            repo.run(&["rev-parse", "HEAD"]),
            String::from_utf8_lossy(&fs::read(repo.dir.join(".git/index")).unwrap()).into_owned(),
            repo.run(&["diff", "--binary", "HEAD"]),
            repo.run(&["status", "--porcelain=v1"]),
            repo.read("staged.txt"),
            repo.read("untracked.txt"),
        ]
    };
    let before = snapshot(&mut repo);
    let out = repo.release(None);
    assert_eq!(snapshot(&mut repo), before);
    assert!(
        out.starts_with("Would release from origin/main:\n\nRelease 0.1.1\n"),
        "{out}"
    );
}

#[test]
fn real_process_release_without_tags_bumps_version() {
    let mut repo = Repo::new(
        UNRELEASED_ONLY,
        &[("34.md", "### Added\n\n- New option.\n")],
    );
    let mut gates = FakeGates::default();
    let out = repo.release(Some(&mut gates));
    let head = repo.run(&["rev-parse", "HEAD"]);
    assert_eq!(
        out,
        format!("Release 0.1.1 committed as {head} on top of origin/main\n")
    );
    assert_eq!(repo.run(&["show", "HEAD:version.txt"]), "0.1.1");
    assert_eq!(repo.run(&["log", "-1", "--format=%s"]), "Release 0.1.1");
    assert!(
        repo.run(&["show", "HEAD:CHANGELOG.md"])
            .contains("## [0.1.1] - 2026-09-26\n\n### Added\n\n- New option ([#34])."),
    );
    assert_eq!(
        repo.run(&["ls-tree", "--name-only", "HEAD", "changes/"]),
        "changes/README.md"
    );
    assert_eq!(
        repo.run(&["rev-parse", "HEAD^"]),
        repo.run(&["rev-parse", "origin/main"])
    );
    assert!(!repo.dir.join(".git/togi-release-index").exists());
}

#[test]
fn real_process_release_resumes_tag_only_repository() {
    let mut repo = Repo::new(RELEASED, &[]);
    let commit = repo.run(&["rev-parse", "HEAD"]);
    repo.tag(&["v0.1.0"]);
    repo.run(&["switch", "--quiet", "-c", "work"]);
    repo.write("later.txt", "later\n");
    repo.run(&["add", "later.txt"]);
    repo.run(&["commit", "--quiet", "-m", "Later work"]);
    let mut gates = FakeGates::default();
    repo.release(Some(&mut gates));
    assert_eq!(repo.run(&["rev-parse", "HEAD"]), commit);
}

#[test]
fn real_process_publish_tag_only_from_shallow_later_commit() {
    let mut repo = Repo::new(RELEASED, &[]);
    let commit = repo.run(&["rev-parse", "HEAD"]);
    repo.tag(&["v0.1.0"]);
    repo.write("later.txt", "later\n");
    repo.run(&["add", "later.txt"]);
    repo.run(&["commit", "--quiet", "-m", "Later work"]);
    repo.run(&["push", "--quiet", "origin", "HEAD:refs/heads/main"]);
    let clone = repo.root.path().join("shallow");
    let url = format!("file://{}", repo.remote.display());
    repo.run(&[
        "clone",
        "--quiet",
        "--depth=1",
        &url,
        &clone.to_string_lossy(),
    ]);
    let mut shallow = isolated(&clone, &repo.root.path().join("home"));
    assert_eq!(
        shallow
            .run(GitCommand::new(&["rev-parse", "--is-shallow-repository"]))
            .unwrap(),
        "true\n"
    );
    let (result, posted) = publish(&mut shallow, 404);
    result.unwrap();
    assert_eq!(posted.unwrap()["target_commitish"], commit.as_str());
}

#[test]
fn real_process_preview_reads_fetched_main_not_checkout() {
    let mut repo = Repo::new(RELEASED, &[]);
    let other = repo.root.path().join("other");
    let remote = repo.remote.to_string_lossy().into_owned();
    repo.run(&["clone", "--quiet", &remote, &other.to_string_lossy()]);
    let mut upstream = isolated(&other, &repo.root.path().join("home"));
    fs::write(other.join("changes/35.md"), "### Fixed\n\n- A fix.\n").unwrap();
    fs::write(
        other.join("Cargo.toml"),
        "[workspace.package]\nrepository = \"https://forge.example/moved/r\"\n",
    )
    .unwrap();
    for args in [
        &["add", "."][..],
        &["commit", "--quiet", "-m", "Fix"],
        &["push", "--quiet", "origin", "HEAD:refs/heads/main"],
    ] {
        upstream.run(GitCommand::new(args)).unwrap();
    }
    repo.write("version.txt", "7.0.0\n");
    repo.write(
        "Cargo.toml",
        "[workspace.package]\nrepository = \"https://forge.example/local/r\"\n",
    );
    repo.write("changes/36.md", "### Added\n\n- Local work.\n");
    let out = repo.release(None);
    assert_eq!(
        out,
        "Would release from origin/main:\n\nRelease 0.1.1\n\nNo breaking changes (patch bump).\n\n### Fixed\n\n- A fix ([#35]).\n\n[#35]: https://forge.example/moved/r/pull/35\n"
    );
}
