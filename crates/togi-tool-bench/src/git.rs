use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::Error;

/// The tree a bench ran from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Tree {
    /// The short revision of `HEAD`.
    pub commit: String,
    /// Whether any file differs from `HEAD`, an untracked one included, other than the records file `out`.
    pub dirty: bool,
}

/// Reads the working tree's revision and dirtiness from git, at run time: a bench has no build-time revision of
/// the tree it measures. `out`, the file the run is about to write, does not make the tree dirty.
pub fn inspect(out: Option<&Path>) -> Result<Tree, Error> {
    let commit = text(&git(&["rev-parse", "--short", "HEAD"])?);
    let root = PathBuf::from(text(&git(&["rev-parse", "--show-toplevel"])?));
    let out = out
        .map(std::path::absolute)
        .transpose()
        .map_err(|error| Error::Git {
            command: "status".to_owned(),
            message: format!("resolve the output path: {error}"),
        })?;
    let status = git(&["status", "--porcelain", "-z", "--untracked-files=all"])?;
    Ok(Tree {
        commit,
        dirty: dirty(&status, &root, out.as_deref()),
    })
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_owned()
}

#[expect(
    clippy::disallowed_methods,
    reason = "the bench reads its tree's revision and state from git at run time"
)]
fn git(args: &[&str]) -> Result<Vec<u8>, Error> {
    let failed = |message: String| Error::Git {
        command: args.join(" "),
        message,
    };
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|error| failed(error.to_string()))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(failed(text(&output.stderr)))
    }
}

/// Whether `git status --porcelain -z` names any path other than `out`. A rename or copy is followed by its
/// source path as one more entry.
fn dirty(status: &[u8], root: &Path, out: Option<&Path>) -> bool {
    let mut entries = status.split(|&byte| byte == 0);
    while let Some(entry) = entries.next() {
        let Some(path) = entry.get(3..).filter(|path| !path.is_empty()) else {
            continue;
        };
        if out != Some(&root.join(String::from_utf8_lossy(path).as_ref())) {
            return true;
        }
        if entry.iter().take(2).any(|code| matches!(code, b'R' | b'C'))
            && let Some(source) = entries.next()
            && out != Some(&root.join(String::from_utf8_lossy(source).as_ref()))
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/repo";

    fn dirty_at(status: &[u8], out: Option<&str>) -> bool {
        dirty(status, Path::new(ROOT), out.map(Path::new))
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
    fn a_rename_counts_its_source_path_too() {
        assert!(dirty_at(b"R  new.rs\0old.rs\0", Some("/repo/new.rs")));
        assert!(!dirty_at(
            b"R  out.jsonl\0out.jsonl\0",
            Some("/repo/out.jsonl")
        ));
    }
}
