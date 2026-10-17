use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::rc::Rc;

use test_case::test_case;

use super::*;
use crate::chain;
use crate::fakes::{FakeGates, FakeGit, Requests, fake_api, repository, respond, uri, uris};

pub(super) const RELEASED: &str = "# Changelog\n\n## [0.1.0] - 2026-09-25\n\n### Added\n\n- New option ([#34]).\n\n[0.1.0]: https://forge.example/o/r/releases/tag/v0.1.0\n\n[#34]: https://forge.example/o/r/pull/34\n";
pub(super) const UNRELEASED_ONLY: &str = "# Changelog\n";
pub(super) const A_FIX: &[(&str, &str)] = &[("35.md", "### Fixed\n\n- A fix.\n")];
pub(super) const MANIFEST: &str = "[workspace]\nmembers = [\"crates/*\"]\n\n[workspace.package]\nrepository = \"https://forge.example/o/r\"\n";
const COMMIT_MUTATIONS: &[&str] = &[
    "read-tree base",
    "hash-object -w --stdin",
    "update-index --cacheinfo 100644,blob0,version.txt",
    "hash-object -w --stdin",
    "update-index --cacheinfo 100644,blob1,CHANGELOG.md",
    "update-index --force-remove changes/35.md",
    "write-tree",
    "commit-tree tree -p base -F -",
    "checkout --quiet --detach commit",
];

fn main_git(version: &str, changelog: &str, tags: &str, fragments: &[(&str, &str)]) -> FakeGit {
    let mut git = FakeGit::default();
    for (command, output) in [
        ("fetch --quiet origin main", ""),
        ("rev-parse --verify origin/main^{commit}", "base\n"),
        ("show base:version.txt", &format!("{version}\n")),
        ("show base:CHANGELOG.md", changelog),
        ("show base:Cargo.toml", MANIFEST),
        ("ls-remote --tags origin refs/tags/v*", tags),
        (
            "rev-parse --git-path togi-release-index",
            ".git/togi-release-index\n",
        ),
        ("read-tree base", ""),
        ("write-tree", "tree\n"),
        ("commit-tree tree -p base -F -", "commit\n"),
        ("checkout --quiet --detach base", ""),
        ("checkout --quiet --detach commit", ""),
    ] {
        git.respond(command, output);
    }
    let mut listing = String::new();
    for (name, content) in fragments {
        let _ = writeln!(listing, "changes/{name}");
        git.respond(&format!("show base:changes/{name}"), content);
    }
    git.respond(
        "ls-tree --name-only base changes/",
        &format!("{listing}changes/README.md\n"),
    );
    git
}

fn release(git: &mut FakeGit, gates: Option<&mut FakeGates>) -> (Result<(), ReleaseError>, String) {
    let mut out = Vec::new();
    let mut runner = Runner {
        git,
        today: "2026-09-26T00:00:00Z".parse().unwrap(),
        out: &mut out,
    };
    let mode = match gates {
        Some(gates) => Mode::Commit(gates),
        None => Mode::Preview,
    };
    let result = runner.release(mode);
    (result, String::from_utf8(out).unwrap())
}

