//! The release: a preview of what `origin/main` would release, the release commit the workflow pushes and the
//! GitHub Release it publishes ([`docs/releasing.md`](../../../docs/releasing.md), ADRs 0014, 0033 and 0043).

use std::collections::BTreeMap;
use std::io::{self, Write};

use jiff::Timestamp;
use serde::Deserialize;

use crate::changelog::{
    BumpError, RewriteError, bump, is_version, release_notes, rewrite_changelog, section_named,
};
use crate::fragments::{FRAGMENT_DIR, FRAGMENT_README, FragmentError, assemble, parse_fragments};
use crate::gates::{CheckError, Gates, QueueError};
use crate::git::{Git, GitCommand, GitError};
use crate::github::{ApiError, GitHub, Method, Repository, escape};

/// The remote the release reads and the workflow pushes to.
pub const REMOTE: &str = "origin";
/// The branch releases are made from.
pub const DEFAULT_BRANCH: &str = "main";
/// The remote-tracking ref of [`DEFAULT_BRANCH`].
pub const BASE: &str = "origin/main";
/// The scratch index the release commit is built in, beside the repository's own.
const INDEX: &str = "togi-release-index";

/// Whether [`Runner::release`] only prints the release or makes its commit, once the gates pass.
pub enum Mode<'a> {
    Preview,
    Commit(&'a mut dyn Gates),
}

