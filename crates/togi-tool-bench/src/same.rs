//! Compares two trees on a fixed set of scripted sessions.
//!
//! Both trees and their simulators must support the input contract. Comparison uses the live and
//! archived journals with build stamps removed. Cached records avoid rerunning an unchanged simulator.

mod compare;
mod digests;
pub mod journal;

use std::fmt;
use std::fs;
use std::io;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use togi_tool_sim::scripted::{CONTRACT_PATH, CONTRACT_VERSION, ScriptedCase};

use crate::cache::Costs;
pub use crate::error::ScriptedError;
use crate::error::{Error, RunError};
use crate::process::Fleet;
use crate::simulator::{self, Simulator};

use compare::{Config, Launcher, Ran, Side, remove_empty_dirs};
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
    /// Compares only the two scripted smoke cases.
    pub smoke: bool,
    /// Retired base-side kinds when comparing semantic rather than raw journals.
    /// Runs both sides freshly and reports every session; never uses raw digest equality.
    pub semantic: Option<Vec<String>>,
    /// Reports every differing session instead of stopping at the first.
    pub keep_going: bool,
    /// Runs every session on both trees instead of using cached digests; the runs are still cached.
    pub no_cache: bool,
    /// Keeps every run directory, `base` and `head` apart, under a new directory in this one; keeping runs every
    /// session.
    pub keep: Option<PathBuf>,
    pub jobs: NonZeroUsize,
    pub timeout: Duration,
    /// The cache directory: the digests, the session costs and the target directory of the base's build. The
    /// run directories live in the temporary directory, or under `keep`.
    pub cache: PathBuf,
    /// A simulator to run for a tree instead of building it, base first.
    pub simulators: [Option<PathBuf>; 2],
}

/// Why a comparison could not be made.
#[derive(Debug, thiserror::Error)]
pub enum SameError {
    #[error(
        "{side} simulator cannot run the scripted comparison set (requires scripted input contract v{CONTRACT_VERSION})"
    )]
    MissingContract { side: &'static str },
    #[error(
        "{side} simulator declares scripted input contract {found:?}; expected v{CONTRACT_VERSION}"
    )]
    WrongContract { side: &'static str, found: String },
    #[error("check scripted-contract for the {side} simulator {}", binary.display())]
    Probe {
        side: &'static str,
        binary: PathBuf,
        #[source]
        source: ScriptedError,
    },
    #[error("build the {side} simulator in {}", tree.display())]
    Build {
        side: &'static str,
        tree: PathBuf,
        #[source]
        source: Box<Error>,
    },
    #[error("find the {side} simulator")]
    Simulator {
        side: &'static str,
        #[source]
        source: Box<Error>,
    },
    #[error("{what} {}", path.display())]
    Io {
        what: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{side} {key}")]
    Run {
        side: &'static str,
        key: String,
        #[source]
        source: ScriptedError,
    },
    #[error("{side} {key}")]
    Digest {
        side: &'static str,
        key: String,
        #[source]
        source: JournalError,
    },
    #[error("compare {key}")]
    Compare {
        key: String,
        #[source]
        source: JournalError,
    },
    #[error("compare {key}: rerun {side}")]
    Rerun {
        side: &'static str,
        key: String,
        #[source]
        source: ScriptedError,
    },
    #[error("remove matching {side} {key}")]
    Remove {
        side: &'static str,
        key: String,
        #[source]
        source: io::Error,
    },
    #[error("interrupted")]
    Interrupted,
}

impl SameError {
    /// Whether the error is only a run stopped by an interrupt or by another session's halt.
    fn interrupted(&self) -> bool {
        matches!(
            self,
            Self::Interrupted
                | Self::Run {
                    source: ScriptedError::Run(RunError::Interrupted),
                    ..
                }
                | Self::Rerun {
                    source: ScriptedError::Run(RunError::Interrupted),
                    ..
                }
        )
    }
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

/// The failure itself: `Failed` only carries where its runs were kept, so its chain is the error's chain.
impl std::error::Error for Failed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}

/// How long each phase took.
#[derive(Clone, Copy, Debug, Default)]
pub struct Timings {
    pub contracts: Duration,
    pub builds: Duration,
    pub keys: Duration,
    pub sessions: Duration,
}

/// The result of a comparison.
#[derive(Debug)]
pub struct Report {
    pub outcome: Outcome,
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
        self.outcome.write(w)
    }
}

