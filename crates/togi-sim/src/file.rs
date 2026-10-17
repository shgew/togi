//! Machine files on disk: loading with overlays, and encoding the materialized machine.
//!
//! A file that names a parent in `extends` is an overlay: its parent's materialized machine with only the
//! listed `shared_voltage_override` entries changed. Overlays carry only `description`, `notes`,
//! `extends` and `shared_voltage_override`; the result takes the overlay's own `description` and `notes`.

use serde::de::IgnoredAny;
use serde::{Deserialize, Deserializer};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::definition::{MachineDefinition, object};
use crate::error::InvalidMachine;
use crate::model::Model;

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("read machine file {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("parse machine file {path}: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("machine file {path}: invalid overlay: {problem}")]
    Overlay {
        path: PathBuf,
        problem: OverlayProblem,
    },
    #[error("machine file {path}: parent: {source}")]
    Parent { path: PathBuf, source: Box<Self> },
    #[error("machine file {path}: resolve directory {directory}: {source}")]
    Directory {
        path: PathBuf,
        directory: PathBuf,
        source: io::Error,
    },
    #[error("machine file {path}: {source}")]
    Invalid {
        path: PathBuf,
        source: InvalidMachine,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum OverlayProblem {
    #[error("extends is missing or empty")]
    EmptyExtends,
    #[error("extends {0} must be relative to the overlay's directory")]
    AbsoluteExtends(PathBuf),
    #[error("shared_voltage_override is missing or empty, so the overlay changes nothing")]
    EmptyOverride,
    #[error("extends cycle through {0}")]
    Cycle(PathBuf),
    #[error("parent {0} has no shared_voltage to override")]
    NoSharedVoltage(PathBuf),
    #[error("shared_voltage_override[{index}] needs core")]
    MissingCore { index: usize },
    #[error(
        "shared_voltage_override[{index}] sets neither threshold_v nor threshold_clock_v_per_100mhz"
    )]
    NoEffect { index: usize },
    #[error("shared_voltage_override[{index}] names unknown workload {workload:?}")]
    UnknownWorkload { index: usize, workload: String },
    #[error("shared_voltage_override[{index}] names unknown core {core} of workload {workload}")]
    UnknownCore {
        index: usize,
        workload: String,
        core: i64,
    },
    #[error("shared_voltage_override[{index}] repeats workload {workload} core {core}")]
    Repeated {
        index: usize,
        workload: String,
        core: usize,
    },
    #[error("facts {0:?} cannot be rebased to this directory")]
    Facts(String),
}

#[derive(Debug, thiserror::Error)]
pub enum EncodeError {
    #[error("encode simulator machine: {0}")]
    Invalid(#[from] InvalidMachine),
    #[error("encode simulator machine: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Decodes a full machine document, strictly. Values are not checked: see [`Model::new`].
pub fn decode(text: &str) -> Result<MachineDefinition, serde_json::Error> {
    serde_json::from_str(text)
}

/// Writes the materialized machine as pretty JSON ending in a newline, with maps sorted and every field
/// equal to its default left out. The machine must be valid, so what is written always loads again.
pub fn encode(machine: &MachineDefinition) -> Result<String, EncodeError> {
    Model::new(machine, 0)?;
    let mut text = serde_json::to_string_pretty(machine)?;
    text.push('\n');
    Ok(text)
}

/// The two path operations needed to materialize machine overlays.
pub trait Source {
    fn read_to_string(&self, path: &Path) -> io::Result<String>;
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;
}

struct Files;

impl Source for Files {
    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        fs::read_to_string(path)
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        fs::canonicalize(path)
    }
}

/// Loads and validates the machine at `path`, materializing overlays.
pub fn load(path: impl AsRef<Path>) -> Result<MachineDefinition, LoadError> {
    load_with_files(path).map(|(machine, _)| machine)
}

/// [`load`] that also returns every file the machine came from: `path` first, then each parent in turn.
/// The facts extract is not listed.
pub fn load_with_files(
    path: impl AsRef<Path>,
) -> Result<(MachineDefinition, Vec<PathBuf>), LoadError> {
    load_from(path, &Files)
}

/// Loads and validates through `source`, returning the leaf and its parents in load order.
pub fn load_from(
    path: impl AsRef<Path>,
    source: &impl Source,
) -> Result<(MachineDefinition, Vec<PathBuf>), LoadError> {
    let mut files = Vec::new();
    let machine = load_file(path.as_ref(), source, &mut files, &mut Vec::new())?;
    Ok((machine, files))
}

/// Whether a key is present at all, even with a null value.
fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    IgnoredAny::deserialize(deserializer).map(|_| true)
}

/// Reads only whether a document is an overlay; the root must be an object.
#[derive(Deserialize)]
#[serde(remote = "Self")]
struct Probe {
    #[serde(default, deserialize_with = "present")]
    extends: bool,
    #[serde(default, deserialize_with = "present")]
    shared_voltage_override: bool,
}

#[derive(Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
struct Overlay {
    #[serde(default)]
    description: String,
    #[serde(default)]
    notes: Vec<String>,
    extends: Option<String>,
    shared_voltage_override: Option<Vec<Override>>,
}

