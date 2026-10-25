//! The changed files the coverage build leaves out in part or whole.

use std::collections::HashSet;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use crate::Error;
use crate::cfg::HostCfg;
use crate::diff::Changed;
use crate::lcov::{Measured, relative};
use crate::scan::cfg_out;
use crate::tree::Tree;

/// The repository-relative sources the dep-info files under a build profile directory name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Built {
    /// Every file a build into the directory compiled.
    pub sources: HashSet<String>,
    /// The files an integration-test target compiled: those its dep-info names.
    pub tested: HashSet<String>,
}

/// The sources the dep-info files under a build profile directory name. Crates and their tests
/// write dep-info into `deps`, build scripts into `build/<package>-<hash>`. Each source also has a
/// line of its own ending in a colon, which is what is read, the crate root first; rustc escapes a
/// space in its path as a backslash and a space. A dep-info whose crate root is
/// `<package>/tests/<name>.rs` or `<package>/tests/<name>/main.rs` is an integration test's.
pub fn built_sources(tree: &dyn Tree, build_dir: &Path, root: &Path) -> Result<Built, Error> {
    let dep_infos = |dir: &Path| {
        tree.list(dir)
            .unwrap_or_default()
            .into_iter()
            .filter(|name| Path::new(name).extension().is_some_and(|ext| ext == "d"))
            .map(|name| dir.join(name))
            .collect::<Vec<_>>()
    };
    let build = build_dir.join("build");
    let mut infos = dep_infos(&build_dir.join("deps"));
    for package in tree.list(&build).unwrap_or_default() {
        infos.extend(dep_infos(&build.join(package)));
    }
    if infos.is_empty() {
        return Err(Error::NoDepInfo(build_dir.to_owned()));
    }
    let mut built = Built::default();
    for info in infos {
        let data = tree.read(&info).map_err(|source| Error::ReadDepInfo {
            path: info.clone(),
            source,
        })?;
        let sources: Vec<String> = String::from_utf8_lossy(&data)
            .lines()
            .filter_map(|line| line.strip_suffix(':'))
            .filter_map(|source| relative(root, Path::new(&source.replace("\\ ", " "))))
            .collect();
        if sources
            .first()
            .is_some_and(|crate_root| is_integration_test_root(tree, root, crate_root))
        {
            built.tested.extend(sources.iter().cloned());
        }
        built.sources.extend(sources);
    }
    Ok(built)
}

/// Whether the repository-relative `path` is the crate root of an integration test:
/// `<package>/tests/<name>.rs` or `<package>/tests/<name>/main.rs` with a `Cargo.toml` in `<package>`.
fn is_integration_test_root(tree: &dyn Tree, root: &Path, path: &str) -> bool {
    let target = path.strip_suffix("/main.rs").unwrap_or(path);
    let Some((tests, _)) = target.rsplit_once('/') else {
        return false;
    };
    let package = match tests.strip_suffix("/tests") {
        Some(package) => package,
        None if tests == "tests" => "",
        None => return false,
    };
    tree.list(&root.join(package))
        .is_ok_and(|entries| entries.iter().any(|entry| entry == "Cargo.toml"))
}

/// The changed files, sorted, that the coverage build leaves out in part or whole: a file no
/// dep-info names, or one with a changed line inside an item whose cfg predicate is false or
/// unknown for the host and none of whose lines the trace measures. A measured line inside the
/// item shows some build compiled it, so the item is not claimed; this covers `cfg(not(test))` and
/// `cfg(feature = "...")` code with executable lines. `measured` holds the lines the trace lists
/// per file, reached or not. A file an integration test's dep-info names is judged with `test`
/// set: cargo compiles such a target with `--test` unless it sets `harness = false`, which no
/// workspace test does, and the trace leaves `tests/` files out.
pub fn not_built(
    tree: &dyn Tree,
    root: &Path,
    changed: &Changed,
    built: &Built,
    measured: &Measured,
    cfg: &HostCfg,
) -> Result<Vec<String>, Error> {
    let mut paths = Vec::new();
    for (path, changes) in changed {
        if !built.sources.contains(path) {
            paths.push(path.clone());
            continue;
        }
        let src = tree
            .read(&root.join(path))
            .map_err(|source| Error::ReadSource {
                path: path.clone(),
                source,
            })?;
        let lines: Vec<&[u8]> = src.split(|&b| b == b'\n').collect();
        let measured = measured.get(path);
        let test_cfg = built.tested.contains(path).then(|| cfg.with_test());
        for item in cfg_out(&src, test_cfg.as_ref().unwrap_or(cfg)) {
            if measured.is_some_and(|lines| lines.keys().any(|&line| item.contains(line))) {
                continue;
            }
            let text = String::from_utf8_lossy(&lines[item.indices()].join(&b'\n')).into_owned();
            if module_built(tree, root, path, &text, &built.sources)? {
                continue;
            }
            if changes.iter().any(|&change| change.overlaps(item)) {
                paths.push(path.clone());
                break;
            }
        }
    }
    Ok(paths)
}