#[test_case(
    RELEASED, "sha\trefs/tags/v0.1.0\n", A_FIX,
    "0.1.1", "No breaking changes (patch bump).",
    "# Changelog\n\n## [0.1.1] - 2026-09-26\n\n### Fixed\n\n- A fix ([#35]).\n\n## [0.1.0] - 2026-09-25\n\n### Added\n\n- New option ([#34]).\n\n[0.1.0]: https://forge.example/o/r/releases/tag/v0.1.0\n\n[0.1.1]: https://forge.example/o/r/releases/tag/v0.1.1\n\n[#34]: https://forge.example/o/r/pull/34\n[#35]: https://forge.example/o/r/pull/35\n" ;
    "bump"
)]
#[test_case(
    UNRELEASED_ONLY, "", &[("34.md", "### Added\n\n- New option.\n")],
    "0.1.1", "No breaking changes (patch bump).",
    "# Changelog\n\n## [0.1.1] - 2026-09-26\n\n### Added\n\n- New option ([#34]).\n\n[0.1.1]: https://forge.example/o/r/releases/tag/v0.1.1\n\n[#34]: https://forge.example/o/r/pull/34\n" ;
    "bump without tags"
)]
fn commits_release(
    changelog: &str,
    tags: &str,
    fragments: &[(&str, &str)],
    next: &str,
    reason: &str,
    changelog_out: &str,
) {
    let mut git = main_git("0.1.0", changelog, tags, fragments);
    let mut gates = FakeGates::default();
    let (result, out) = release(&mut git, Some(&mut gates));
    result.unwrap();
    let commands = git.commands();
    assert_eq!(commands[0], "fetch --quiet origin main");
    let (files, removed) = git.staged();
    assert_eq!(
        files,
        HashMap::from([
            (
                "version.txt".to_owned(),
                ("100644".to_owned(), format!("{next}\n"))
            ),
            (
                "CHANGELOG.md".to_owned(),
                ("100644".to_owned(), changelog_out.to_owned())
            ),
        ])
    );
    let consumed: Vec<String> = fragments
        .iter()
        .map(|(name, _)| format!("changes/{name}"))
        .collect();
    assert_eq!(removed, consumed);
    assert_eq!(
        git.stdin("commit-tree tree -p base -F -"),
        [format!("Release {next}\n\n{reason}\n")]
    );
    for call in &git.calls {
        if matches!(
            call.args[0].as_str(),
            "read-tree" | "hash-object" | "update-index" | "write-tree" | "commit-tree"
        ) {
            assert_eq!(
                call.env,
                [(
                    "GIT_INDEX_FILE".to_owned(),
                    ".git/togi-release-index".to_owned()
                )],
                "{}",
                call.command()
            );
        }
    }
    assert_eq!(commands.last().unwrap(), "checkout --quiet --detach commit");
    assert_eq!(git.removed, [".git/togi-release-index"]);
    assert_eq!(
        out,
        format!("Release {next} committed as commit on top of origin/main\n")
    );
    assert_eq!(gates.checked, ["base"]);
    assert_eq!(gates.queue_reads, 1);
}

#[test]
fn refuses_without_green_check() {
    let mut git = main_git("0.1.0", RELEASED, "sha\trefs/tags/v0.1.0\n", A_FIX);
    let mut gates = FakeGates {
        fail_check: true,
        ..FakeGates::default()
    };
    let (result, _) = release(&mut git, Some(&mut gates));
    assert_eq!(
        chain(&result.unwrap_err()),
        "require a passing check on origin/main: check run u for base concluded failure"
    );
    assert_eq!(git.mutations(), Vec::<String>::new());
}

/// A page of `n` open pull requests, which the issues endpoint lists among issues.
fn pull_requests(n: usize) -> String {
    let records: Vec<String> = (0..n)
        .map(|i| {
            format!(
                r#"{{"number":{},"title":"A pull request","pull_request":{{"url":"u"}}}}"#,
                1000 + i
            )
        })
        .collect();
    format!("[{}]", records.join(","))
}

fn queue_page(page: usize) -> String {
    format!("GET /repos/o/r/issues?labels=needs-hardware&page={page}&per_page=100&state=all")
}