/// Compares the sessions of the base and head trees. `waiting` is told once, with the lock's path, when another
/// comparison holds the base's target directory and this one waits for it.
pub fn execute(
    options: &Options,
    fleet: &Fleet,
    waiting: &dyn Fn(&Path),
) -> Result<Report, Failed> {
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
    check_contracts(&trees).map_err(fail)?;
    if fleet.cancelled() {
        return Err(fail(SameError::Interrupted));
    }
    let loaded = started.elapsed();
    let simulators =
        simulators(options, &trees, fleet, &simulator::build_in, waiting).map_err(fail)?;
    if fleet.cancelled() {
        return Err(fail(SameError::Interrupted));
    }
    let built = started.elapsed();
    let keys = simulators
        .each_ref()
        .map(|sim| digests::key(sim.digest(), CONTRACT_VERSION));
    let cases: &[ScriptedCase] = if options.smoke {
        &ScriptedCase::SMOKE
    } else {
        &ScriptedCase::ALL
    };
    let keyed = started.elapsed();
    let mut timings = Timings {
        contracts: loaded,
        builds: built.saturating_sub(loaded),
        keys: keyed.saturating_sub(built),
        sessions: Duration::ZERO,
    };
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
    let parent = options.keep.clone().unwrap_or_else(std::env::temp_dir);
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
        semantic: options.semantic.as_deref(),
    };
    let config = Config {
        jobs: options.jobs.get(),
        keep: options.keep.is_some(),
        keep_going: options.keep_going || options.semantic.is_some(),
        fresh: options.no_cache || options.keep.is_some() || options.semantic.is_some(),
        caches: if options.semantic.is_some() {
            [None, None]
        } else {
            [Some(&caches[0]), Some(&caches[1])]
        },
        costs: Some(&costs),
        scopes: keys,
        fleet,
    };
    let result = compare::run(cases, &config, &launcher);
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

fn check_contracts(trees: &[PathBuf; 2]) -> Result<(), SameError> {
    for side in Side::BOTH {
        let path = trees[side.index()].join(CONTRACT_PATH);
        let text = fs::read_to_string(&path).map_err(|source| {
            if source.kind() == io::ErrorKind::NotFound {
                SameError::MissingContract { side: side.name() }
            } else {
                SameError::Io {
                    what: "read the scripted input contract at",
                    path,
                    source,
                }
            }
        })?;
        let found = text.trim();
        if found != CONTRACT_VERSION {
            return Err(SameError::WrongContract {
                side: side.name(),
                found: found.to_owned(),
            });
        }
    }
    Ok(())
}

/// How a tree's simulator is built: in the tree, with the target directory when given, stoppable by the fleet.
type Build<'a> = &'a dyn Fn(&Fleet, &Path, Option<&Path>) -> Result<PathBuf, Error>;

/// Told once that another comparison holds the lock at the path, before the wait for it.
type Waiting<'a> = &'a dyn Fn(&Path);

/// Each tree's simulator: the one given, or the one built from the tree. The head builds in its own target
/// directory, as any build of it does. The base is built in one under the cache directory that every comparison
/// reuses, so its dependencies stay built; cargo takes a source file older than its last build for unchanged,
/// and the base is a fresh export whose files keep the time of their commit, so each of its files is marked
/// modified now first, and only its own crates build again. A target directory is never shared between trees,
/// since one tree's build would be taken for the other's, nor between comparisons at once: a comparison holds
/// a lock on that directory from marking the files to copying the binary out of it. [`Simulator::locate`]
/// copies each binary out of its target directory, which a later build may replace. An interrupt before or
/// during a build is [`SameError::Interrupted`].
fn simulators(
    options: &Options,
    trees: &[PathBuf; 2],
    fleet: &Fleet,
    build: Build<'_>,
    waiting: Waiting<'_>,
) -> Result<[Simulator; 2], SameError> {
    let locate = |side: Side, binary: &Path| {
        Simulator::locate(Some(binary)).map_err(|source| SameError::Simulator {
            side: side.name(),
            source: Box::new(source),
        })
    };
    let probe = |side: Side, binary: &Path, sim: Simulator| {
        sim.check_scripted_contract(fleet, options.timeout)
            .map_err(|source| {
                if matches!(source, ScriptedError::Run(RunError::Interrupted)) {
                    SameError::Interrupted
                } else {
                    SameError::Probe {
                        side: side.name(),
                        binary: binary.to_path_buf(),
                        source,
                    }
                }
            })?;
        Ok(sim)
    };
    // Finish all supplied-binary checks before either tree can start a build.
    let supplied = |side: Side| {
        options.simulators[side.index()]
            .as_deref()
            .map(|binary| probe(side, binary, locate(side, binary)?))
            .transpose()
    };
    let base = supplied(Side::Base)?;
    let head = supplied(Side::Head)?;
    let one = |side: Side, supplied: Option<Simulator>| {
        if let Some(sim) = supplied {
            return Ok(sim);
        }
        if fleet.cancelled() {
            return Err(SameError::Interrupted);
        }
        let tree = &trees[side.index()];
        let built = |target: Option<&Path>| build_for(side, tree, fleet, build, target);
        if side == Side::Head {
            let binary = built(None)?;
            return probe(side, &binary, locate(side, &binary)?);
        }
        let target = std::path::absolute(options.cache.join("same-target")).map_err(|source| {
            SameError::Io {
                what: "resolve the target directory",
                path: options.cache.clone(),
                source,
            }
        })?;
        let held = lock_target(&options.cache, fleet, waiting)?;
        if fleet.cancelled() {
            return Err(SameError::Interrupted);
        }
        mark_modified(tree).map_err(|source| SameError::Io {
            what: "mark the base tree modified",
            path: tree.clone(),
            source,
        })?;
        let binary = built(Some(&target))?;
        let sim = locate(side, &binary)?;
        drop(held);
        probe(side, &binary, sim)
    };
    Ok([one(Side::Base, base)?, one(Side::Head, head)?])
}