/// An out-of-line module declaration with its leading attributes: `#[cfg(test)] pub(crate) mod tests;`.
static MODULE_DECLARATION: LazyLock<Result<Regex, regex::Error>> = LazyLock::new(|| {
    Regex::new(
        r"^((?:#\[[^\]]*\][\t\n\f\r ]*)*)(?:pub(?:\([^)]*\))?[\t\n\f\r ]+)?mod[\t\n\f\r ]+([0-9A-Za-z_]+)[\t\n\f\r ]*;$",
    )
});

/// Whether `item`, the text of an item in the source file `path`, declares an out-of-line module
/// whose file a dep-info names. The declaration holds no lines of the module, so the trace measures
/// the module in its own file. An indented declaration sits in an inline module and resolves
/// under that module's directory, so it proves nothing.
fn module_built(
    tree: &dyn Tree,
    root: &Path,
    path: &str,
    item: &str,
    built: &HashSet<String>,
) -> Result<bool, Error> {
    let pattern = MODULE_DECLARATION
        .as_ref()
        .map_err(|error| Error::Pattern(error.clone()))?;
    let Some(declaration) = pattern.captures(item.trim_end_matches([' ', '\t', '\r', '\n'])) else {
        return Ok(false);
    };
    let attributes = &declaration[1];
    if attributes
        .split("#[")
        .filter_map(|attribute| attribute.trim_end().strip_suffix(']'))
        .any(|attribute| {
            crate::cfg::path_attribute(attribute).is_none() && has_path_attribute(attribute)
        })
    {
        return Ok(false);
    }
    let plain_path = attributes
        .split("#[")
        .filter_map(|attribute| attribute.trim_end().strip_suffix(']'))
        .find_map(crate::cfg::path_attribute);
    let (dir, file) = path
        .rsplit_once('/')
        .map_or(("", path), |(dir, file)| (dir, file));
    let mut dir = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    if let Some(target) = plain_path {
        return Ok(relative(root, &root.join(&dir).join(target))
            .is_some_and(|target| built.contains(&target)));
    }
    if has_path_attribute(attributes) {
        return Ok(false);
    }
    let stem = file.strip_suffix(".rs").unwrap_or(file);
    let package = if file == "build.rs" {
        Some(dir.trim_end_matches('/'))
    } else {
        ["/tests/", "/benches/", "/examples/", "/src/bin/"]
            .into_iter()
            .find_map(|suffix| dir.strip_suffix(suffix))
    };
    let crate_root = package.is_some_and(|package| {
        tree.list(&root.join(package))
            .is_ok_and(|entries| entries.iter().any(|entry| entry == "Cargo.toml"))
    });
    if !matches!(stem, "lib" | "main" | "mod") && !crate_root {
        dir.push_str(stem);
        dir.push('/');
    }
    let name = &declaration[2];
    Ok(
        built.contains(&format!("{dir}{name}.rs"))
            || built.contains(&format!("{dir}{name}/mod.rs")),
    )
}

/// Whether `attributes` hold `path =` with `path` a whole word, as `#[path = "x.rs"]` or a
/// `cfg_attr(..., path = "x.rs")` does.
fn has_path_attribute(attributes: &str) -> bool {
    let word = |b: u8| b == b'_' || b.is_ascii_alphanumeric();
    attributes.match_indices("path").any(|(at, _)| {
        let bytes = attributes.as_bytes();
        let before = at.checked_sub(1).map(|i| bytes[i]);
        let after = &bytes[at + 4..];
        let rest = after
            .iter()
            .position(|&b| !matches!(b, b'\t' | b'\n' | b'\x0c' | b'\r' | b' '))
            .map(|i| after[i]);
        !before.is_some_and(word) && rest == Some(b'=')
    })
}

#[cfg(test)]
#[path = "tests/unbuilt.rs"]
mod tests;