#[test_case(
    vec![r#"[{"number":40,"title":"A pull request","pull_request":{"url":"u"}}]"#.to_owned()],
    "" ;
    "empty queue"
)]
#[test_case(
    vec![r#"[{"number":417,"title":"Backends: shared check","state":"open","pull_request":null},{"number":40,"title":"A pull request","state":"open","pull_request":{"url":"u"}}]"#.to_owned()],
    "refuse to release: issues wait on a run on the target machine (needs-hardware): #417 Backends: shared check" ;
    "open issue"
)]
#[test_case(
    vec![r#"[{"number":417,"title":"Backends: shared check","state":"closed","pull_request":null},{"number":40,"title":"A pull request","state":"closed","pull_request":{"url":"u"}}]"#.to_owned()],
    "refuse to release: issues wait on a run on the target machine (needs-hardware): #417 Backends: shared check (closed)" ;
    "closed issue"
)]
#[test_case(
    vec![r#"[{"number":417,"title":"Backends: shared check","state":"open"},{"number":418,"title":"Hardware-layer names","state":"closed"}]"#.to_owned()],
    "refuse to release: issues wait on a run on the target machine (needs-hardware): #417 Backends: shared check; #418 Hardware-layer names (closed)" ;
    "open and closed issues"
)]
#[test_case(
    vec![pull_requests(100), r#"[{"number":417,"title":"Backends: shared check","state":"closed","pull_request":null}]"#.to_owned()],
    "refuse to release: issues wait on a run on the target machine (needs-hardware): #417 Backends: shared check (closed)" ;
    "issue after a full page of pull requests"
)]
fn waits_on_the_target_machine(pages: Vec<String>, error: &str) {
    let want: Vec<String> = (1..=pages.len()).map(queue_page).collect();
    let (api, requests) = fake_api(move |request| {
        let page = pages
            .iter()
            .enumerate()
            .find(|(i, _)| uri(request) == queue_page(i + 1))
            .map(|(_, page)| page.clone());
        respond(
            200,
            &page.unwrap_or_else(|| panic!("unexpected {}", uri(request))),
        )
    });
    let mut git = main_git("0.1.0", RELEASED, "sha\trefs/tags/v0.1.0\n", A_FIX);
    let mut gates = FakeGates {
        queue: Some(api),
        ..FakeGates::default()
    };
    let (result, _) = release(&mut git, Some(&mut gates));
    assert_eq!(result.err().map(|e| chain(&e)).unwrap_or_default(), error);
    assert_eq!(uris(&requests), want);
    if error.is_empty() {
        assert_eq!(git.mutations(), COMMIT_MUTATIONS);
        assert_eq!(gates.checked, ["base"]);
    } else {
        assert_eq!(git.mutations(), Vec::<String>::new());
        assert_eq!(gates.checked, Vec::<String>::new());
    }
}

#[test]
fn preview_has_nothing_to_release() {
    let mut git = main_git("0.1.0", RELEASED, "sha\trefs/tags/v0.1.0\n", &[]);
    let (result, out) = release(&mut git, None);
    result.unwrap();
    assert_eq!(out, "nothing to release\n");
    assert_eq!(git.mutations(), Vec::<String>::new());
}

#[test]
fn commit_has_nothing_to_release() {
    let mut git = main_git("0.1.0", UNRELEASED_ONLY, "sha\trefs/tags/v0.1.0\n", &[]);
    let mut gates = FakeGates::default();
    let (result, out) = release(&mut git, Some(&mut gates));
    assert_eq!(
        chain(&result.unwrap_err()),
        "nothing to release: changes/ has no fragments"
    );
    assert_eq!(out, "");
    assert_eq!(git.mutations(), Vec::<String>::new());
}

#[test]
fn resumes_unpublished_release() {
    let mut git = main_git("0.1.0", RELEASED, "sha\trefs/tags/v0.0.9\n", &[]);
    let mut gates = FakeGates::default();
    let (result, out) = release(&mut git, Some(&mut gates));
    result.unwrap();
    assert_eq!(git.mutations(), ["checkout --quiet --detach base"]);
    assert_eq!(
        out,
        "0.1.0 is released in CHANGELOG.md; would publish origin/main if its GitHub Release is missing\n"
    );
    assert_eq!(gates.checked, Vec::<String>::new());
    assert_eq!(gates.queue_reads, 0);
}

#[test]
fn previews_release() {
    let mut git = main_git("0.1.0", RELEASED, "sha\trefs/tags/v0.1.0\n", A_FIX);
    let (result, out) = release(&mut git, None);
    result.unwrap();
    assert_eq!(git.mutations(), Vec::<String>::new());
    assert_eq!(
        out,
        "Would release from origin/main:\n\nRelease 0.1.1\n\nNo breaking changes (patch bump).\n\n### Fixed\n\n- A fix ([#35]).\n\n[#35]: https://forge.example/o/r/pull/35\n"
    );
}

#[test]
fn previews_publishing_an_untagged_release() {
    let mut git = main_git("0.1.0", RELEASED, "sha\trefs/tags/v0.0.9\n", &[]);
    let (result, out) = release(&mut git, None);
    result.unwrap();
    assert_eq!(git.mutations(), Vec::<String>::new());
    assert_eq!(
        out,
        "0.1.0 is released in CHANGELOG.md; would publish origin/main if its GitHub Release is missing\n"
    );
}

