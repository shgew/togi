//! `--same`: proves that two trees decide the same on every simulated session.
//!
//! Each tree's simulator is built, or given, and each tree's own suite is planned. Sessions pair by scenario, split
//! and seed. A session is reduced to the digest of its journals, the live one and the archived ones, with the build
//! stamp taken out ([`journal`]), and to its exit status. A tree whose simulator did not change has its sessions in
//! the cache ([`DigestCache`]), so only the side that changed runs; when both trees build the same simulator from
//! the same inputs, nothing runs at all. A difference stops the comparison at the first session that shows one,
//! unless every differing session is asked for, and is shown as the first differing journal line.

mod compare;
mod digests;
pub mod journal;

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::io;
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::cache::Costs;
use crate::error::{Error, RunError};
use crate::process::Fleet;
use crate::simulator::{self, Simulator};
use crate::suite::{RunSpec, Selection, Suite};

use compare::{Config, Key, Launcher, Ran, Side, remove_empty_dirs};
pub use compare::{Found, Outcome, Slow};
pub use digests::{DigestCache, SideRecord};
pub use journal::{Difference, JournalError};

/// What one `--same` comparison does.
#[derive(Clone, Debug)]
pub struct Options {
    /// The tree to compare against.
    pub base: PathBuf,
    /// The tree under test.
    pub head: PathBuf,
    /// The suite file; a relative path resolves in each tree.
    pub suite: PathBuf,
    /// Compares only the sessions the head's suite lists under `smoke`.
    pub smoke: bool,
    /// Reports every differing session instead of stopping at the first.
    pub keep_going: bool,
    /// Runs every session on both trees instead of using cached digests; the runs are still cached.
    pub no_cache: bool,
    /// Keeps every run directory, `base` and `head` apart, under a new directory in this one; keeping runs every
    /// session.
    pub keep: Option<PathBuf>,
    pub jobs: NonZeroUsize,
    pub timeout: Duration,
    pub max_boots: NonZeroU32,
    /// The cache directory: the digests, the session costs, the target directory of the builds and, without
    /// `keep`, the run directories.
    pub cache: PathBuf,
    /// A simulator to run for a tree instead of building it, base first.
    pub simulators: [Option<PathBuf>; 2],
}

/// Why a comparison could not be made.
#[derive(Debug, thiserror::Error)]
pub enum SameError {
    #[error("load the {side} suite in {}: {source}", tree.display())]
    Suite {
        side: &'static str,
        tree: PathBuf,
        #[source]
        source: Box<Error>,
    },
    #[error("suite has no smoke sessions")]
    NoSmoke,
    #[error("build the {side} simulator in {}: {source}", tree.display())]
    Build {
        side: &'static str,
        tree: PathBuf,
        #[source]
        source: Box<Error>,
    },
    #[error("find the {side} simulator: {source}")]
    Simulator {
        side: &'static str,
        #[source]
        source: Box<Error>,
    },
    #[error("{what} {}: {source}", path.display())]
    Io {
        what: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{side} {key}: {source}")]
    Run {
        side: &'static str,
        key: String,
        #[source]
        source: RunError,
    },
    #[error("{side} {key}: {source}")]
    Digest {
        side: &'static str,
        key: String,
        #[source]
        source: JournalError,
    },
    #[error("compare {key}: {source}")]
    Compare {
        key: String,
        #[source]
        source: JournalError,
    },
    #[error("compare {key}: rerun {side}: {source}")]
    Rerun {
        side: &'static str,
        key: String,
        #[source]
        source: RunError,
    },
    #[error("remove matching {side} {key}: {source}")]
    Remove {
        side: &'static str,
        key: String,
        #[source]
        source: io::Error,
    },
    #[error("interrupted")]
    Interrupted,
}

/// A comparison that failed, and the run directory it left for inspection, if any.
#[derive(Debug)]
pub struct Failed {
    pub error: Box<SameError>,
    pub kept: Option<PathBuf>,
}

impl fmt::Display for Failed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for Failed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// How long each phase took.
#[derive(Clone, Copy, Debug, Default)]
pub struct Timings {
    pub suites: Duration,
    pub builds: Duration,
    pub keys: Duration,
    pub sessions: Duration,
}

/// The result of a comparison.
#[derive(Debug)]
pub struct Report {
    pub outcome: Outcome,
    /// Whether both trees built the same simulator and read the same inputs, so no session ran.
    pub identical: bool,
    /// The run directory kept: all of it with `keep`, otherwise what a differing session left.
    pub kept: Option<PathBuf>,
    pub timings: Timings,
    pub warnings: Vec<String>,
}

impl Report {
    pub fn different(&self) -> bool {
        self.outcome.different()
    }

