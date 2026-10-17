use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use tempfile::TempDir;

use crate::diff::parse_diff;
use crate::fakes::{Handler, Harness, PR_URL, fail, read_file};
use crate::manifest::{hunk_file_name, read_manifest, reviewer_count};
use crate::process::{Output, Programs, System};
use crate::snapshot::run_snapshot;
use crate::snapshot::tests::TOO_LARGE;
use crate::text::quote;

/// git with no inherited configuration, attributes or Git environment, and sync disabled, run in `dir`.
fn clean_git(home: &Path, dir: &Path) -> System {
    let mut env: Vec<(OsString, OsString)> = [
        ("HOME", home.as_os_str()),
        ("GIT_CONFIG_NOSYSTEM", "1".as_ref()),
        ("GIT_ATTR_NOSYSTEM", "1".as_ref()),
        ("GIT_CONFIG_COUNT", "1".as_ref()),
        ("GIT_CONFIG_KEY_0", "core.fsync".as_ref()),
        ("GIT_CONFIG_VALUE_0", "none".as_ref()),
        ("GIT_AUTHOR_NAME", "Snapshot Test".as_ref()),
        ("GIT_AUTHOR_EMAIL", "snapshot@example.com".as_ref()),
        ("GIT_COMMITTER_NAME", "Snapshot Test".as_ref()),
        ("GIT_COMMITTER_EMAIL", "snapshot@example.com".as_ref()),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value.to_owned()))
    .collect();
    env.extend(std::env::var_os("PATH").map(|path| ("PATH".into(), path)));
    System {
        dir: Some(dir.to_owned()),
        env,
    }
}

fn run_git(home: &Path, dir: &Path, args: &[&str]) -> String {
    let out = clean_git(home, dir)
        .git(args)
        .unwrap_or_else(|error| panic!("git {args:?}: {error}"));
    String::from_utf8(out).expect("git output is UTF-8")
}

/// A pull request in a local origin: a shared ancestor, base commits after it and head commits with a text
/// change, a textconv target, a rename, a mode-only change and excluded paths.
struct LargePull {
    _root: TempDir,
    home: TempDir,
    origin: PathBuf,
    caller: PathBuf,
    base: String,
    head: String,
}

fn write(work: &Path, name: &str, content: &str, mode: u32) {
    let path = work.join(name);
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    fs::write(&path, content).expect("write");
    fs::set_permissions(&path, fs::Permissions::from_mode(mode)).expect("chmod");
}

