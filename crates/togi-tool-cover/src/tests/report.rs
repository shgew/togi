use std::fs;
use std::path::Path;
use std::process::Command;

use super::*;
use crate::cfg::HostCfg;
use crate::chain;
use crate::fakes::MemTree;

const LIB_BASE: &str =
    "fn a() -> u32 {\n    1\n}\n\n#[cfg(target_os = \"nowhere\")]\nfn gone() -> u32 {\n    1\n}\n";
const LIB_HEAD: &str = "fn a() -> u32 {\n    2\n}\n\n#[cfg(target_os = \"nowhere\")]\nfn gone() -> u32 {\n    2\n}\n\nfn b() {\n    todo!();\n}\n";
const DIFF: &str = "diff --git crates/a/src/lib.rs crates/a/src/lib.rs\nindex 1..2 100644\n--- crates/a/src/lib.rs\n+++ crates/a/src/lib.rs\n@@ -2 +2 @@ fn a() -> u32 {\n-    1\n+    2\n@@ -7 +7 @@ fn gone() -> u32 {\n-    1\n+    2\n@@ -8,0 +9,4 @@ fn gone() -> u32 {\n+\n+fn b() {\n+    todo!();\n+}\ndiff --git crates/a/build.rs crates/a/build.rs\n--- crates/a/build.rs\n+++ crates/a/build.rs\n@@ -2 +2 @@ fn main() {\n-    let _ = 1;\n+    let _ = 2;\n";
const TRACE: &str = "SF:/repo/crates/a/src/lib.rs\nDA:1,1\nDA:2,1\nDA:9,1\nDA:10,0\nend_of_record\nSF:/repo/crates/a/build.rs\nDA:1,1\nDA:2,1\nend_of_record\n";