    pub fn write(&self, w: &mut impl io::Write) -> io::Result<()> {
        if self.identical {
            writeln!(
                w,
                "same: both trees build the same simulator and read the same inputs; 0 of {} sessions differ",
                self.outcome.sessions
            )
        } else {
            self.outcome.write(w)
        }
    }
}

/// Compares the sessions of the base and head trees.
pub fn execute(options: &Options, fleet: &Fleet) -> Result<Report, Failed> {
    let started = Instant::now();
    let fail = |error| Failed {
        error: Box::new(error),
        kept: None,
    };
    let trees = [&options.base, &options.head].map(|tree| {
        std::path::absolute(tree).map_err(|source| SameError::Io {
            what: "resolve the tree",
            path: tree.clone(),
            source,
        })
    });
    let [base, head] = trees;
    let trees = [base.map_err(fail)?, head.map_err(fail)?];
    let runs = load_runs(options, &trees).map_err(fail)?;
    let loaded = started.elapsed();
    let build_dir = tempfile::Builder::new()
        .prefix("togi-same-build-")
        .tempdir()
        .map_err(|source| SameError::Io {
            what: "create the build directory",
            path: std::env::temp_dir(),
            source,
        })
        .map_err(fail)?;
    let simulators = simulators(options, &trees, build_dir.path()).map_err(fail)?;
    let built = started.elapsed();
    let keys = [0, 1].map(|side| digests::key(simulators[side].digest(), options.max_boots.get()));
    let pairs = compare::pair(runs);
    let identical = keys[0] == keys[1]
        && pairs.iter().all(|pair| {
            pair.runs[0].is_some() && pair.runs[1].is_some() && pair.inputs[0] == pair.inputs[1]
        });
    let keyed = started.elapsed();
    let mut timings = Timings {
        suites: loaded,
        builds: built.saturating_sub(loaded),
        keys: keyed.saturating_sub(built),
        sessions: Duration::ZERO,
    };
    if identical && !options.no_cache && options.keep.is_none() {
        return Ok(Report {
            outcome: Outcome {
                sessions: pairs.len(),
                differences: Vec::new(),
                keep_going: options.keep_going,
                cached: [0; 2],
                scheduled: [0; 2],
                total_s: [0.0; 2],
                slow: Vec::new(),
                warnings: Vec::new(),
            },
            identical: true,
            kept: None,
            timings,
            warnings: Vec::new(),
        });
    }
    let cache_error = |what, source| SameError::Io {
        what,
        path: options.cache.clone(),
        source,
    };
    let caches = [0, 1].map(|side| {
        DigestCache::open(&options.cache, &keys[side])
            .map_err(|source| cache_error("open the session cache", source))
    });
    let [base_cache, head_cache] = caches;
    let caches = [base_cache.map_err(fail)?, head_cache.map_err(fail)?];
    let costs = Costs::load(options.cache.join("costs.json"));
    let parent = options
        .keep
        .clone()
        .unwrap_or_else(|| options.cache.join("runs"));
    let root = fs::create_dir_all(&parent)
        .and_then(|()| {
            tempfile::Builder::new()
                .prefix("togi-same-runs-")
                .tempdir_in(&parent)
                .map(tempfile::TempDir::keep)
        })
        .map_err(|source| SameError::Io {
            what: "create the run directory",
            path: parent.clone(),
            source,
        })
        .map_err(fail)?;
    let launcher = Real {
        simulators: &simulators,
        root: &root,
        fleet,
        timeout: options.timeout,
        max_boots: options.max_boots,
    };
    let config = Config {
        jobs: options.jobs.get(),
        keep: options.keep.is_some(),
        keep_going: options.keep_going,
        fresh: options.no_cache || options.keep.is_some(),
        caches: [Some(&caches[0]), Some(&caches[1])],
        costs: Some(&costs),
        scopes: [0, 1].map(|side| simulators[side].digest().to_owned()),
        fleet,
    };
    let result = compare::run(&pairs, &config, &launcher);
    let mut warnings = Vec::new();
    if let Err(error) = costs.save() {
        warnings.push(format!("save session costs: {error}"));
    }
    timings.sessions = started.elapsed().saturating_sub(keyed);
    let keep = !fleet.cancelled()
        && (options.keep.is_some() || result.as_ref().map_or(true, Outcome::different));
    let kept = if keep {
        remove_empty_dirs(&root);
        Some(root)
    } else {
        let _ = fs::remove_dir_all(&root);
        None
    };
    match result {
        Ok(outcome) => {
            warnings.extend(outcome.warnings.iter().cloned());
            Ok(Report {
                outcome,
                identical: false,
                kept,
                timings,
                warnings,
            })
        }
        Err(error) => Err(Failed {
            error: Box::new(error),
            kept,
        }),
    }
}

/// Plans each tree's suite, or with `smoke` only the sessions the head's suite lists under `smoke`.
fn load_runs(options: &Options, trees: &[PathBuf; 2]) -> Result<[Vec<RunSpec>; 2], SameError> {
    let mut runs = [Vec::new(), Vec::new()];
    for side in Side::BOTH {
        let tree = &trees[side.index()];
        let suite = if options.suite.is_absolute() {
            options.suite.clone()
        } else {
            tree.join(&options.suite)
        };
        runs[side.index()] = Suite::load(&suite)
            .map_err(Error::from)
            .and_then(|suite| suite.plan(Selection::All))
            .map_err(|source| SameError::Suite {
                side: side.name(),
                tree: tree.clone(),
                source: Box::new(source),
            })?
            .runs;
    }
    if options.smoke {
        let smoke: BTreeSet<Key> = runs[Side::Head.index()]
            .iter()
            .filter(|spec| spec.smoke)
            .map(Key::of)
            .collect();
        if smoke.is_empty() {
            return Err(SameError::NoSmoke);
        }
        for side in &mut runs {
            side.retain(|spec| smoke.contains(&Key::of(spec)));
        }
    }
    Ok(runs)
}

/// Each tree's simulator: the one given, or the one built from the tree. The head builds in its own target
/// directory, as any build of it does. The base is built in one under the cache directory that every comparison
/// reuses, so its dependencies stay built; cargo takes a source file older than its last build for unchanged,
/// and the base is a fresh export whose files keep the time of their commit, so each of its files is marked
/// modified now first, and only its own crates build again. A target directory is never shared between trees,
/// since one tree's build would be taken for the other's. Each binary is copied out of its target directory,
/// which a later build may replace.
fn simulators(
    options: &Options,
    trees: &[PathBuf; 2],
    build_dir: &Path,
) -> Result<[Simulator; 2], SameError> {
    let one = |side: Side| {
        let given = &options.simulators[side.index()];
        let binary = if let Some(binary) = given {
            binary.clone()
        } else {
            let tree = &trees[side.index()];
            let target = (side == Side::Base).then(|| options.cache.join("same-target"));
            if side == Side::Base {
                mark_modified(tree).map_err(|source| SameError::Io {
                    what: "mark the base tree modified",
                    path: tree.clone(),
                    source,
                })?;
            }
            let built = simulator::build(Some(tree), target.as_deref()).map_err(|source| {
                SameError::Build {
                    side: side.name(),
                    tree: tree.clone(),
                    source: Box::new(source),
                }
            })?;
            let copy = build_dir.join(side.name());
            fs::copy(&built, &copy).map_err(|source| SameError::Io {
                what: "copy the built simulator",
                path: built,
                source,
            })?;
            copy
        };
        Simulator::locate(Some(&binary)).map_err(|source| SameError::Simulator {
            side: side.name(),
            source: Box::new(source),
        })
    };
    Ok([one(Side::Base)?, one(Side::Head)?])
}

/// Marks every file under `dir` modified now, skipping the directories of version control and builds.
fn mark_modified(dir: &Path) -> io::Result<()> {
    let now = std::time::SystemTime::now();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            if entry.file_name() != ".git" && entry.file_name() != "target" {
                mark_modified(&path)?;
            }
        } else if kind.is_file() {
            fs::OpenOptions::new()
                .write(true)
                .open(&path)?
                .set_modified(now)?;
        }
    }
    Ok(())
}

struct Real<'a> {
    simulators: &'a [Simulator; 2],
    root: &'a Path,
    fleet: &'a Fleet,
    timeout: Duration,
    max_boots: NonZeroU32,
}

impl Launcher for Real<'_> {
    fn dir(&self, side: Side, spec: &RunSpec) -> PathBuf {
        self.root.join(side.name()).join(spec.name())
    }

    fn launch(&self, side: Side, spec: &RunSpec) -> Result<Ran, RunError> {
        self.simulators[side.index()]
            .launch(
                self.fleet,
                &self.root.join(side.name()),
                spec,
                self.max_boots,
                self.timeout,
            )
            .map(|launched| Ran {
                dir: launched.dir,
                exit: launched.exit,
                wall: launched.wall,
            })
    }
}

#[cfg(test)]
mod tests;
