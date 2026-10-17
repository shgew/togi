//! The lines an LCOV trace measures and whether a test reached them.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};

use crate::Error;
use crate::diff::LineRange;

/// Per repository-relative path, each line the trace measures and whether a test reached it.
pub type Measured = HashMap<String, BTreeMap<u32, bool>>;

/// The lines the trace measures. A line listed more than once counts as reached when any listing
/// reached it. Files outside `root` are left out.
pub fn parse_lcov(trace: &str, root: &Path) -> Result<Measured, Error> {
    let mut files = Measured::new();
    let mut current: Option<String> = None;
    for line in trace.lines() {
        if let Some(source) = line.strip_prefix("SF:") {
            let source = Path::new(source);
            current = source
                .is_absolute()
                .then(|| relative(root, source))
                .flatten();
            if let Some(path) = &current {
                files.entry(path.clone()).or_default();
            }
        } else if let Some(record) = line.strip_prefix("DA:") {
            let Some(lines) = current.as_ref().and_then(|path| files.get_mut(path)) else {
                continue;
            };
            let malformed = || Error::MalformedLcov(line.to_owned());
            let mut fields = record.split(',');
            let (Some(number), Some(count)) = (fields.next(), fields.next()) else {
                return Err(malformed());
            };
            let number: u32 = number.parse().map_err(|_| malformed())?;
            let count: u64 = count.parse().map_err(|_| malformed())?;
            *lines.entry(number).or_default() |= count > 0;
        } else if line == "end_of_record" {
            current = None;
        }
    }
    Ok(files)
}

/// Each file's unreached lines joined into ranges; lines the trace does not measure, such as
/// blank lines and comments, do not split a range.
pub fn unreached(files: &Measured) -> HashMap<String, Vec<LineRange>> {
    let mut out: HashMap<String, Vec<LineRange>> = HashMap::new();
    for (path, lines) in files {
        let mut ranges: Vec<LineRange> = Vec::new();
        let mut open = false;
        for (&number, &reached) in lines {
            match (reached, open, ranges.last_mut()) {
                (true, _, _) => open = false,
                (false, true, Some(range)) => range.last = number,
                (false, _, _) => {
                    ranges.push(LineRange::new(number, number));
                    open = true;
                }
            }
        }
        if !ranges.is_empty() {
            out.insert(path.clone(), ranges);
        }
    }
    out
}

/// `path` relative to `root`, both cleaned lexically, with `/` separators, or `None` when it lies
/// outside `root` or is `root` itself. A relative `path` is taken as relative to `root`.
pub fn relative(root: &Path, path: &Path) -> Option<String> {
    let path = clean(&if path.is_absolute() {
        path.to_owned()
    } else {
        root.join(path)
    });
    let rest = path.strip_prefix(clean(root)).ok()?;
    let parts: Vec<_> = rest
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect();
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn clean(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() && !out.has_root() {
                    out.push(part);
                }
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => out.push(part),
        }
    }
    out
}

#[cfg(test)]
#[path = "tests/lcov.rs"]
mod tests;