#[test_case(Some("fetch --quiet origin main"), None, "fetch main: git fetch --quiet origin main: exit status 1: injected" ; "fetch")]
#[test_case(Some("rev-parse --verify origin/main^{commit}"), None, "resolve origin/main: git rev-parse --verify origin/main^{commit}: exit status 1: injected" ; "resolve")]
#[test_case(Some("show base:CHANGELOG.md"), None, "read CHANGELOG.md from origin/main: git show base:CHANGELOG.md: exit status 1: injected" ; "read")]
#[test_case(Some("show base:Cargo.toml"), None, "read Cargo.toml from origin/main: git show base:Cargo.toml: exit status 1: injected" ; "read manifest")]
#[test_case(None, Some(("show base:version.txt", "bad")), "read version.txt: invalid semantic version \"bad\"" ; "version")]
#[test_case(None, Some(("show base:Cargo.toml", "[workspace.package]\nname = \"togi\"\n")), "read Cargo.toml: missing [workspace.package] repository" ; "manifest without repository")]
#[test_case(None, Some(("show base:Cargo.toml", "[workspace.package]\nrepository = 1\n")), "read Cargo.toml: missing [workspace.package] repository" ; "manifest repository not a string")]
#[test_case(None, Some(("show base:Cargo.toml", "not a manifest")), "read Cargo.toml: parse: TOML parse error at line 1, column 5\n  |\n1 | not a manifest\n  |     ^\nkey with no value, expected `=`\n" ; "invalid manifest")]
#[test_case(Some("ls-tree --name-only base changes/"), None, "read changes/ from origin/main: list: git ls-tree --name-only base changes/: exit status 1: injected" ; "list fragments")]
#[test_case(Some("show base:changes/35.md"), None, "read changes/ from origin/main: read 35.md: git show base:changes/35.md: exit status 1: injected" ; "read fragment")]
#[test_case(None, Some(("show base:changes/35.md", "- A fix.\n")), "read changes/: 35.md:1: entry before any ### section heading" ; "invalid fragment")]
#[test_case(Some("ls-remote --tags origin refs/tags/v*"), None, "list release tags: git ls-remote --tags origin refs/tags/v*: exit status 1: injected" ; "tags")]
#[test_case(None, Some(("show base:version.txt", "9999999999999999999999999999999999999999.1.0")), "bump version: parse version \"9999999999999999999999999999999999999999.1.0\": number too large to fit in target type" ; "overflow")]
#[test_case(None, Some(("show base:CHANGELOG.md", &format!("{RELEASED}\n## [0.1.1] - 2026-09-25\n"))), "rewrite changelog: version [0.1.1] already exists" ; "duplicate")]
fn release_failures_do_not_commit(
    failure: Option<&str>,
    response: Option<(&str, &str)>,
    want: &str,
) {
    let mut git = main_git("0.1.0", RELEASED, "", A_FIX);
    git.failures.extend(failure.map(ToOwned::to_owned));
    if let Some((command, output)) = response {
        git.respond(command, output);
    }
    let mut gates = FakeGates::default();
    let (result, out) = release(&mut git, Some(&mut gates));
    assert_eq!(chain(&result.unwrap_err()), want);
    assert_eq!(git.mutations(), Vec::<String>::new());
    assert_eq!(gates.checked, Vec::<String>::new());
    assert_eq!(out, "");
}

#[test_case("rev-parse --git-path togi-release-index", "commit release 0.1.1: locate temporary index: git rev-parse --git-path togi-release-index: exit status 1: injected" ; "locate index")]
#[test_case("read-tree base", "commit release 0.1.1: read parent tree: git read-tree base: exit status 1: injected" ; "read tree")]
#[test_case("hash-object -w --stdin", "commit release 0.1.1: store version.txt: git hash-object -w --stdin: exit status 1: injected" ; "store")]
#[test_case("update-index --cacheinfo", "commit release 0.1.1: stage version.txt: git update-index --cacheinfo 100644,blob0,version.txt: exit status 1: injected" ; "stage")]
#[test_case("update-index --force-remove", "commit release 0.1.1: remove changes/35.md: git update-index --force-remove changes/35.md: exit status 1: injected" ; "remove")]
#[test_case("write-tree", "commit release 0.1.1: write tree: git write-tree: exit status 1: injected" ; "write tree")]
#[test_case("commit-tree tree -p base -F -", "commit release 0.1.1: create commit: git commit-tree tree -p base -F -: exit status 1: injected" ; "create commit")]
#[test_case("checkout --quiet --detach commit", "check out commit: git checkout --quiet --detach commit: exit status 1: injected" ; "checkout")]
fn commit_failures_stop_before_checkout(failure: &str, want: &str) {
    let mut git = main_git("0.1.0", RELEASED, "", A_FIX);
    git.failures.push(failure.to_owned());
    let mut gates = FakeGates::default();
    let (result, out) = release(&mut git, Some(&mut gates));
    assert_eq!(chain(&result.unwrap_err()), want);
    let last = git.commands().pop().unwrap();
    assert!(
        last == failure || last.starts_with(&format!("{failure} ")),
        "continued after failure: {last}"
    );
    if failure == "checkout --quiet --detach commit" {
        assert_eq!(
            out,
            "Release 0.1.1 committed as commit on top of origin/main\n"
        );
    } else {
        assert_eq!(out, "");
    }
    let removed: &[&str] = if failure.starts_with("rev-parse") {
        &[]
    } else {
        &[".git/togi-release-index"]
    };
    assert_eq!(git.removed, removed);
}