/// Why the release was not previewed or committed.
#[derive(Debug, thiserror::Error)]
pub enum ReleaseError {
    #[error("fetch {DEFAULT_BRANCH}")]
    Fetch(#[source] GitError),
    #[error("resolve {BASE}")]
    Resolve(#[source] GitError),
    #[error("read {path} from {BASE}")]
    Read {
        path: &'static str,
        #[source]
        source: GitError,
    },
    #[error("read version.txt: invalid semantic version {0:?}")]
    Version(String),
    #[error("read Cargo.toml")]
    Manifest(#[source] ManifestError),
    #[error("read {FRAGMENT_DIR}/ from {BASE}")]
    FragmentFiles(#[source] FragmentFilesError),
    #[error("read {FRAGMENT_DIR}/")]
    Fragments(#[source] FragmentError),
    #[error("list release tags")]
    Tags(#[source] GitError),
    #[error("nothing to release: {FRAGMENT_DIR}/ has no fragments")]
    Nothing,
    #[error("bump version")]
    Bump(#[source] BumpError),
    #[error("rewrite changelog")]
    Rewrite(#[source] RewriteError),
    #[error("refuse to release")]
    Queue(#[source] QueueError),
    #[error("require a passing check on {BASE}")]
    Check(#[source] CheckError),
    #[error("commit release {version}")]
    Commit {
        version: String,
        #[source]
        source: CommitError,
    },
    #[error("check out {commit}")]
    Checkout {
        commit: String,
        #[source]
        source: GitError,
    },
    #[error("print the result")]
    Output(#[source] io::Error),
}

/// Why the repository address could not be read from the workspace manifest.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("parse")]
    Parse(#[source] toml_edit::TomlError),
    #[error("missing [workspace.package] repository")]
    Missing,
}

/// Why the fragments could not be read from the base commit.
#[derive(Debug, thiserror::Error)]
pub enum FragmentFilesError {
    #[error("list")]
    List(#[source] GitError),
    #[error("read {name}")]
    Read {
        name: String,
        #[source]
        source: GitError,
    },
}

/// Why the release commit could not be built.
#[derive(Debug, thiserror::Error)]
pub enum CommitError {
    #[error("locate temporary index")]
    Locate(#[source] GitError),
    #[error("read parent tree")]
    ReadTree(#[source] GitError),
    #[error("store {path}")]
    Store {
        path: &'static str,
        #[source]
        source: GitError,
    },
    #[error("stage {path}")]
    Stage {
        path: &'static str,
        #[source]
        source: GitError,
    },
    #[error("remove {path}")]
    Remove {
        path: String,
        #[source]
        source: GitError,
    },
    #[error("write tree")]
    WriteTree(#[source] GitError),
    #[error("create commit")]
    Create(#[source] GitError),
}

/// Why the release at `HEAD` was not published.
#[derive(Debug, thiserror::Error)]
pub enum PublishError {
    #[error("read {path}")]
    Read {
        path: &'static str,
        #[source]
        source: GitError,
    },
    #[error("read version.txt: invalid semantic version {0:?}")]
    Version(String),
    #[error("check release history")]
    History(#[source] GitError),
    #[error("fetch complete release history")]
    Unshallow(#[source] GitError),
    #[error("find the commit that set version.txt to {version}")]
    FindCommit {
        version: String,
        #[source]
        source: GitError,
    },
    #[error("find the commit that set version.txt to {version}: no commit changed version.txt")]
    NoCommit { version: String },
    #[error("check tag {tag}")]
    Tag {
        tag: String,
        #[source]
        source: GitError,
    },
    #[error("tag {tag} points at {target}, but the release commit is {commit}")]
    TagElsewhere {
        tag: String,
        target: String,
        commit: String,
    },
    #[error("check release {tag}")]
    CheckRelease {
        tag: String,
        #[source]
        source: ApiError,
    },
    #[error("create release {tag}")]
    Create {
        tag: String,
        #[source]
        source: ApiError,
    },
    #[error("print the result")]
    Output(#[source] io::Error),
}

/// Makes and publishes releases in one repository.
pub struct Runner<'a> {
    pub git: &'a mut dyn Git,
    /// The moment the release is dated by, in UTC.
    pub today: Timestamp,
    pub out: &'a mut dyn Write,
}

impl Runner<'_> {
    fn output(&mut self, args: &[&str]) -> Result<String, GitError> {
        self.git.run(GitCommand::new(args))
    }

    /// Fetches `origin/main` and computes the next release from the version, changelog, repository address and
    /// fragments of that exact commit, never the checkout's. A preview prints it and changes nothing but the
    /// remote-tracking ref. A commit passes the gates, builds the release commit on top of that commit in a
    /// scratch index and checks it out detached; the workflow pushes it. With no fragments, a dated version
    /// without its tag (or any dated version, when committing) is a release to publish again: the commit mode
    /// checks out `origin/main` itself.
    pub fn release(&mut self, mode: Mode<'_>) -> Result<(), ReleaseError> {
        self.output(&["fetch", "--quiet", REMOTE, DEFAULT_BRANCH])
            .map_err(ReleaseError::Fetch)?;
        let base_commit = self
            .output(&["rev-parse", "--verify", &format!("{BASE}^{{commit}}")])
            .map_err(ReleaseError::Resolve)?
            .trim()
            .to_owned();
        let mut read = |path: &'static str| {
            self.output(&["show", &format!("{base_commit}:{path}")])
                .map_err(|source| ReleaseError::Read { path, source })
        };
        let version = read("version.txt")?.trim().to_owned();
        let changelog = read("CHANGELOG.md")?;
        let manifest = read("Cargo.toml")?;
        if !is_version(&version) {
            return Err(ReleaseError::Version(version));
        }
        let web_url = repository_url(&manifest).map_err(ReleaseError::Manifest)?;
        let files = self
            .fragment_files(&base_commit)
            .map_err(ReleaseError::FragmentFiles)?;
        let fragments = parse_fragments(&files).map_err(ReleaseError::Fragments)?;
        let tags = self
            .output(&["ls-remote", "--tags", REMOTE, "refs/tags/v*"])
            .map_err(ReleaseError::Tags)?;
        if fragments.is_empty() {
            let committing = matches!(mode, Mode::Commit(_));
            let dated = section_named(&changelog, &version).is_some_and(|s| s.date.is_some());
            if dated && (!has_tag(&tags, &format!("v{version}")) || committing) {
                writeln!(
                    self.out,
                    "{version} is released in CHANGELOG.md; would publish {BASE} if its GitHub Release is missing"
                )
                .map_err(ReleaseError::Output)?;
                if committing {
                    return self.checkout(&base_commit);
                }
                return Ok(());
            }
            if committing {
                return Err(ReleaseError::Nothing);
            }
            writeln!(self.out, "nothing to release").map_err(ReleaseError::Output)?;
            return Ok(());
        }
        let (body, prs) = assemble(&fragments);
        let (next, reason) = bump(&version, &body).map_err(ReleaseError::Bump)?;
        let updated = rewrite_changelog(&changelog, &next, &body, &prs, &web_url, self.today)
            .map_err(ReleaseError::Rewrite)?;
        let message = format!("Release {next}\n\n{reason}\n");
        let gates = match mode {
            Mode::Preview => {
                let notes = section_named(&updated, &next)
                    .map(|section| release_notes(&updated, section))
                    .unwrap_or_default();
                write!(
                    self.out,
                    "Would release from {BASE}:\n\n{message}\n{notes}\n"
                )
                .map_err(ReleaseError::Output)?;
                return Ok(());
            }
            Mode::Commit(gates) => gates,
        };
        gates
            .require_no_hardware_wait()
            .map_err(ReleaseError::Queue)?;
        gates
            .require_green(&base_commit, self.out)
            .map_err(ReleaseError::Check)?;
        let consumed: Vec<String> = files
            .keys()
            .filter(|name| *name != FRAGMENT_README)
            .map(|name| format!("{FRAGMENT_DIR}/{name}"))
            .collect();
        let commit = self
            .commit_files(
                &base_commit,
                &message,
                &format!("{next}\n"),
                &updated,
                &consumed,
            )
            .map_err(|source| ReleaseError::Commit {
                version: next.clone(),
                source,
            })?;
        writeln!(
            self.out,
            "Release {next} committed as {commit} on top of {BASE}"
        )
        .map_err(ReleaseError::Output)?;
        self.checkout(&commit)
    }

    fn fragment_files(
        &mut self,
        commit: &str,
    ) -> Result<BTreeMap<String, String>, FragmentFilesError> {
        let listing = self
            .output(&[
                "ls-tree",
                "--name-only",
                commit,
                &format!("{FRAGMENT_DIR}/"),
            ])
            .map_err(FragmentFilesError::List)?;
        let mut files = BTreeMap::new();
        for path in listing.split_terminator('\n') {
            let name = path
                .strip_prefix(&format!("{FRAGMENT_DIR}/"))
                .unwrap_or(path)
                .to_owned();
            if name == FRAGMENT_README {
                files.insert(name, String::new());
                continue;
            }
            match self.output(&["show", &format!("{commit}:{path}")]) {
                Ok(text) => files.insert(name, text),
                Err(source) => return Err(FragmentFilesError::Read { name, source }),
            };
        }
        Ok(files)
    }

    fn checkout(&mut self, commit: &str) -> Result<(), ReleaseError> {
        self.output(&["checkout", "--quiet", "--detach", commit])
            .map_err(|source| ReleaseError::Checkout {
                commit: commit.to_owned(),
                source,
            })?;
        Ok(())
    }

    /// A commit on `parent` with `message` that sets `version.txt` and `CHANGELOG.md` and deletes `remove`, built
    /// in a scratch index so the checkout's index and worktree stay as they are.
    fn commit_files(
        &mut self,
        parent: &str,
        message: &str,
        version: &str,
        changelog: &str,
        remove: &[String],
    ) -> Result<String, CommitError> {
        let index = self
            .output(&["rev-parse", "--git-path", INDEX])
            .map_err(CommitError::Locate)?
            .trim()
            .to_owned();
        let commit = self.write_commit(&index, parent, message, version, changelog, remove);
        self.git.remove_scratch(&index);
        commit
    }

    fn write_commit(
        &mut self,
        index: &str,
        parent: &str,
        message: &str,
        version: &str,
        changelog: &str,
        remove: &[String],
    ) -> Result<String, CommitError> {
        let env = [("GIT_INDEX_FILE", index)];
        let mut indexed = |stdin: &str, args: &[&str]| {
            self.git
                .run(GitCommand {
                    args,
                    env: &env,
                    stdin,
                })
                .map(|out| out.trim().to_owned())
        };
        indexed("", &["read-tree", parent]).map_err(CommitError::ReadTree)?;
        for (path, content) in [("version.txt", version), ("CHANGELOG.md", changelog)] {
            let blob = indexed(content, &["hash-object", "-w", "--stdin"])
                .map_err(|source| CommitError::Store { path, source })?;
            indexed(
                "",
                &[
                    "update-index",
                    "--cacheinfo",
                    &format!("100644,{blob},{path}"),
                ],
            )
            .map_err(|source| CommitError::Stage { path, source })?;
        }
        for path in remove {
            indexed("", &["update-index", "--force-remove", path]).map_err(|source| {
                CommitError::Remove {
                    path: path.clone(),
                    source,
                }
            })?;
        }
        let tree = indexed("", &["write-tree"]).map_err(CommitError::WriteTree)?;
        indexed(message, &["commit-tree", &tree, "-p", parent, "-F", "-"])
            .map_err(CommitError::Create)
    }

    /// Publishes the version `version.txt` names at `HEAD` as a GitHub Release, if its changelog section is dated
    /// and the Release does not exist yet, targeting the first-parent commit that set that version. A tag on that
    /// commit, lightweight or annotated, is accepted; a tag on another commit is refused.
    pub fn publish(
        &mut self,
        api: &mut GitHub,
        repository: &Repository,
    ) -> Result<(), PublishError> {
        let mut read = |path: &'static str| {
            self.output(&["show", &format!("HEAD:{path}")])
                .map_err(|source| PublishError::Read { path, source })
        };
        let version = read("version.txt")?.trim().to_owned();
        let changelog = read("CHANGELOG.md")?;
        if !is_version(&version) {
            return Err(PublishError::Version(version));
        }
        let Some(section) = section_named(&changelog, &version).filter(|s| s.date.is_some()) else {
            writeln!(
                self.out,
                "nothing to publish: CHANGELOG.md has no released [{version}] section"
            )
            .map_err(PublishError::Output)?;
            return Ok(());
        };
        self.complete_history()?;
        let commit = self
            .output(&[
                "log",
                "--first-parent",
                "-1",
                "--format=%H",
                "HEAD",
                "--",
                "version.txt",
            ])
            .map_err(|source| PublishError::FindCommit {
                version: version.clone(),
                source,
            })?
            .trim()
            .to_owned();
        if commit.is_empty() {
            return Err(PublishError::NoCommit { version });
        }
        let tag = format!("v{version}");
        self.check_tag(&tag, &commit)?;
        let prefix = repository.path();
        let (status, _) = api
            .request(
                Method::Get,
                &format!("{prefix}/releases/tags/{}", escape(&tag)),
                None,
                &[404],
            )
            .map_err(|source| PublishError::CheckRelease {
                tag: tag.clone(),
                source,
            })?;
        if status != 404 {
            writeln!(
                self.out,
                "nothing to publish: GitHub Release {tag} already exists"
            )
            .map_err(PublishError::Output)?;
            return Ok(());
        }
        let body = serde_json::json!({
            "tag_name": tag,
            "target_commitish": commit,
            "name": version,
            "body": release_notes(&changelog, section),
        });
        let created: Created = api
            .decoded(Method::Post, &format!("{prefix}/releases"), Some(&body))
            .map_err(|source| PublishError::Create {
                tag: tag.clone(),
                source,
            })?;
        writeln!(self.out, "{}", created.html_url).map_err(PublishError::Output)
    }

    fn complete_history(&mut self) -> Result<(), PublishError> {
        let shallow = self
            .output(&["rev-parse", "--is-shallow-repository"])
            .map_err(PublishError::History)?;
        if shallow.trim() == "true" {
            self.output(&[
                "fetch",
                "--quiet",
                "--unshallow",
                "--no-tags",
                REMOTE,
                DEFAULT_BRANCH,
            ])
            .map_err(PublishError::Unshallow)?;
        }
        Ok(())
    }

    fn check_tag(&mut self, tag: &str, commit: &str) -> Result<(), PublishError> {
        let reference = format!("refs/tags/{tag}");
        let peeled = format!("{reference}^{{}}");
        let tags = self
            .output(&["ls-remote", "--tags", REMOTE, &reference, &peeled])
            .map_err(|source| PublishError::Tag {
                tag: tag.to_owned(),
                source,
            })?;
        let mut target = None;
        for (sha, name) in tags.split('\n').filter_map(|line| line.split_once('\t')) {
            if name == peeled {
                target = Some(sha);
                break;
            }
            if name == reference {
                target = Some(sha);
            }
        }
        match target {
            Some(target) if !target.is_empty() && target != commit => {
                Err(PublishError::TagElsewhere {
                    tag: tag.to_owned(),
                    target: target.to_owned(),
                    commit: commit.to_owned(),
                })
            }
            Some(_) | None => Ok(()),
        }
    }
}

#[derive(Deserialize)]
struct Created {
    html_url: String,
}

/// The repository's web address, `[workspace.package].repository` of the workspace manifest.
fn repository_url(manifest: &str) -> Result<String, ManifestError> {
    let manifest = toml_edit::Document::parse(manifest).map_err(ManifestError::Parse)?;
    manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("repository"))
        .and_then(toml_edit::Item::as_str)
        .map(ToOwned::to_owned)
        .ok_or(ManifestError::Missing)
}

fn has_tag(ls_remote: &str, tag: &str) -> bool {
    let wanted = format!("refs/tags/{tag}");
    ls_remote
        .split('\n')
        .filter_map(|line| line.split_once('\t'))
        .any(|(_, name)| name == wanted)
}

#[cfg(test)]
#[path = "tests/release.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/release_process.rs"]
mod process_tests;
