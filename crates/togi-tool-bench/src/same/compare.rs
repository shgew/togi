//! The comparison of every session of two trees: which sessions to run, in what order, how each pair is judged and
//! which run directories remain.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use crate::cache::{Costs, longest_first};
use crate::error::RunError;
use crate::pool;
use crate::process::Fleet;
use crate::suite::{RunSpec, Split};

use super::SameError;
use super::digests::{DigestCache, SideRecord, inputs};
use super::journal::{self, Difference};

/// A session of the head's own time beyond this multiple of its base's is reported, never judged.
const SLOW_RATIO: f64 = 2.0;
const SLOW_SHOWN: usize = 10;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Side {
    Base,
    Head,
}

impl Side {
    pub(crate) const BOTH: [Self; 2] = [Self::Base, Self::Head];

    pub(crate) const fn index(self) -> usize {
        match self {
            Self::Base => 0,
            Self::Head => 1,
        }
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Head => "head",
        }
    }
}

/// The identity of a session across the two trees: its scenario, split and seed.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Key {
    pub scenario: String,
    pub split: Split,
    pub seed: u64,
}

impl Key {
    pub(super) fn of(spec: &RunSpec) -> Self {
        Self {
            scenario: spec.scenario.clone(),
            split: spec.split,
            seed: spec.seed,
        }
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}-{}", self.scenario, self.split, self.seed)
    }
}

/// One session as each tree's suite defines it; a side without it has none.
#[derive(Clone, Debug)]
pub(crate) struct Pair {
    pub key: Key,
    pub runs: [Option<RunSpec>; 2],
    /// Each side's digest of what its simulator reads besides its binary, which a cached record must match.
    pub inputs: [String; 2],
}

/// Pairs the sessions of the base's and the head's runs by scenario, split and seed, in that order.
pub(crate) fn pair(runs: [Vec<RunSpec>; 2]) -> Vec<Pair> {
    let mut pairs: BTreeMap<Key, [Option<RunSpec>; 2]> = BTreeMap::new();
    for (side, specs) in runs.into_iter().enumerate() {
        for spec in specs {
            let key = Key::of(&spec);
            pairs.entry(key).or_default()[side] = Some(spec);
        }
    }
    pairs
        .into_iter()
        .map(|(key, runs)| {
            let inputs = [0, 1].map(|side| runs[side].as_ref().map(inputs).unwrap_or_default());
            Pair { key, runs, inputs }
        })
        .collect()
}

/// A finished simulator process and the directory it left.
#[derive(Debug)]
pub(crate) struct Ran {
    pub dir: PathBuf,
    pub exit: i32,
    pub wall: Duration,
}

/// Runs the sessions of the comparison.
pub(crate) trait Launcher: Sync {
    /// The directory a session of `side` runs in.
    fn dir(&self, side: Side, spec: &RunSpec) -> PathBuf;

    /// Runs `spec` on `side` to its end. A failure to run is an error; a session that ended badly is a result.
    fn launch(&self, side: Side, spec: &RunSpec) -> Result<Ran, RunError>;

    fn digest(&self, dir: &Path) -> Result<String, journal::JournalError> {
        journal::digest(dir)
    }

    fn difference(
        &self,
        base: &Path,
        head: &Path,
    ) -> Result<Option<Difference>, journal::JournalError> {
        journal::compare_dirs(base, head)
    }

    fn remove(&self, dir: &Path) -> io::Result<()> {
        fs::remove_dir_all(dir)
    }
}

pub(crate) struct Config<'a> {
    pub jobs: usize,
    /// Retains every run directory.
    pub keep: bool,
    /// Reports every differing session instead of stopping at the first.
    pub keep_going: bool,
    /// Ignores cached records; the runs still write them.
    pub fresh: bool,
    pub caches: [Option<&'a DigestCache>; 2],
    pub costs: Option<&'a Costs>,
    /// Names each side's simulator for the cost table.
    pub scopes: [String; 2],
    pub fleet: &'a Fleet,
}

/// A session whose two sides differ.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Found {
    pub key: Key,
    pub difference: Difference,
}

/// A session whose head took more than [`SLOW_RATIO`] times its base's time.
#[derive(Clone, Debug, PartialEq)]
pub struct Slow {
    pub key: Key,
    pub base_s: f64,
    pub head_s: f64,
}

/// What a comparison found.
#[derive(Debug)]
pub struct Outcome {
    pub sessions: usize,
    /// The differing sessions in session order: one unless `keep_going`.
    pub differences: Vec<Found>,
    pub keep_going: bool,
    /// The sessions of each side that had a cached record, and those that ran.
    pub cached: [usize; 2],
    pub scheduled: [usize; 2],
    /// The time of the compared sessions' simulators, summed by side.
    pub total_s: [f64; 2],
    /// The compared sessions whose head was slow, slowest first.
    pub slow: Vec<Slow>,
    pub warnings: Vec<String>,
}

