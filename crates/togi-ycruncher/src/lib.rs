use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
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
        .map(|cpu| cpu.0.to_string())
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

static BINARY_NAME: LazyLock<Regex> = LazyLock::new(|| pattern(r"^[0-9]{2}-[A-Za-z0-9]+ ~ .+$"));
static SGR: LazyLock<Regex> = LazyLock::new(|| pattern(r"\x1b\[[0-9;]*m"));
static AFFINITY: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"(?i)Failed to set core affinity to core: *([0-9]+)"));
static COMPUTATION: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"(?i)Error\(s\) encountered|Coefficient is too large|Checksum mismatch|Running[\t\n\x0c\r ]+[^\t\n\x0c\r ]+:[\t\n\x0c\r ]*FAIL",
    )
});
static SETUP: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"(?i)InvalidParametersException|Invalid Parameter"));
static PROGRESS: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"(?i)Running[\t\n\x0c\r ]+([^\t\n\x0c\r ]+):[\t\n\x0c\r ]*Passed"));

#[expect(
    clippy::expect_used,
    reason = "the patterns are literals proven by every classify test"
)]
fn pattern(source: &str) -> Regex {
    Regex::new(source).expect("literal pattern compiles")
}

/// The lexically smallest binary name and the smallest Zen 5 name among `names`.
///
/// `read_dir` yields entries in no order, so the minimum is chosen explicitly.
fn select<'a>(names: impl IntoIterator<Item = &'a str>) -> (Option<&'a str>, Option<&'a str>) {
    let mut lowest: Option<&str> = None;
    let mut zen5: Option<&str> = None;
    for name in names.into_iter().filter(|name| BINARY_NAME.is_match(name)) {
        lowest = Some(lowest.map_or(name, |current| current.min(name)));
        if name.starts_with(ZEN5_PREFIX) {
            zen5 = Some(zen5.map_or(name, |current| current.min(name)));
        }
    }
    (lowest, zen5)
}

#[derive(Debug)]
struct Binaries {
    root: PathBuf,
    lowest: String,
    zen5: String,
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
}

impl Ycruncher {
    pub fn new(pkg: impl Into<PathBuf>) -> Self {
        Self { pkg: pkg.into() }
    }

    /// Both the lowest-ISA and the Zen 5 binary are required and checked, whichever one runs.
    fn discover(&self) -> Result<Binaries, Error> {
        let root = self.pkg.join(BINARIES);
        let read = |source: io::Error| {
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
        let mut names = Vec::new();
        for entry in fs::read_dir(&root).map_err(read)? {
            if let Ok(name) = entry.map_err(read)?.file_name().into_string() {
                names.push(name);
            }
        }
        let (lowest, zen5) = select(names.iter().map(String::as_str));
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
            check_executable(NAME, &binaries.path(isa))?;
        }
        Ok(binaries)
    }
}

impl Backend for Ycruncher {
    fn kind(&self) -> togi_machine::Backend {
        togi_machine::Backend::Ycruncher
    }

    fn check(&self) -> Result<Ready, Error> {
        let binaries = self.discover()?;
        Ok(Ready {
            detail: format!(
                "{} ({}, {})",
                self.pkg.display(),
                binaries.lowest,
                binaries.zen5
            ),
        })
    }

    fn plan(&self, workload: &Workload, dir: &Path, cpus: &[CpuId]) -> Result<Plan, Error> {
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
        let binaries = self.discover()?;
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

    fn classify(&self, line: &str) -> Line {
        let line = SGR.replace_all(line, "");
        if let Some(cpu) = AFFINITY.captures(&line).and_then(|c| c.get(1)) {
            Line::AffinityError(cpu.as_str().parse().ok().map(CpuId))
        } else if COMPUTATION.is_match(&line) {
            Line::ComputationError
        } else if SETUP.is_match(&line) {
            Line::SetupError
        } else if let Some(test) = PROGRESS.captures(&line).and_then(|c| c.get(1)) {
            Line::Progress(format!("{} passed", test.as_str()))
        } else {
            Line::Other
        }
    }
}

#[cfg(test)]
mod tests;
