use std::path::{Component, Path, PathBuf};

use crate::error::Error;

fn absolute(path: &Path) -> Result<PathBuf, Error> {
    let path = std::path::absolute(path).map_err(|source| Error::Io {
        action: "resolve",
        path: path.to_path_buf(),
        source,
    })?;
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
    Ok(clean)
}

/// The path of the extract as a machine file written to `out_dir` declares it: relative to that directory,
/// with `/` separators.
pub(crate) fn facts_reference(out_dir: &Path, facts: &Path) -> Result<String, Error> {
    let out = absolute(out_dir)?;
    let facts = absolute(facts)?;
    let out: Vec<_> = out.components().collect();
    let target: Vec<_> = facts.components().collect();
    let shared = out.iter().zip(&target).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = Vec::new();
    for _ in shared..out.len() {
        parts.push("..".to_owned());
    }
    for component in &target[shared..] {
        let part = component.as_os_str();
        parts.push(
            part.to_str()
                .ok_or_else(|| Error::Path(facts.clone()))?
                .to_owned(),
        );
    }
    Ok(parts.join("/"))
}
