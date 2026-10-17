use std::path::{Component, Path, PathBuf};
use std::process::Command;

use crate::error::{Error, GitError};

/// The tree a bench ran from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tree {
    /// The short revision of `HEAD`.
    pub commit: String,
    /// Whether any file differs from `HEAD`, an untracked one included, other than the records files `outs`.
    pub dirty: bool,
}

/// Reads the working tree's revision and dirtiness from git, at run time: a bench has no build-time revision of
/// the tree it measures. `outs`, the files the run is about to write, do not make the tree dirty.
pub fn inspect(outs: &[&Path]) -> Result<Tree, Error> {
    let commit = text(&git(&["rev-parse", "--short", "HEAD"])?);
    let root = PathBuf::from(text(&git(&["rev-parse", "--show-toplevel"])?));
    let outs = outs
        .iter()
        .map(|path| std::path::absolute(path).map(|path| clean(&path)))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| Error::Git {
            command: "status".to_owned(),
            source: GitError::Resolve(error),
        })?;
    let status = git(&["status", "--porcelain", "-z", "--untracked-files=all"])?;
    Ok(Tree {
        commit,
        dirty: dirty(&status, &root, &outs),
    })
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

/// `path` without its `.` and `..` components, as `Path` equality compares components and resolves neither.
fn clean(path: &Path) -> PathBuf {
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                clean.pop();
            }
            other @ (Component::Prefix(_) | Component::RootDir | Component::Normal(_)) => {
                clean.push(other);
            }
        }
    }
    clean
}

#[expect(
    clippy::disallowed_methods,
    reason = "the bench reads its tree's revision and state from git at run time"
)]
fn git(args: &[&str]) -> Result<Vec<u8>, Error> {
    let failed = |source| Error::Git {
        command: args.join(" "),
        source,
    };
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|error| failed(GitError::Spawn(error)))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(failed(GitError::Failed(text(&output.stderr))))
    }
}

/// Whether `git status --porcelain -z` names any path other than `outs`. A rename or copy is followed by its
/// source path as one more entry.
fn dirty(status: &[u8], root: &Path, outs: &[PathBuf]) -> bool {
    let mut entries = status.split(|&byte| byte == 0);
    while let Some(entry) = entries.next() {
        let Some(path) = entry.get(3..).filter(|path| !path.is_empty()) else {
            continue;
        };
        if !listed(outs, root, path) {
            return true;
        }
        if entry.iter().take(2).any(|code| matches!(code, b'R' | b'C'))
            && let Some(source) = entries.next()
            && !listed(outs, root, source)
        {
            return true;
        }
    }
    false
}

fn listed(outs: &[PathBuf], root: &Path, path: &[u8]) -> bool {
    let path = root.join(String::from_utf8_lossy(path).as_ref());
    outs.contains(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/repo";

    fn dirty_at(status: &[u8], out: Option<&str>) -> bool {
        let outs: Vec<PathBuf> = out.map(PathBuf::from).into_iter().collect();
        dirty(status, Path::new(ROOT), &outs)
    }

    #[test]
    fn a_clean_status_is_clean() {
        assert!(!dirty_at(b"", None));
    }

    #[test]
    fn any_changed_or_untracked_path_is_dirty() {
        assert!(dirty_at(b" M src/lib.rs\0", None));
        assert!(dirty_at(b"?? new.txt\0", Some("/repo/out.jsonl")));
    }

    #[test]
    fn the_records_file_alone_does_not_make_the_tree_dirty() {
        assert!(!dirty_at(b"?? out.jsonl\0", Some("/repo/out.jsonl")));
        assert!(dirty_at(
            b"?? out.jsonl\0 M a.rs\0",
            Some("/repo/out.jsonl")
        ));
    }

    #[test]
    fn each_of_several_output_files_is_set_aside() {
        let outs = [
            PathBuf::from("/repo/out.jsonl"),
            PathBuf::from("/repo/base.jsonl"),
        ];
        let root = Path::new(ROOT);
        assert!(!dirty(b"?? out.jsonl\0?? base.jsonl\0", root, &outs));
        assert!(dirty(b"?? base.jsonl\0?? other.jsonl\0", root, &outs));
    }

    #[test]
    fn a_rename_counts_its_source_path_too() {
        assert!(dirty_at(b"R  new.rs\0old.rs\0", Some("/repo/new.rs")));
        assert!(!dirty_at(
            b"R  out.jsonl\0out.jsonl\0",
            Some("/repo/out.jsonl")
        ));
    }

    #[test]
    fn dots_in_the_output_path_are_cleaned() {
        assert_eq!(
            clean(Path::new("/repo/sub/../out.jsonl")),
            Path::new("/repo/out.jsonl")
        );
        assert_eq!(
            clean(Path::new("/repo/./a/b/../../out.jsonl")),
            Path::new("/repo/out.jsonl")
        );
        assert_eq!(clean(Path::new("/../out.jsonl")), Path::new("/out.jsonl"));
    }
}