fn test_host() -> Host {
    Host {
        triple: "test-host".to_owned(),
        cfg: HostCfg::new(["unix", r#"target_os="linux""#]),
    }
}

fn tree() -> MemTree {
    MemTree::new(
        "/repo",
        &[
            ("crates/a/src/lib.rs", LIB_HEAD),
            ("crates/a/build.rs", "fn main() {\n    let _ = 2;\n}\n"),
            ("target/debug/deps/a.d", "crates/a/src/lib.rs:\n"),
            (
                "target/debug/build/a-1/build_script_build.d",
                "crates/a/build.rs:\n",
            ),
        ],
    )
}

fn report_text(diff: &str, trace: &str) -> Result<String, Error> {
    let host = test_host();
    let mut out = Vec::new();
    report(
        &tree(),
        Inputs {
            diff: diff.as_bytes(),
            trace,
            build_dir: Path::new("/repo/target/debug"),
            root: Path::new("/repo"),
            base: "HEAD",
            host: &host,
        },
        &mut out,
    )?;
    Ok(String::from_utf8(out).unwrap())
}

#[test]
fn unreached_ranges_come_before_files_the_build_leaves_out() {
    assert_eq!(
        report_text(DIFF, TRACE).unwrap(),
        "crates/a/src/lib.rs:10\ncrates/a/src/lib.rs: not built for test-host\n"
    );
}

#[test]
fn every_line_reached_is_said_before_files_the_build_leaves_out() {
    let trace = TRACE.replace("DA:10,0", "DA:10,1");
    assert_eq!(
        report_text(DIFF, &trace).unwrap(),
        "Every changed Rust line the trace measures ran in a test since HEAD.\ncrates/a/src/lib.rs: not built for test-host\n"
    );
}

#[test]
fn no_changed_rust_lines_reads_nothing_else() {
    assert_eq!(
        report_text("", "not a trace").unwrap(),
        "No Rust lines changed since HEAD.\n"
    );
}

#[test]
fn a_malformed_trace_is_refused() {
    let error = report_text(DIFF, "SF:/repo/crates/a/src/lib.rs\nDA:1\n").unwrap_err();
    assert_eq!(chain(&error), "malformed lcov line \"DA:1\"");
}

/// Runs git in `dir` with no system or global configuration and a fixed identity.
fn git(dir: &Path, args: &[&str]) {
    #[expect(clippy::disallowed_methods, reason = "a test helper running git")]
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.fsync")
        .env("GIT_CONFIG_VALUE_0", "none")
        .env("GIT_AUTHOR_NAME", "Coverage Test")
        .env("GIT_AUTHOR_EMAIL", "coverage@example.com")
        .env("GIT_COMMITTER_NAME", "Coverage Test")
        .env("GIT_COMMITTER_EMAIL", "coverage@example.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write(dir: &Path, name: &str, content: &str) {
    let path = dir.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// A repository whose working tree changes the files of `base` to those of `head`.
fn repository(base: &[(&str, &str)], head: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "--quiet", "--initial-branch=main"]);
    for (name, content) in base {
        write(dir.path(), name, content);
    }
    git(dir.path(), &["add", "."]);
    git(dir.path(), &["commit", "--quiet", "-m", "Base"]);
    for (name, content) in head {
        write(dir.path(), name, content);
    }
    dir
}

#[test]
fn real_process_the_diff_ignores_configured_inter_hunk_context() {
    let dir = repository(
        &[(
            "a.rs",
            "fn f() {\n    let x = 1;\n    let y = 2;\n    let z = 3;\n}\n",
        )],
        &[(
            "a.rs",
            "fn f() {\n    let x = 4;\n    let y = 2;\n    let z = 5;\n}\n",
        )],
    );
    git(dir.path(), &["config", "diff.interHunkContext", "10"]);
    let changed =
        crate::diff::parse_diff(&process::rust_diff(dir.path(), "HEAD").unwrap()).unwrap();
    let lines: Vec<_> = changed["a.rs"]
        .iter()
        .map(|range| (range.first, range.last))
        .collect();
    assert_eq!(lines, [(2, 2), (4, 4)]);
}

#[test]
fn real_process_the_diff_against_an_unknown_base_is_refused() {
    let dir = repository(&[("a.rs", "fn f() {}\n")], &[]);
    let error = process::rust_diff(dir.path(), "no-such-revision").unwrap_err();
    assert!(
        chain(&error).starts_with("diff against no-such-revision: git exited with "),
        "{}",
        chain(&error)
    );
}

#[test]
fn real_process_run_reports_lines_and_files_the_build_leaves_out() {
    let dir = repository(
        &[
            ("crates/a/src/lib.rs", LIB_BASE),
            ("crates/a/build.rs", "fn main() {\n    let _ = 1;\n}\n"),
            ("target/debug/deps/a.d", "crates/a/src/lib.rs:\n"),
            (
                "target/debug/build/a-1/build_script_build.d",
                "crates/a/build.rs:\n",
            ),
        ],
        &[
            ("crates/a/src/lib.rs", LIB_HEAD),
            ("crates/a/build.rs", "fn main() {\n    let _ = 2;\n}\n"),
        ],
    );
    let root = dir.path();
    write(
        root,
        "trace.info",
        &TRACE.replace("/repo", &root.to_string_lossy()),
    );
    let options = crate::cli::Options {
        lcov: "trace.info".into(),
        build_dir: "target/debug".into(),
        base: "HEAD".to_owned(),
    };
    let mut out = Vec::new();
    run(&options, root, &mut out).unwrap();
    let triple = Host::of_rustc(root).unwrap().triple;
    assert_eq!(
        String::from_utf8(out).unwrap(),
        format!("crates/a/src/lib.rs:10\ncrates/a/src/lib.rs: not built for {triple}\n")
    );
}

#[test]
fn a_missing_trace_is_refused() {
    let options = crate::cli::Options {
        lcov: "missing.info".into(),
        build_dir: "target/debug".into(),
        base: "HEAD".to_owned(),
    };
    let error = run(
        &options,
        Path::new("/nonexistent-cover-root"),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert_eq!(
        chain(&error),
        "read the lcov trace missing.info: No such file or directory (os error 2)"
    );
}