fn publish_git(version: &str, changelog: &str) -> FakeGit {
    let mut git = FakeGit::default();
    git.respond("show HEAD:version.txt", &format!("{version}\n"));
    git.respond("show HEAD:CHANGELOG.md", changelog);
    git.respond("rev-parse --is-shallow-repository", "false\n");
    git.respond(
        "log --first-parent -1 --format=%H HEAD -- version.txt",
        "merge-sha\n",
    );
    git.respond(
        &format!("ls-remote --tags origin refs/tags/v{version} refs/tags/v{version}^{{}}"),
        &format!("merge-sha\trefs/tags/v{version}\n"),
    );
    git
}

pub(super) type Posted = Rc<RefCell<Option<serde_json::Value>>>;

/// An API whose release lookup answers `lookup` and whose release creation answers `create`, recording the
/// created release.
pub(super) fn publish_api(lookup: u16, create: u16) -> (GitHub, Requests, Posted) {
    let posted = Posted::default();
    let record = Rc::clone(&posted);
    let (api, requests) = fake_api(move |request| match uri(request).as_str() {
        "GET /repos/o/r/releases/tags/v0.1.0" => respond(lookup, "{}"),
        "POST /repos/o/r/releases" if create == 201 => {
            let body = request.body.as_deref().unwrap_or_default();
            *record.borrow_mut() = Some(serde_json::from_str(body).unwrap());
            respond(
                201,
                r#"{"html_url":"https://forge.example/o/r/releases/tag/v0.1.0"}"#,
            )
        }
        "POST /repos/o/r/releases" => respond(create, "denied"),
        other => panic!("unexpected request {other}"),
    });
    (api, requests, posted)
}

fn publish(git: &mut FakeGit, api: &mut GitHub) -> (Result<(), PublishError>, String) {
    let mut out = Vec::new();
    let mut runner = Runner {
        git,
        today: "2026-09-26T00:00:00Z".parse().unwrap(),
        out: &mut out,
    };
    let result = runner.publish(api, &repository());
    (result, String::from_utf8(out).unwrap())
}

#[test]
fn publishes_release() {
    let mut git = publish_git("0.1.0", RELEASED);
    let (mut api, requests, posted) = publish_api(404, 201);
    let (result, out) = publish(&mut git, &mut api);
    result.unwrap();
    assert_eq!(
        posted.borrow().clone(),
        Some(serde_json::json!({
            "tag_name": "v0.1.0",
            "target_commitish": "merge-sha",
            "name": "0.1.0",
            "body": "### Added\n\n- New option ([#34]).\n\n[#34]: https://forge.example/o/r/pull/34",
        }))
    );
    assert_eq!(
        uris(&requests),
        [
            "GET /repos/o/r/releases/tags/v0.1.0",
            "POST /repos/o/r/releases"
        ]
    );
    assert_eq!(out, "https://forge.example/o/r/releases/tag/v0.1.0\n");
}

#[test_case(RELEASED, 200, &["GET /repos/o/r/releases/tags/v0.1.0"], "nothing to publish: GitHub Release v0.1.0 already exists\n" ; "already published")]
#[test_case(UNRELEASED_ONLY, 404, &[], "nothing to publish: CHANGELOG.md has no released [0.1.0] section\n" ; "not released yet")]
fn publish_skips(changelog: &str, lookup: u16, requests_want: &[&str], output: &str) {
    let mut git = publish_git("0.1.0", changelog);
    let (mut api, requests, posted) = publish_api(lookup, 201);
    let (result, out) = publish(&mut git, &mut api);
    result.unwrap();
    assert_eq!(*posted.borrow(), None);
    assert_eq!(uris(&requests), requests_want);
    assert_eq!(out, output);
}