/// Replaces the required-voltage fields of one core of one workload; an absent field keeps the parent's.
#[derive(Deserialize)]
#[serde(remote = "Self", deny_unknown_fields)]
struct Override {
    #[serde(default)]
    workload: String,
    core: Option<i64>,
    threshold_v: Option<f64>,
    threshold_clock_v_per_100mhz: Option<f64>,
}

object!(Probe, Overlay, Override);

fn load_file(
    path: &Path,
    source: &impl Source,
    files: &mut Vec<PathBuf>,
    chain: &mut Vec<PathBuf>,
) -> Result<MachineDefinition, LoadError> {
    files.push(path.to_path_buf());
    let text = source
        .read_to_string(path)
        .map_err(|source| LoadError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    let parse = |source| LoadError::Parse {
        path: path.to_path_buf(),
        source,
    };
    let probe: Probe = serde_json::from_str(&text).map_err(parse)?;
    let machine = if probe.extends || probe.shared_voltage_override {
        load_overlay(path, &text, source, files, chain)?
    } else {
        decode(&text).map_err(parse)?
    };
    Model::new(&machine, 0).map_err(|source| LoadError::Invalid {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(machine)
}

fn load_overlay(
    path: &Path,
    text: &str,
    source: &impl Source,
    files: &mut Vec<PathBuf>,
    chain: &mut Vec<PathBuf>,
) -> Result<MachineDefinition, LoadError> {
    let fail = |problem| LoadError::Overlay {
        path: path.to_path_buf(),
        problem,
    };
    let overlay: Overlay = serde_json::from_str(text).map_err(|source| LoadError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    let extends = match overlay.extends.as_deref() {
        None | Some("") => return Err(fail(OverlayProblem::EmptyExtends)),
        Some(extends) if Path::new(extends).is_absolute() => {
            return Err(fail(OverlayProblem::AbsoluteExtends(extends.into())));
        }
        Some(extends) => extends,
    };
    let overrides = overlay
        .shared_voltage_override
        .filter(|overrides| !overrides.is_empty())
        .ok_or_else(|| fail(OverlayProblem::EmptyOverride))?;
    let identity = source
        .canonicalize(path)
        .map_err(|source| LoadError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    if chain.contains(&identity) {
        return Err(fail(OverlayProblem::Cycle(identity)));
    }
    let dir = path.parent().unwrap_or(Path::new(""));
    let parent_path = clean(&dir.join(extends));
    chain.push(identity);
    let parent = load_file(&parent_path, source, files, chain);
    chain.pop();
    let mut machine = parent.map_err(|source| LoadError::Parent {
        path: path.to_path_buf(),
        source: Box::new(source),
    })?;
    let voltage = machine
        .shared_voltage
        .as_mut()
        .ok_or_else(|| fail(OverlayProblem::NoSharedVoltage(parent_path.clone())))?;
    let mut seen = BTreeSet::new();
    for (index, change) in overrides.iter().enumerate() {
        let core = change
            .core
            .ok_or_else(|| fail(OverlayProblem::MissingCore { index }))?;
        if change.threshold_v.is_none() && change.threshold_clock_v_per_100mhz.is_none() {
            return Err(fail(OverlayProblem::NoEffect { index }));
        }
        let workload = voltage.workload.get_mut(&change.workload).ok_or_else(|| {
            fail(OverlayProblem::UnknownWorkload {
                index,
                workload: change.workload.clone(),
            })
        })?;
        let slot = usize::try_from(core)
            .ok()
            .filter(|&slot| slot < workload.core.len())
            .ok_or_else(|| {
                fail(OverlayProblem::UnknownCore {
                    index,
                    workload: change.workload.clone(),
                    core,
                })
            })?;
        if !seen.insert((change.workload.as_str(), slot)) {
            return Err(fail(OverlayProblem::Repeated {
                index,
                workload: change.workload.clone(),
                core: slot,
            }));
        }
        let target = &mut workload.core[slot];
        if let Some(threshold_v) = change.threshold_v {
            target.threshold_v = threshold_v;
        }
        if let Some(coefficient) = change.threshold_clock_v_per_100mhz {
            target.threshold_clock_v_per_100mhz = coefficient;
        }
    }
    if let Some(facts) = &machine.facts {
        let parent_dir = parent_path.parent().unwrap_or(Path::new(""));
        if !facts.is_empty() && !Path::new(facts).is_absolute() {
            let canonical = |directory: &Path| {
                canonical_dir(source, directory).map_err(|source| LoadError::Directory {
                    path: path.to_path_buf(),
                    directory: directory.to_path_buf(),
                    source,
                })
            };
            let (from, to) = (canonical(dir)?, canonical(parent_dir)?);
            if from != to {
                machine.facts = Some(
                    rebase(&from, &to, Path::new(facts))
                        .and_then(|rebased| rebased.to_str().map(str::to_owned))
                        .ok_or_else(|| fail(OverlayProblem::Facts(facts.clone())))?,
                );
            }
        }
    }
    machine.description = overlay.description;
    machine.notes = overlay.notes;
    Ok(machine)
}

/// `directory` resolved through every symlink; the empty parent of a bare file name is the current directory.
fn canonical_dir(source: &impl Source, directory: &Path) -> io::Result<PathBuf> {
    source.canonicalize(if directory.as_os_str().is_empty() {
        Path::new(".")
    } else {
        directory
    })
}

/// `facts`, a path relative to canonical directory `to`, as a path relative to canonical directory `from`.
/// Only the climb between the two directories is simplified: it is made of real directories, while a segment of
/// `facts` itself may be a symlink, so `facts` is otherwise kept verbatim and never cleaned.
fn rebase(from: &Path, to: &Path, facts: &Path) -> Option<PathBuf> {
    let climb = relative(from, to)?;
    let mut parts: Vec<Component<'_>> = climb.components().collect();
    let mut rest = facts.components().peekable();
    while let Some(leading) =
        rest.next_if(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        if leading == Component::CurDir {
            continue;
        }
        if matches!(parts.last(), Some(Component::Normal(_))) {
            parts.pop();
        } else {
            parts.push(Component::ParentDir);
        }
    }
    parts.extend(rest.filter(|component| *component != Component::CurDir));
    Some(parts.into_iter().collect())
}

/// `path` with `.` and `..` resolved lexically, as a file system would not: symlinks are not followed.
fn clean(path: &Path) -> PathBuf {
    let mut parts: Vec<Component<'_>> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match parts.last() {
                Some(Component::Normal(_)) => {
                    parts.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => parts.push(component),
            },
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                parts.push(component);
            }
        }
    }
    parts.into_iter().collect()
}

/// The path from directory `from` to `to`, both cleaned and both absolute or both relative.
fn relative(from: &Path, to: &Path) -> Option<PathBuf> {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let shared = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for component in &from[shared..] {
        if !matches!(component, Component::Normal(_)) {
            return None;
        }
        out.push("..");
    }
    out.extend(&to[shared..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_resolves_dots_lexically() {
        assert_eq!(clean(Path::new("a/./b/../c")), Path::new("a/c"));
        assert_eq!(clean(Path::new("./../x")), Path::new("../x"));
        assert_eq!(clean(Path::new("/a/../../b")), Path::new("/b"));
        assert_eq!(clean(Path::new("sub/../a.json")), Path::new("a.json"));
        assert_eq!(clean(Path::new(".")), Path::new(""));
    }

    #[test]
    fn relative_climbs_and_descends() {
        let go = |from, to| relative(Path::new(from), Path::new(to));
        assert_eq!(go("/d/x/y", "/d/facts/e"), Some("../../facts/e".into()));
        assert_eq!(go("/d", "/d/machines/e"), Some("machines/e".into()));
        assert_eq!(go("a", "../x"), Some("../../x".into()));
        assert_eq!(go("..", "x"), None);
        assert_eq!(go("", "x/y"), Some("x/y".into()));
    }

    #[test]
    fn a_directory_that_cannot_be_resolved_is_reported_with_the_overlay_file_that_needed_it() {
        let missing = PathBuf::from("/missing/directory");
        let source = io::Error::from(io::ErrorKind::NotFound);
        let error = LoadError::Directory {
            path: PathBuf::from("overlays/leaf.json"),
            directory: missing.clone(),
            source,
        };
        let message = error.to_string();
        assert!(
            message.contains("overlays/leaf.json") && message.contains(&*missing.to_string_lossy()),
            "{message}"
        );
    }

    #[test]
    fn real_fs_the_empty_directory_of_a_bare_file_name_is_the_current_directory() {
        assert_eq!(Path::new("machine.json").parent(), Some(Path::new("")));
        assert_eq!(
            canonical_dir(&Files, Path::new("")).expect("the current directory"),
            fs::canonicalize(".").expect("the current directory")
        );
    }

    #[test]
    fn rebase_climbs_past_the_shared_directories_and_keeps_the_rest_verbatim() {
        let go = |from, to, facts| rebase(Path::new(from), Path::new(to), Path::new(facts));
        for (from, to, facts, want) in [
            (
                "/d/overlays",
                "/d/machines",
                "../../facts/extract.gz",
                "../../facts/extract.gz",
            ),
            ("/d/x/y", "/d/machines", "../facts/e", "../../facts/e"),
            ("/d/overlays", "/d/machines", "./../facts/e", "../facts/e"),
            ("/d", "/d/machines", "../facts/e", "facts/e"),
            ("/a", "/a/b", "../../../x", "../../x"),
            (
                "/d/overlays",
                "/d/machines",
                "alias/../x",
                "../machines/alias/../x",
            ),
        ] {
            let got = go(from, to, facts).expect("a rebased path");
            assert_eq!(got, Path::new(want), "{from} {to} {facts}");
            // Without symlinks, both name the same file.
            assert_eq!(
                clean(&Path::new(from).join(&got)),
                clean(&Path::new(to).join(facts)),
                "{from} {to} {facts}"
            );
        }
    }
}