impl LargePull {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("temporary directory");
        let home = tempfile::tempdir().expect("temporary directory");
        let work = root.path().join("work");
        let origin = root.path().join("origin.git");
        let caller = root.path().join("caller");
        let h = home.path();
        let git = |args: &[&str]| run_git(h, &work, args);
        let path = |p: &Path| p.to_string_lossy().into_owned();
        run_git(
            h,
            root.path(),
            &["init", "--quiet", "--initial-branch=main", &path(&work)],
        );
        write(
            &work,
            "src/text.go",
            "package src\n\nfunc One() int {\n\treturn 1\n}\n\nfunc Two() int {\n\treturn 2\n}\n",
            0o644,
        );
        write(
            &work,
            "src/conv.go",
            "package src\n\nconst Name = \"before\"\n",
            0o644,
        );
        write(
            &work,
            "docs/old.md",
            "# Notes\n\nA page long enough\nto be detected\nas a rename\nwhen it moves.\n",
            0o644,
        );
        write(&work, "run.sh", "#!/bin/sh\necho run\n", 0o644);
        write(&work, "go.sum", "example.com/a v1.0.0 h1:one\n", 0o644);
        write(&work, "src/testdata/in.txt", "input one\n", 0o644);
        write(&work, "base.txt", "base 0\n", 0o644);
        git(&["add", "."]);
        git(&["commit", "--quiet", "-m", "ancestor"]);
        git(&["switch", "--quiet", "-c", "feature"]);
        write(
            &work,
            "src/text.go",
            "package src\n\nfunc One() int {\n\treturn 1\n}\n\nfunc Two() int {\n\treturn 22\n}\n",
            0o644,
        );
        write(
            &work,
            "src/conv.go",
            "package src\n\nconst Name = \"after\"\n",
            0o644,
        );
        git(&["mv", "docs/old.md", "docs/new.md"]);
        git(&["commit", "--quiet", "-am", "head one"]);
        write(&work, "run.sh", "#!/bin/sh\necho run\n", 0o755);
        write(&work, "go.sum", "example.com/a v1.0.0 h1:two\n", 0o644);
        write(&work, "src/testdata/in.txt", "input two\n", 0o644);
        git(&["commit", "--quiet", "-am", "head two"]);
        let head = git(&["rev-parse", "HEAD"]).trim().to_owned();
        git(&["switch", "--quiet", "main"]);
        for i in 1..=2 {
            write(&work, "base.txt", &format!("base {i}\n"), 0o644);
            write(&work, &format!("base{i}.go"), "package base\n", 0o644);
            git(&["add", "."]);
            git(&["commit", "--quiet", "-m", &format!("base {i}")]);
        }
        let base = git(&["rev-parse", "HEAD"]).trim().to_owned();
        run_git(
            h,
            root.path(),
            &[
                "init",
                "--quiet",
                "--bare",
                "--initial-branch=main",
                &path(&origin),
            ],
        );
        git(&["push", "--quiet", &path(&origin), "main", "feature"]);
        let url = format!("file://{}", path(&origin));
        run_git(
            h,
            root.path(),
            &[
                "clone",
                "--quiet",
                "--depth",
                "1",
                "--branch",
                "main",
                &url,
                &path(&caller),
            ],
        );
        let shallow = run_git(h, &caller, &["rev-parse", "--is-shallow-repository"]);
        assert_eq!("true", shallow.trim(), "caller is not shallow");
        Self {
            _root: root,
            home,
            origin,
            caller,
            base,
            head,
        }
    }

    /// Answers pr view with the frozen base and head, refuses pr diff as too large, and reads files at the
    /// head from the origin.
    fn gh(&self) -> Handler {
        const FILES_PREFIX: &str = "repos/shgew/togi/contents/";
        let view = format!(
            r#"{{"headRefOid":{},"baseRefOid":{},"url":{}}}"#,
            quote(&self.head),
            quote(&self.base),
            quote(PR_URL)
        );
        let mut origin = clean_git(self.home.path(), &self.origin);
        Box::new(move |args: &[String]| {
            if args[0] == "pr" && args[1] == "view" {
                return Ok(view.clone().into_bytes());
            }
            if args[0] == "pr" && args[1] == "diff" {
                return Err(fail(TOO_LARGE));
            }
            if args.len() == 4 && args[0] == "api" && args[3].starts_with(FILES_PREFIX) {
                let (name, reference) = args[3][FILES_PREFIX.len()..]
                    .split_once("?ref=")
                    .unwrap_or_default();
                return origin.git(&["show", &format!("{reference}:{name}")]);
            }
            Err(fail(&format!("unexpected gh {args:?}")))
        })
    }

    /// Points every configuration and attribute source the caller's git reads at settings that change a diff,
    /// and makes the caller the working directory of git.
    fn hostile(&self, hostile: &Path) -> System {
        let config = format!(
            "[diff]\n\tnoprefix = true\n\tmnemonicPrefix = true\n\tcontext = 0\n[diff \"conv\"]\n\ttextconv = sed s/^/CONVERTED:/\n[core]\n\tattributesFile = {}\n",
            hostile.join("attributes").display()
        );
        let attributes = "src/conv.go diff=conv\nsrc/text.go -diff\n";
        for (name, content) in [
            (hostile.join(".gitconfig"), config.as_str()),
            (hostile.join("attributes"), attributes),
            (hostile.join("xdg/git/config"), config.as_str()),
            (hostile.join("xdg/git/attributes"), attributes),
            (self.caller.join(".git/info/attributes"), attributes),
            (hostile.join("external"), "#!/bin/sh\necho EXTERNAL\n"),
        ] {
            fs::create_dir_all(name.parent().expect("parent")).expect("mkdir");
            fs::write(&name, content).expect("write");
            fs::set_permissions(&name, fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        let git_config = self.caller.join(".git/config");
        let mut caller_config = fs::read_to_string(&git_config).expect("read caller config");
        caller_config.push_str(&config);
        fs::write(&git_config, caller_config).expect("write caller config");
        let mut env: Vec<(OsString, OsString)> = [
            ("HOME", hostile.as_os_str().to_owned()),
            ("XDG_CONFIG_HOME", hostile.join("xdg").into_os_string()),
            ("GIT_CONFIG_NOSYSTEM", "".into()),
            ("GIT_DIFF_OPTS", "--unified=1".into()),
            (
                "GIT_EXTERNAL_DIFF",
                hostile.join("external").into_os_string(),
            ),
            ("GIT_CONFIG_COUNT", "2".into()),
            ("GIT_CONFIG_KEY_0", "diff.noprefix".into()),
            ("GIT_CONFIG_VALUE_0", "true".into()),
            ("GIT_CONFIG_KEY_1", "diff.renames".into()),
            ("GIT_CONFIG_VALUE_1", "false".into()),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value))
        .collect();
        env.extend(std::env::var_os("PATH").map(|path| ("PATH".into(), path)));
        System {
            dir: Some(self.caller.clone()),
            env,
        }
    }
}