fn build_for(
    side: Side,
    tree: &Path,
    fleet: &Fleet,
    build: Build<'_>,
    target: Option<&Path>,
) -> Result<PathBuf, SameError> {
    if fleet.cancelled() {
        return Err(SameError::Interrupted);
    }
    let result = build(fleet, tree, target);
    if fleet.cancelled() {
        return Err(SameError::Interrupted);
    }
    result.map_err(|source| SameError::Build {
        side: side.name(),
        tree: tree.to_path_buf(),
        source: Box::new(source),
    })
}

/// Takes the exclusive lock on the base's target directory, which a comparison holds until it drops the file.
/// While another comparison holds it, `waiting` is told once and the wait ends at an interrupt.
fn lock_target(cache: &Path, fleet: &Fleet, waiting: Waiting<'_>) -> Result<fs::File, SameError> {
    let path = cache.join("same-target.lock");
    let failed = |source| SameError::Io {
        what: "lock the target directory with",
        path: path.clone(),
        source,
    };
    fs::create_dir_all(cache).map_err(failed)?;
    let file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .map_err(failed)?;
    let mut told = false;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(fs::TryLockError::Error(source)) => return Err(failed(source)),
            Err(fs::TryLockError::WouldBlock) => {}
        }
        if !told {
            waiting(&path);
            told = true;
        }
        if fleet.cancelled() {
            return Err(SameError::Interrupted);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
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
    semantic: Option<&'a [String]>,
}

