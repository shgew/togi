//! The y-cruncher backend: [`Ycruncher`] selects a package binary, plans workload inputs and launch
//! arguments, and classifies progress and failure output through the backend contract.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use regex::Regex;
use regex::bytes::Regex as BytesRegex;
use togi_backend::{Backend, Error, FileName, Input, Line, Plan, Ready, check_executable};
use togi_machine::{CpuId, Workload};

const NAME: &str = "y-cruncher";
const BINARIES: &str = "lib/y-cruncher/Binaries";
const ZEN5_PREFIX: &str = "24-ZN5 ~ ";
const MEMORY_PER_CPU: u64 = 32 << 20;
const CONFIG: FileName = FileName::from_static("stress.cfg");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Isa {
    Lowest,
    Zen5,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Profile {
    tests: &'static [&'static str],
    isa: Isa,
}

impl Profile {
    fn of(base: &str) -> Option<Self> {
        let (tests, isa): (&'static [&'static str], Isa) = match base {
            "ycruncher-bkt-sftv4" => (&["BKT", "SFTv4"], Isa::Lowest),
            "ycruncher-snt-svt" => (&["SNT", "SVT"], Isa::Lowest),
            "ycruncher-fftv4-n63-vt3" => (&["FFTv4", "N63", "VT3"], Isa::Zen5),
            _ => return None,
        };
        Some(Self { tests, isa })
    }
}

fn config(cpus: &[CpuId], tests: &[&str]) -> String {
    let logical = cpus
        .iter()
        .map(|cpu| cpu.get().to_string())
        .collect::<Vec<_>>()
        .join(" ");
    let memory = MEMORY_PER_CPU * cpus.len() as u64;
    let tests: String = tests
        .iter()
        .map(|test| ["            \"", test, "\"\n"].concat())
        .collect();
    format!(
        "{{\n    Action : \"StressTest\"\n    StressTest : {{\n        AllocateLocally : \"true\"\n        LogicalCores : [{logical}]\n        TotalMemory : {memory}\n        SecondsPerTest : 30\n        SecondsTotal : 0\n        StopOnError : \"true\"\n        Tests : [\n{tests}        ]\n    }}\n}}\n"
    )
}

/// The output and file-name patterns, compiled once when the adapter is built.
#[derive(Clone, Debug)]
struct Patterns {
    /// Matched against raw name bytes: a binary name need not be UTF-8.
    binary_name: BytesRegex,
    sgr: Regex,
    affinity: Regex,
    computation: Regex,
    setup: Regex,
    progress: Regex,
}

fn pattern(source: &str) -> Result<Regex, Error> {
    Regex::new(source).map_err(|source| Error::Pattern { name: NAME, source })
}

impl Patterns {
    fn new() -> Result<Self, Error> {
        Ok(Self {
            // `(?-u)` lets `.` match any byte but `\n`, including invalid UTF-8 in file names.
            binary_name: BytesRegex::new(r"(?-u)^[0-9]{2}-[A-Za-z0-9]+ ~ .+$")
                .map_err(|source| Error::Pattern { name: NAME, source })?,
            sgr: pattern(r"\x1b\[[0-9;]*m")?,
            affinity: pattern(r"(?i)Failed to set core affinity to core: *([0-9]+)")?,
            computation: pattern(
                r"(?i)Error\(s\) encountered|Coefficient is too large|Checksum mismatch|Running[\t\n\x0c\r ]+[^\t\n\x0c\r ]+:[\t\n\x0c\r ]*FAIL",
            )?,
            setup: pattern(r"(?i)InvalidParametersException|Invalid Parameter")?,
            progress: pattern(r"(?i)Running[\t\n\x0c\r ]+([^\t\n\x0c\r ]+):[\t\n\x0c\r ]*Passed")?,
        })
    }
}

/// Whichever of `current` and `name` is bytewise smaller.
fn smaller<'a>(current: Option<&'a OsStr>, name: &'a OsStr) -> &'a OsStr {
    match current {
        Some(current) if current.as_bytes() <= name.as_bytes() => current,
        _ => name,
    }
}

/// The lexically smallest binary name and the smallest Zen 5 name among `names`.
///
/// `read_dir` yields entries in no order, so the minimum is chosen explicitly, bytewise.
fn select<'a>(
    binary_name: &BytesRegex,
    names: impl IntoIterator<Item = &'a OsStr>,
) -> (Option<&'a OsStr>, Option<&'a OsStr>) {
    let mut lowest: Option<&OsStr> = None;
    let mut zen5: Option<&OsStr> = None;
    for name in names
        .into_iter()
        .filter(|name| binary_name.is_match(name.as_bytes()))
    {
        lowest = Some(smaller(lowest, name));
        if name.as_bytes().starts_with(ZEN5_PREFIX.as_bytes()) {
            zen5 = Some(smaller(zen5, name));
        }
    }
    (lowest, zen5)
}

#[derive(Debug)]
struct Binaries {
    root: PathBuf,
    lowest: OsString,
    zen5: OsString,
}