/// The faked gh with real git, as the snapshot runs it.
struct Real {
    gh: Handler,
    system: System,
}

impl Programs for Real {
    fn gh(&mut self, args: &[&str]) -> Output {
        let args: Vec<String> = args.iter().map(|&arg| arg.to_owned()).collect();
        (self.gh)(&args)
    }

    fn git(&mut self, args: &[&str]) -> Output {
        self.system.git(args)
    }

    fn isolated_git(&mut self, dir: &Path, args: &[&str]) -> Output {
        self.system.isolated_git(dir, args)
    }
}

fn snapshot(programs: &mut Real, tmp: &Path) -> Result<(), crate::Error> {
    let mut h = Harness::new(None, None);
    let mut t = h.tools();
    t.programs = programs;
    t.tmp = tmp.to_string_lossy().into_owned();
    run_snapshot(&mut t, &["700".to_owned()])
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("read dir")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn real_process_snapshot_too_large_diff_isolated_from_caller() {
    let p = LargePull::new();
    let reference_home = tempfile::tempdir().expect("temporary directory");
    let want = run_git(
        reference_home.path(),
        &p.origin,
        &["diff", "-M", &format!("{}...{}", p.base, p.head)],
    );
    for s in [
        "rename from docs/old.md",
        "old mode 100644",
        "-\treturn 2\n+\treturn 22\n",
        "-const Name = \"before\"\n+const Name = \"after\"\n",
    ] {
        assert!(want.contains(s), "the reference diff lacks {s:?}:\n{want}");
    }
    assert!(
        !want.contains("base"),
        "the reference diff holds base-only changes:\n{want}"
    );
    let hostile = tempfile::tempdir().expect("temporary directory");
    let mut programs = Real {
        gh: p.gh(),
        system: p.hostile(hostile.path()),
    };
    let tmp = tempfile::tempdir().expect("temporary directory");
    snapshot(&mut programs, tmp.path()).expect("snapshot");
    let dir = tmp
        .path()
        .join("togi-review")
        .join(format!("700-{}", &p.head[..8]));
    assert_eq!(
        want,
        read_file(dir.join("pr.diff")),
        "pr.diff differs from the origin's merge-base diff"
    );
    let m = read_manifest(&dir.to_string_lossy()).expect("manifest");
    let sections = parse_diff(want.as_bytes()).expect("parse");
    let mut got = Vec::new();
    for (i, f) in m.files.iter().enumerate() {
        got.push((
            f.path.as_str(),
            f.old_path.as_str(),
            f.status.as_str(),
            f.exclusion.as_str(),
        ));
        let hunk = dir.join(hunk_file_name(i + 1));
        if !f.exclusion.is_empty() {
            assert!(!hunk.exists(), "excluded file {} has a hunk file", f.path);
            continue;
        }
        assert!(
            f.hunk_file == hunk_file_name(i + 1) && read_file(&hunk).as_bytes() == sections[i].text,
            "{}'s hunk file is not its section of the origin's diff",
            f.path
        );
    }
    assert_eq!(
        vec![
            ("docs/new.md", "docs/old.md", "renamed", ""),
            ("go.sum", "", "modified", "generated"),
            ("run.sh", "", "modified", ""),
            ("src/conv.go", "", "modified", ""),
            (
                "src/testdata/in.txt",
                "",
                "modified",
                "test data (`**/testdata/**`)"
            ),
            ("src/text.go", "", "modified", ""),
        ],
        got
    );
    assert_eq!(
        (4, 4, reviewer_count(4, 4)),
        (m.included_files, m.included_lines, m.reviewers),
        "manifest totals"
    );
    assert_eq!(
        vec!["togi-review"],
        entries(tmp.path()),
        "snapshot left entries in its temporary directory"
    );
}

#[test]
fn real_process_snapshot_too_large_diff_fetch_failure() {
    let mut p = LargePull::new();
    p.head = "0123456789".repeat(4);
    let hostile = tempfile::tempdir().expect("temporary directory");
    let mut programs = Real {
        gh: p.gh(),
        system: p.hostile(hostile.path()),
    };
    let tmp = tempfile::tempdir().expect("temporary directory");
    let error = snapshot(&mut programs, tmp.path())
        .expect_err("snapshot of a pull whose head the origin lacks succeeded");
    // The operator sees the failed step, the command and its subcommand with git's exit status, and git's
    // own explanation.
    let error = error.render();
    for s in [
        "fetch the frozen base and head of pull request #700, whose diff GitHub refuses as too large: ",
        "git fetch: exit status ",
        "fatal: ",
        &p.head,
    ] {
        assert!(error.contains(s), "error lacks {s:?}: {error}");
    }
    assert!(
        entries(tmp.path()).is_empty(),
        "failed snapshot left entries in its temporary directory"
    );
}