impl Outcome {
    pub fn different(&self) -> bool {
        !self.differences.is_empty()
    }

    /// Writes the differences, the time of the compared sessions and the verdict.
    pub fn write(&self, w: &mut impl io::Write) -> io::Result<()> {
        for found in &self.differences {
            let difference = &found.difference;
            write!(w, "same: {}\n  {}", found.key, difference.file)?;
            if let Some(line) = difference.line {
                write!(w, ":{line}")?;
            }
            writeln!(
                w,
                "\n  base: {}\n  head: {}",
                difference.base, difference.head
            )?;
        }
        if self.total_s != [0.0, 0.0] {
            writeln!(
                w,
                "same: simulator time of the compared sessions: base {:.1}s, head {:.1}s",
                self.total_s[0], self.total_s[1]
            )?;
        }
        if !self.slow.is_empty() {
            writeln!(
                w,
                "same: {} sessions took more than {SLOW_RATIO}x their base's time (informational)",
                self.slow.len()
            )?;
            for slow in self.slow.iter().take(SLOW_SHOWN) {
                writeln!(
                    w,
                    "  {}: head {:.1}s, base {:.1}s, {:.1}x",
                    slow.key,
                    slow.head_s,
                    slow.base_s,
                    slow.head_s / slow.base_s
                )?;
            }
            if self.slow.len() > SLOW_SHOWN {
                writeln!(w, "  and {} more", self.slow.len() - SLOW_SHOWN)?;
            }
        }
        if self.different() && !self.keep_going {
            writeln!(
                w,
                "same: stopped at the first difference; --keep-going reports every differing session"
            )
        } else {
            writeln!(
                w,
                "same: {} of {} sessions differ",
                self.differences.len(),
                self.sessions
            )
        }
    }
}

/// What stops the pool, and the simulators still running with it: a difference found while stopping at the
/// first, or a failure. The runs the stop interrupts are not failures of their own.
enum Halt {
    Different,
    Failed(SameError),
}

impl From<SameError> for Halt {
    fn from(error: SameError) -> Self {
        Self::Failed(error)
    }
}

/// What is known of one session: each side's record, and the run directory of a side that ran and whose pair is
/// not yet compared.
#[derive(Default)]
struct PairState {
    rec: [Option<SideRecord>; 2],
    dirs: [Option<PathBuf>; 2],
}

impl PairState {
    fn complete(&self, pair: &Pair) -> bool {
        pair.runs
            .iter()
            .zip(&self.rec)
            .all(|(run, rec)| run.is_none() || rec.is_some())
    }
}

struct State {
    pairs: Vec<PairState>,
    found: Vec<(usize, Difference)>,
    total_s: [f64; 2],
    slow: Vec<usize>,
    warnings: Vec<String>,
}

struct Comparison<'a> {
    pairs: &'a [Pair],
    config: &'a Config<'a>,
    launcher: &'a dyn Launcher,
    state: Mutex<State>,
}

/// Compares every pair of sessions by digest. A side with a cached record is not run; every other side is run
/// once, longest recorded session first, and its digest taken from its journals. A difference stops the
/// comparison unless `keep_going`, and a differing session is shown by rerunning whichever side has no run
/// directory left. Run directories of sessions that match are removed unless `keep`, and a differing session
/// keeps the directories of every side.
pub(crate) fn run(
    pairs: &[Pair],
    config: &Config<'_>,
    launcher: &dyn Launcher,
) -> Result<Outcome, SameError> {
    let comparison = Comparison {
        pairs,
        config,
        launcher,
        state: Mutex::new(State {
            pairs: pairs.iter().map(|_| PairState::default()).collect(),
            found: Vec::new(),
            total_s: [0.0; 2],
            slow: Vec::new(),
            warnings: Vec::new(),
        }),
    };
    comparison.run()
}