impl Binaries {
    fn path(&self, isa: Isa) -> PathBuf {
        self.root.join(match isa {
            Isa::Lowest => &self.lowest,
            Isa::Zen5 => &self.zen5,
        })
    }
}

/// The y-cruncher package at `pkg`; its programs are in `pkg/lib/y-cruncher/Binaries`.
#[derive(Clone, Debug)]
pub struct Ycruncher {
    pkg: PathBuf,
    patterns: Patterns,
}

impl Ycruncher {
    /// # Errors
    ///
    /// [`Error::Pattern`] if a pattern does not compile.
    pub fn new(pkg: impl Into<PathBuf>) -> Result<Self, Error> {
        Ok(Self {
            pkg: pkg.into(),
            patterns: Patterns::new()?,
        })
    }

    /// Both the lowest-ISA and the Zen 5 binary are required and checked, whichever one runs.
    fn discover(&self) -> Result<Binaries, Error> {
        self.discover_with(
            |root| {
                fs::read_dir(root)?
                    .map(|entry| entry.map(|entry| entry.file_name()))
                    .collect()
            },
            check_executable,
        )
    }

    fn discover_with(
        &self,
        read: impl FnOnce(&Path) -> io::Result<Vec<OsString>>,
        mut check: impl FnMut(&'static str, &Path) -> Result<(), Error>,
    ) -> Result<Binaries, Error> {
        let root = self.pkg.join(BINARIES);
        let read_error = |source: io::Error| {
            let root = root.clone();
            if source.kind() == io::ErrorKind::NotFound {
                Error::BinariesMissing {
                    name: NAME,
                    root,
                    source,
                }
            } else {
                Error::ReadBinaries {
                    name: NAME,
                    root,
                    source,
                }
            }
        };
        let names = read(&root).map_err(read_error)?;
        let (lowest, zen5) = select(
            &self.patterns.binary_name,
            names.iter().map(OsString::as_os_str),
        );
        let lowest = lowest.ok_or_else(|| Error::NoBinaries {
            name: NAME,
            root: root.clone(),
        })?;
        let zen5 = zen5.ok_or_else(|| Error::NoZen5 { root: root.clone() })?;
        let binaries = Binaries {
            lowest: lowest.to_owned(),
            zen5: zen5.to_owned(),
            root,
        };
        for isa in [Isa::Lowest, Isa::Zen5] {
            check(NAME, &binaries.path(isa))?;
        }
        Ok(binaries)
    }

    fn check_with(
        &self,
        discover: impl FnOnce() -> Result<Binaries, Error>,
    ) -> Result<Ready, Error> {
        let binaries = discover()?;
        Ok(Ready {
            detail: format!(
                "{} ({}, {})",
                self.pkg.display(),
                Path::new(&binaries.lowest).display(),
                Path::new(&binaries.zen5).display()
            ),
        })
    }

    fn plan_with(
        &self,
        workload: &Workload,
        dir: &Path,
        cpus: &[CpuId],
        discover: impl FnOnce() -> Result<Binaries, Error>,
    ) -> Result<Plan, Error> {
        let backend = self.kind();
        let profile = Profile::of(workload.base).ok_or(Error::Unsupported {
            backend,
            workload: workload.id(),
        })?;
        if cpus.is_empty() {
            return Err(Error::NoCpus {
                backend,
                workload: workload.id(),
            });
        }
        let binaries = discover()?;
        Ok(Plan {
            program: binaries.path(profile.isa),
            args: vec![
                OsString::from("skip-warnings"),
                OsString::from("pause:-2"),
                OsString::from("status:none"),
                OsString::from("config"),
                dir.join(CONFIG.as_str()).into_os_string(),
            ],
            inputs: vec![Input {
                name: CONFIG,
                contents: config(cpus, profile.tests).into_bytes(),
            }],
            watched: Vec::new(),
        })
    }
}

impl Backend for Ycruncher {
    fn kind(&self) -> togi_machine::Backend {
        togi_machine::Backend::Ycruncher
    }

    fn check(&self) -> Result<Ready, Error> {
        self.check_with(|| self.discover())
    }

    fn plan(&self, workload: &Workload, dir: &Path, cpus: &[CpuId]) -> Result<Plan, Error> {
        self.plan_with(workload, dir, cpus, || self.discover())
    }

    fn classify(&self, line: &str) -> Line {
        let patterns = &self.patterns;
        let line = patterns.sgr.replace_all(line, "");
        if let Some(cpu) = patterns.affinity.captures(&line).and_then(|c| c.get(1)) {
            Line::AffinityError(cpu.as_str().parse::<u16>().ok().map(CpuId::from))
        } else if patterns.computation.is_match(&line) {
            Line::ComputationError
        } else if patterns.setup.is_match(&line) {
            Line::SetupError
        } else if let Some(test) = patterns.progress.captures(&line).and_then(|c| c.get(1)) {
            Line::Progress(format!("{} passed", test.as_str()))
        } else {
            Line::Other
        }
    }
}

#[cfg(test)]
mod tests;