impl Launcher for Real<'_> {
    fn dir(&self, side: Side, case: ScriptedCase) -> PathBuf {
        self.root.join(side.name()).join(case.name())
    }

    fn launch(&self, side: Side, case: ScriptedCase) -> Result<Ran, ScriptedError> {
        let dir = self.dir(side, case);
        let finished =
            self.simulators[side.index()].launch_scripted(self.fleet, &dir, case, self.timeout)?;
        Ok(Ran {
            dir,
            exit: finished.exit,
            wall: finished.wall,
        })
    }

    fn semantic_comparison(
        &self,
        base: &Path,
        head: &Path,
    ) -> Result<Option<journal::semantic::Comparison>, JournalError> {
        if let Some(retired) = self.semantic {
            journal::semantic::compare_dirs(base, head, retired).map(Some)
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod build_tests {
    use std::cell::RefCell;
    use std::fs::TryLockError;
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;

    struct Fixture {
        dir: tempfile::TempDir,
        simulator: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            for tree in ["base", "head"] {
                let marker = dir.path().join(tree).join(CONTRACT_PATH);
                fs::create_dir_all(marker.parent().unwrap()).unwrap();
                fs::write(marker, CONTRACT_VERSION).unwrap();
            }
            let simulator = dir.path().join("sim");
            fs::write(
                &simulator,
                format!("#!/bin/sh\nprintf '%s\\n' '{CONTRACT_VERSION}'\n"),
            )
            .unwrap();
            fs::set_permissions(&simulator, fs::Permissions::from_mode(0o755)).unwrap();
            Self { dir, simulator }
        }

        fn cache(&self) -> PathBuf {
            self.dir.path().join("cache")
        }

        fn trees(&self) -> [PathBuf; 2] {
            ["base", "head"].map(|tree| self.dir.path().join(tree))
        }

        fn options(&self) -> Options {
            Options {
                base: self.dir.path().join("base"),
                head: self.dir.path().join("head"),
                smoke: false,
                semantic: None,
                keep_going: false,
                no_cache: false,
                keep: None,
                jobs: NonZeroUsize::MIN,
                timeout: Duration::from_secs(1),
                cache: self.cache(),
                simulators: [None, None],
            }
        }
    }

    #[test]
    fn real_process_the_base_builds_with_its_target_directory_locked_and_the_head_without() {
        let fixture = Fixture::new();
        let lock = fixture.cache().join("same-target.lock");
        let seen = RefCell::new(Vec::new());
        let build = |_: &Fleet, _: &Path, target: Option<&Path>| {
            let probe = fs::File::options().write(true).open(&lock).unwrap();
            let blocked = matches!(probe.try_lock(), Err(TryLockError::WouldBlock));
            seen.borrow_mut().push((target.is_some(), blocked));
            Ok(fixture.simulator.clone())
        };
        simulators(
            &fixture.options(),
            &fixture.trees(),
            &Fleet::new(),
            &build,
            &|_| {},
        )
        .unwrap();
        assert_eq!(*seen.borrow(), [(true, true), (false, false)]);
        let probe = fs::File::options().write(true).open(&lock).unwrap();
        assert!(probe.try_lock().is_ok(), "the lock outlived the build");
    }

    #[test]
    fn an_interrupt_during_the_build_is_an_interrupt_not_equality() {
        let simulator = PathBuf::from("/built/simulator");
        let fleet = Fleet::new();
        let build = |fleet: &Fleet, _: &Path, _: Option<&Path>| {
            fleet.cancel();
            Ok(simulator.clone())
        };
        let error = build_for(Side::Base, Path::new("/base"), &fleet, &build, None).unwrap_err();
        assert!(matches!(error, SameError::Interrupted), "{error}");
    }

    #[test]
    fn a_build_that_fails_after_an_interrupt_is_an_interrupt() {
        let fleet = Fleet::new();
        let build = |fleet: &Fleet, _: &Path, _: Option<&Path>| {
            fleet.cancel();
            Err(Error::BuildSimulator(crate::error::BuildFailure::Exit(1)))
        };
        let error = build_for(Side::Base, Path::new("/base"), &fleet, &build, None).unwrap_err();
        assert!(matches!(error, SameError::Interrupted), "{error}");
    }

    #[test]
    fn an_interrupt_before_the_builds_starts_none() {
        let fleet = Fleet::new();
        fleet.cancel();
        let build = |_: &Fleet, _: &Path, _: Option<&Path>| -> Result<PathBuf, Error> {
            panic!("built after an interrupt")
        };
        let error = build_for(Side::Base, Path::new("/base"), &fleet, &build, None).unwrap_err();
        assert!(matches!(error, SameError::Interrupted), "{error}");
    }

    #[test]
    fn an_interrupt_during_the_head_build_is_an_interrupt_not_equality() {
        let simulator = PathBuf::from("/built/simulator");
        let fleet = Fleet::new();
        let build = |fleet: &Fleet, _: &Path, target: Option<&Path>| {
            if target.is_none() {
                fleet.cancel();
            }
            Ok(simulator.clone())
        };
        build_for(
            Side::Base,
            Path::new("/base"),
            &fleet,
            &build,
            Some(Path::new("/target")),
        )
        .unwrap();
        let error = build_for(Side::Head, Path::new("/head"), &fleet, &build, None).unwrap_err();
        assert!(matches!(error, SameError::Interrupted), "{error}");
    }

    #[test]
    fn real_fs_a_comparison_waiting_for_the_target_lock_stops_at_an_interrupt() {
        let fixture = Fixture::new();
        let cache = fixture.cache();
        fs::create_dir_all(&cache).unwrap();
        let holder = fs::File::create(cache.join("same-target.lock")).unwrap();
        holder.lock().unwrap();
        let fleet = Fleet::new();
        fleet.cancel();
        let told = RefCell::new(Vec::new());
        let waiting = |path: &Path| told.borrow_mut().push(path.to_path_buf());
        let error = lock_target(&cache, &fleet, &waiting).unwrap_err();
        assert!(matches!(error, SameError::Interrupted), "{error}");
        assert_eq!(*told.borrow(), [cache.join("same-target.lock")]);
    }

    #[test]
    fn real_fs_a_free_target_lock_is_taken_without_a_notice() {
        let fixture = Fixture::new();
        let told = RefCell::new(0);
        let waiting = |_: &Path| *told.borrow_mut() += 1;
        let held = lock_target(&fixture.cache(), &Fleet::new(), &waiting).unwrap();
        assert_eq!(*told.borrow(), 0);
        drop(held);
    }
}