#[test_case(Some("show HEAD:version.txt"), None, "read version.txt: git show HEAD:version.txt: exit status 1: injected" ; "read")]
#[test_case(None, Some(("show HEAD:version.txt", "bad")), "read version.txt: invalid semantic version \"bad\"" ; "version")]
#[test_case(Some("rev-parse --is-shallow-repository"), None, "check release history: git rev-parse --is-shallow-repository: exit status 1: injected" ; "history")]
#[test_case(Some("fetch --quiet --unshallow --no-tags origin main"), Some(("rev-parse --is-shallow-repository", "true\n")), "fetch complete release history: git fetch --quiet --unshallow --no-tags origin main: exit status 1: injected" ; "unshallow")]
#[test_case(Some("log --first-parent -1 --format=%H HEAD -- version.txt"), None, "find the commit that set version.txt to 0.1.0: git log --first-parent -1 --format=%H HEAD -- version.txt: exit status 1: injected" ; "log")]
#[test_case(None, Some(("log --first-parent -1 --format=%H HEAD -- version.txt", "\n")), "find the commit that set version.txt to 0.1.0: no commit changed version.txt" ; "empty history")]
#[test_case(Some("ls-remote --tags origin refs/tags/v0.1.0 refs/tags/v0.1.0^{}"), None, "check tag v0.1.0: git ls-remote --tags origin refs/tags/v0.1.0 refs/tags/v0.1.0^{}: exit status 1: injected" ; "tag lookup")]
#[test_case(None, Some(("ls-remote --tags origin refs/tags/v0.1.0 refs/tags/v0.1.0^{}", "other-sha\trefs/tags/v0.1.0\n")), "tag v0.1.0 points at other-sha, but the release commit is merge-sha" ; "tag elsewhere")]
#[test_case(None, Some(("ls-remote --tags origin refs/tags/v0.1.0 refs/tags/v0.1.0^{}", "tag-object\trefs/tags/v0.1.0\nother-sha\trefs/tags/v0.1.0^{}\n")), "tag v0.1.0 points at other-sha, but the release commit is merge-sha" ; "annotated tag elsewhere")]
fn publish_failures_do_not_create_release(
    failure: Option<&str>,
    response: Option<(&str, &str)>,
    want: &str,
) {
    let mut git = publish_git("0.1.0", RELEASED);
    git.failures.extend(failure.map(ToOwned::to_owned));
    if let Some((command, output)) = response {
        git.respond(command, output);
    }
    let (mut api, requests, posted) = publish_api(404, 201);
    let (result, out) = publish(&mut git, &mut api);
    assert_eq!(chain(&result.unwrap_err()), want);
    assert_eq!(*posted.borrow(), None);
    assert_eq!(uris(&requests), Vec::<String>::new());
    assert_eq!(out, "");
}

#[test]
fn publish_accepts_annotated_tag_on_release_commit() {
    let mut git = publish_git("0.1.0", RELEASED);
    git.respond(
        "ls-remote --tags origin refs/tags/v0.1.0 refs/tags/v0.1.0^{}",
        "tag-object\trefs/tags/v0.1.0\nmerge-sha\trefs/tags/v0.1.0^{}\n",
    );
    let (mut api, _, posted) = publish_api(404, 201);
    let (result, _) = publish(&mut git, &mut api);
    result.unwrap();
    assert!(posted.borrow().is_some());
}

#[test_case(403, 201, "check release v0.1.0: GET /repos/o/r/releases/tags/v0.1.0: HTTP 403: {}" ; "lookup refused")]
#[test_case(404, 403, "create release v0.1.0: POST /repos/o/r/releases: HTTP 403: denied" ; "creation refused")]
fn github_failures_reach_operator(lookup: u16, create: u16, want: &str) {
    let mut git = publish_git("0.1.0", RELEASED);
    let (mut api, _, posted) = publish_api(lookup, create);
    let (result, out) = publish(&mut git, &mut api);
    assert_eq!(chain(&result.unwrap_err()), want);
    assert_eq!(*posted.borrow(), None);
    assert_eq!(out, "");
}