impl Comparison<'_> {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn run(&self) -> Result<Outcome, SameError> {
        let mut jobs = Vec::new();
        let mut ready = Vec::new();
        let (mut cached, mut scheduled) = ([0_usize; 2], [0_usize; 2]);
        for (i, pair) in self.pairs.iter().enumerate() {
            for side in Side::BOTH {
                let Some(spec) = &pair.runs[side.index()] else {
                    continue;
                };
                let hit = if self.config.fresh {
                    None
                } else {
                    self.config.caches[side.index()]
                        .and_then(|cache| cache.get(spec, &pair.inputs[side.index()]))
                };
                if let Some(record) = hit {
                    self.state().pairs[i].rec[side.index()] = Some(record);
                    cached[side.index()] += 1;
                } else {
                    jobs.push((i, side));
                    scheduled[side.index()] += 1;
                }
            }
            if self.state().pairs[i].complete(pair) {
                ready.push(i);
            }
        }
        let halt = |result: Result<(), Halt>| result.inspect_err(|_| self.config.fleet.abort());
        let mut errors = pool::run(self.config.jobs, &ready, |i| halt(self.resolve(i)));
        if errors.is_empty() && !self.config.fleet.cancelled() {
            let order = longest_first(jobs.len(), |n| {
                let (i, side) = jobs[n];
                self.config.costs.and_then(|costs| {
                    costs.get(
                        &self.config.scopes[side.index()],
                        &self.pairs[i].key.to_string(),
                    )
                })
            });
            errors = pool::run(self.config.jobs, &order, |n| {
                let (i, side) = jobs[n];
                halt(self.run_side(i, side))
            });
        }
        let removed = self.remove_unresolved();
        if self.config.fleet.cancelled() {
            return Err(SameError::Interrupted);
        }
        let failure = errors.into_iter().find_map(|(_, halt)| match halt {
            Halt::Failed(error) if !error.interrupted() => Some(error),
            _ => None,
        });
        if let Some(error) = failure {
            return Err(error);
        }
        removed?;
        Ok(self.outcome(cached, scheduled))
    }

    fn outcome(&self, cached: [usize; 2], scheduled: [usize; 2]) -> Outcome {
        let mut state = self.state();
        state.found.sort_by_key(|(i, _)| *i);
        let differences = state
            .found
            .iter()
            .map(|(i, difference)| Found {
                key: self.pairs[*i].key.clone(),
                difference: difference.clone(),
            })
            .collect();
        let mut slow: Vec<Slow> = state
            .slow
            .iter()
            .filter_map(|&i| {
                let [Some(base), Some(head)] = &state.pairs[i].rec else {
                    return None;
                };
                Some(Slow {
                    key: self.pairs[i].key.clone(),
                    base_s: base.wall_s,
                    head_s: head.wall_s,
                })
            })
            .collect();
        slow.sort_by(|a, b| (b.head_s / b.base_s).total_cmp(&(a.head_s / a.base_s)));
        Outcome {
            sessions: self.pairs.len(),
            differences,
            keep_going: self.config.keep_going,
            cached,
            scheduled,
            total_s: state.total_s,
            slow,
            warnings: std::mem::take(&mut state.warnings),
        }
    }

    fn launch(&self, side: Side, spec: &RunSpec) -> Result<Ran, RunError> {
        let result = self.launcher.launch(side, spec);
        if matches!(result, Err(RunError::Interrupted)) {
            let _ = self.launcher.remove(&self.launcher.dir(side, spec));
        }
        result
    }

    /// Runs one side of a session, records its digest and, if that completes the pair, compares the pair.
    fn run_side(&self, i: usize, side: Side) -> Result<(), Halt> {
        let pair = &self.pairs[i];
        let Some(spec) = &pair.runs[side.index()] else {
            return Ok(());
        };
        let name = pair.key.to_string();
        let ran = self.launch(side, spec).map_err(|source| SameError::Run {
            side: side.name(),
            key: name.clone(),
            source,
        })?;
        let record = self
            .launcher
            .digest(&ran.dir)
            .map(|digest| SideRecord {
                digest,
                exit: ran.exit,
                wall_s: ran.wall.as_secs_f64(),
            })
            .map_err(|source| SameError::Digest {
                side: side.name(),
                key: name.clone(),
                source,
            })?;
        if let Some(cache) = self.config.caches[side.index()]
            && let Err(error) = cache.put(spec, &pair.inputs[side.index()], &record)
        {
            self.state()
                .warnings
                .push(format!("cache {} {name}: {error}", side.name()));
        }
        if let Some(costs) = self.config.costs {
            costs.set(&self.config.scopes[side.index()], &name, record.wall_s);
        }
        let complete = {
            let mut state = self.state();
            let entry = &mut state.pairs[i];
            entry.rec[side.index()] = Some(record);
            entry.dirs[side.index()] = Some(ran.dir);
            entry.complete(pair)
        };
        if complete { self.resolve(i) } else { Ok(()) }
    }

    /// Compares a session whose sides all have records, and drops its run directories when they match. A
    /// differing session is kept with every present side's journals, and reported unless the comparison was
    /// interrupted or, without `keep_going`, another session was reported first.
    fn resolve(&self, i: usize) -> Result<(), Halt> {
        let pair = &self.pairs[i];
        if let [Some(_), Some(_)] = &pair.runs {
            let mut state = self.state();
            let [Some(base), Some(head)] = state.pairs[i].rec.clone() else {
                return Ok(());
            };
            state.total_s[0] += base.wall_s;
            state.total_s[1] += head.wall_s;
            if base.wall_s > 0.0 && head.wall_s > SLOW_RATIO * base.wall_s {
                state.slow.push(i);
            }
            drop(state);
            if base.digest == head.digest && base.exit == head.exit {
                return Ok(self.remove_runs(i)?);
            }
        }
        self.rerun_cached(i)?;
        let difference = self.difference(i)?;
        let reported = {
            let mut state = self.state();
            if self.config.fleet.cancelled() || (!self.config.keep_going && !state.found.is_empty())
            {
                false
            } else {
                state.found.push((i, difference));
                true
            }
        };
        if !reported {
            return Ok(self.remove_runs(i)?);
        }
        if self.config.keep_going {
            Ok(())
        } else {
            Err(Halt::Different)
        }
    }

    /// Reruns each present side of a session that has no run directory, so that its journals can be shown.
    fn rerun_cached(&self, i: usize) -> Result<(), SameError> {
        let pair = &self.pairs[i];
        for side in Side::BOTH {
            let Some(spec) = &pair.runs[side.index()] else {
                continue;
            };
            if self.state().pairs[i].dirs[side.index()].is_some() {
                continue;
            }
            let ran = self.launch(side, spec).map_err(|source| SameError::Rerun {
                side: side.name(),
                key: pair.key.to_string(),
                source,
            })?;
            self.state().pairs[i].dirs[side.index()] = Some(ran.dir);
        }
        Ok(())
    }

    fn difference(&self, i: usize) -> Result<Difference, SameError> {
        let pair = &self.pairs[i];
        let (records, dirs) = {
            let state = self.state();
            (state.pairs[i].rec.clone(), state.pairs[i].dirs.clone())
        };
        let marker = |base: &str, head: &str| Difference {
            file: "<session>".to_owned(),
            line: None,
            base: base.to_owned(),
            head: head.to_owned(),
        };
        let [Some(base), Some(head)] = records else {
            return Ok(if pair.runs[0].is_none() {
                marker("<missing>", "<present>")
            } else {
                marker("<present>", "<missing>")
            });
        };
        if base.digest == head.digest {
            return Ok(Difference {
                file: "<exit code>".to_owned(),
                line: None,
                base: base.exit.to_string(),
                head: head.exit.to_string(),
            });
        }
        let [Some(base_dir), Some(head_dir)] = dirs else {
            return Err(SameError::Compare {
                key: pair.key.to_string(),
                source: journal::JournalError::Missing,
            });
        };
        let found = self
            .launcher
            .difference(&base_dir, &head_dir)
            .map_err(|source| SameError::Compare {
                key: pair.key.to_string(),
                source,
            })?;
        Ok(found.unwrap_or(Difference {
            file: "<journal digest>".to_owned(),
            line: None,
            base: base.digest,
            head: head.digest,
        }))
    }

    fn remove_runs(&self, i: usize) -> Result<(), SameError> {
        if self.config.keep {
            return Ok(());
        }
        let dirs = std::mem::take(&mut self.state().pairs[i].dirs);
        for (side, dir) in Side::BOTH.into_iter().zip(dirs) {
            if let Some(dir) = dir {
                self.launcher
                    .remove(&dir)
                    .map_err(|source| SameError::Remove {
                        side: side.name(),
                        key: self.pairs[i].key.to_string(),
                        source,
                    })?;
            }
        }
        Ok(())
    }

    /// Drops the run directories of sessions stopped before their comparison.
    fn remove_unresolved(&self) -> Result<(), SameError> {
        let differing: Vec<usize> = self.state().found.iter().map(|(i, _)| *i).collect();
        let mut first = Ok(());
        for i in 0..self.pairs.len() {
            if !differing.contains(&i)
                && let Err(error) = self.remove_runs(i)
                && first.is_ok()
            {
                first = Err(error);
            }
        }
        first
    }
}

/// Removes the empty directories under `root`, leaving `root` itself, so that what remains of a comparison is
/// only what it kept.
pub(crate) fn remove_empty_dirs(root: &Path) {
    fn prune(dir: &Path) -> bool {
        let Ok(entries) = fs::read_dir(dir) else {
            return false;
        };
        let mut empty = true;
        for entry in entries.flatten() {
            let path = entry.path();
            let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
            if is_dir && prune(&path) {
                continue;
            }
            empty = false;
        }
        empty && fs::remove_dir(dir).is_ok()
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            prune(&entry.path());
        }
    }
}
