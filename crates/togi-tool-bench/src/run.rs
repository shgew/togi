use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::num::{NonZeroU32, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use togi_tool_modelcheck::Check;
use togi_tool_trialfacts::Extracts;
use togi_tuner::{EVIDENCE_EPOCH, RULESET};

use crate::cache::{self, Costs, RunCache};
use crate::compare;
use crate::error::{Error, RunError};
use crate::gate::{self, Judgment};
use crate::git::Tree;
use crate::metrics::{self, Measured};
use crate::pool;
use crate::process::Fleet;
use crate::record::{ModelCheck, Record, Status};
use crate::simulator::{Session, Simulator};
use crate::stats::share;
use crate::suite::{DiskSource, Machine, Plan, RunSpec, Selection, Suite};
use crate::summary;

mod adapters;
use self::adapters::{ExecutionIo, RunRoot, SessionRunner};

/// What a bench run is asked for.
#[derive(Clone, Debug)]
pub struct Options {
    pub suite: PathBuf,
    pub selection: Selection,
    /// Where to write one record per run.
    pub out: Option<PathBuf>,
    /// Where to write the records as a baseline file keeps them: without model checks and wall times.
    pub baseline_out: Option<PathBuf>,
    /// A records file to compare the run with.
    pub baseline: Option<PathBuf>,
    /// A directory to keep the runs' state directories under; keeping them skips the cache.
    pub keep: Option<PathBuf>,
    pub jobs: NonZeroUsize,
    /// The hang safety limit per simulator.
    pub timeout: Duration,
    pub max_boots: NonZeroU32,
    /// The cache directory of finished runs and their costs; none runs every session.
    pub cache: Option<PathBuf>,
    /// The simulator command; none builds `togi-tool-sim` from the tree.
    pub simulator: Option<PathBuf>,
    /// The revision and dirtiness stamped on every record.
    pub tree: Tree,
}

/// A finished bench run.
#[derive(Debug)]
pub struct Report {
    pub notes: Vec<String>,
    /// One record per selected run, in suite order.
    pub records: Vec<Record>,
    pub checks: Vec<Check>,
    pub baseline: Option<Vec<Record>>,
    /// How many records came from the cache.
    pub cached: usize,
    /// The directory the runs were kept in, when asked for.
    pub kept: Option<PathBuf>,
    pub wall: Duration,
    /// Problems that did not stop the run.
    pub warnings: Vec<String>,
    /// The registered gate's verdict, when the suite registers a gate and a baseline was given.
    pub gate: Option<Judgment>,
}

impl Report {
    /// Writes the report: the suite's notes, the summary, the model checks and, with a baseline, the comparison
    /// and the gate's verdict.
    pub fn write(&self, w: &mut impl io::Write) -> io::Result<()> {
        for note in &self.notes {
            writeln!(w, "suite: {note}")?;
        }
        summary::report(w, &self.records)?;
        togi_tool_modelcheck::report(w, &self.checks)?;
        if let Some(baseline) = &self.baseline {
            compare::report(w, &self.records, baseline)?;
        }
        if let Some(judgment) = &self.gate {
            judgment.write(w)?;
        }
        writeln!(
            w,
            "cache: {} of {} runs cached",
            self.cached,
            self.records.len()
        )?;
        writeln!(w, "harness_wall_s={:.3}", self.wall.as_secs_f64())
    }
}

/// Runs the suite's selected sessions on the simulator and measures each.
///
/// The records are in suite order whatever order the sessions finish in, so the report does not depend on
/// `jobs`. A run whose cache entry was made from the same simulator, bench, flags and inputs is not run again.
pub fn execute(options: &Options, fleet: &Fleet) -> Result<Report, Error> {
    execute_with(options, fleet, &DiskSource)
}

fn execute_with(options: &Options, fleet: &Fleet, io: &impl ExecutionIo) -> Result<Report, Error> {
    let started = Instant::now();
    let suite = Suite::load_from(&options.suite, io)?;
    let gate = gate::load(&suite)?;
    let plan = suite.plan_from(options.selection, io)?;
    let (verdicts, checks) = model_checks_with(&plan, |path| io.read_facts(path))?;
    let baseline = options
        .baseline
        .as_deref()
        .map(|path| io.read_records(path, "baseline"))
        .transpose()?;
    let simulator = io.simulator(options)?;
    let (cache, costs) = io.cache(options, &simulator)?;
    let root = io.root(options.keep.as_deref())?;
    let order = cache::longest_first(plan.runs.len(), |i| {
        costs
            .as_ref()
            .and_then(|costs| costs.get(simulator.digest(), &plan.runs[i].name()))
    });
    let slots: Mutex<Vec<Option<Record>>> = Mutex::new(vec![None; plan.runs.len()]);
    let cached = AtomicUsize::new(0);
    let warnings = Mutex::new(Vec::new());
    let job = Job {
        options,
        suite: &suite,
        plan: &plan,
        simulator: &simulator,
        fleet,
        cache: cache.as_ref(),
        costs: costs.as_ref(),
        verdicts: &verdicts,
        root: root.path(),
        cached: &cached,
        warnings: &warnings,
    };
    let mut errors = pool::run(options.jobs.get(), &order, |i| {
        let record = job.run(i).inspect_err(|_| fleet.abort())?;
        slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)[i] = Some(record);
        Ok(())
    });
    let mut warnings = warnings
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    warnings.sort();
    if let Some(costs) = &costs
        && let Err(error) = costs.save()
    {
        warnings.push(format!("save session costs: {error}"));
    }
    if fleet.cancelled() {
        root.discard();
        return Err(Error::Interrupted);
    }
    if !errors.is_empty() {
        let first = errors
            .iter()
            .position(|(_, error)| !matches!(error, Error::Interrupted))
            .unwrap_or(0);
        return Err(errors.swap_remove(first).1);
    }
    let mut records: Vec<Record> = slots
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .into_iter()
        .flatten()
        .collect();
    set_scenario_shares(&mut records);
    let judgment = gate.zip(baseline.as_deref()).map(|(resolved, baseline)| {
        let verdict = resolved.judge(&records, baseline);
        Judgment { resolved, verdict }
    });
    if let Some(out) = &options.out {
        io.write_records(out, &records, false)?;
    }
    if let Some(path) = &options.baseline_out {
        io.write_records(path, &records, true)?;
    }
    Ok(Report {
        notes: suite.notes(),
        records,
        checks,
        baseline,
        cached: cached.into_inner(),
        kept: root.kept(),
        wall: started.elapsed(),
        warnings,
        gate: judgment,
    })
}

struct Job<'a, R> {
    options: &'a Options,
    suite: &'a Suite,
    plan: &'a Plan,
    warnings: &'a Mutex<Vec<String>>,
    simulator: &'a R,
    fleet: &'a Fleet,
    cache: Option<&'a RunCache>,
    costs: Option<&'a Costs>,
    verdicts: &'a BTreeMap<PathBuf, ModelCheck>,
    root: &'a Path,
    cached: &'a AtomicUsize,
}

impl<R: SessionRunner> Job<'_, R> {
    fn run(&self, index: usize) -> Result<Record, Error> {
        let spec = &self.plan.runs[index];
        self.measure(spec).map_err(|source| match source {
            RunError::Interrupted => Error::Interrupted,
            source @ (RunError::Directory { .. }
            | RunError::Start(_)
            | RunError::Wait(_)
            | RunError::TimedOut { .. }
            | RunError::Read { .. }
            | RunError::Journal(_)
            | RunError::Measure(_)
            | RunError::Model(_)) => Error::Run {
                run: spec.name(),
                source: Box::new(source),
            },
        })
    }

    fn measure(&self, spec: &RunSpec) -> Result<Record, RunError> {
        let inputs = cache::inputs(self.suite.digest(), spec);
        let keeping = self.options.keep.is_some();
        let hit = match self.cache {
            Some(cache) if !keeping => cache.get(spec, &inputs),
            _ => None,
        };
        let mut record = if let Some(record) = hit {
            self.cached.fetch_add(1, Ordering::SeqCst);
            record
        } else {
            let session = self.simulator.run(
                self.fleet,
                self.root,
                spec,
                self.options.max_boots,
                self.options.timeout,
            )?;
            let record = record_of(spec, &session, self.verdicts)?;
            if !keeping {
                self.simulator.remove(&session.dir);
            }
            if !session.ended_by_signal() {
                if let Some(cache) = self.cache
                    && let Err(error) = cache.put(spec, &inputs, &record)
                {
                    self.warnings
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(format!("write the run cache for {}: {error}", spec.name()));
                }
                if let Some(costs) = self.costs {
                    costs.set(
                        self.simulator.digest(),
                        &spec.name(),
                        record.wall_s.unwrap_or_default(),
                    );
                }
            }
            record
        };
        record.commit.clone_from(&self.options.tree.commit);
        record.dirty = self.options.tree.dirty;
        Ok(record)
    }
}

fn record_of(
    spec: &RunSpec,
    session: &Session,
    verdicts: &BTreeMap<PathBuf, ModelCheck>,
) -> Result<Record, RunError> {
    let model = spec.model().map_err(RunError::Model)?;
    let Measured {
        sim_hours,
        first_passed_cycle_h,
        passed_cycles,
        crashes,
        trials,
        real_answers,
        real_answer_share,
        trial_hours,
        hunts,
        combinations,
        final_profile,
        depth,
        hazard_per_h,
        hazard_max_per_h,
        worst_r7_hazard_per_h,
    } = metrics::measure(&session.events, &model, spec.replay.as_deref())?;
    Ok(Record {
        scenario: spec.scenario.clone(),
        machine: spec.machine.as_ref().map(|machine| machine.name.clone()),
        seed: spec.seed,
        split: spec.split,
        commit: String::new(),
        dirty: false,
        ruleset: RULESET,
        evidence: EVIDENCE_EPOCH,
        status: if session.ended_by_signal() {
            Status::Error
        } else {
            Status::of(
                session.exit,
                !session.events.is_empty(),
                session.journal_dead_end(),
                session.log_dead_end(),
            )
        },
        model_check: spec
            .machine
            .as_ref()
            .and_then(|machine| verdicts.get(&machine.path))
            .copied(),
        exit_code: session.exit,
        wall_s: Some(session.wall.as_secs_f64()),
        sim_hours,
        first_passed_cycle_h,
        passed_cycles,
        crashes,
        trials,
        real_answers,
        real_answer_share,
        scenario_real_answer_share: 0.0,
        trial_hours,
        hunts,
        combinations,
        final_profile,
        depth,
        hazard_per_h,
        hazard_max_per_h,
        worst_r7_hazard_per_h,
    })
}

fn model_checks_with(
    plan: &Plan,
    mut reader: impl FnMut(
        &Path,
    ) -> Result<
        Vec<togi_tool_trialfacts::TrialFact>,
        togi_tool_trialfacts::ReadError,
    >,
) -> Result<(BTreeMap<PathBuf, ModelCheck>, Vec<Check>), Error> {
    let mut extracts = Extracts::new();
    let mut verdicts = BTreeMap::new();
    let mut checks = Vec::new();
    for machine in &plan.machines {
        if let Some(check) = check_machine_with(machine, &mut extracts, &mut reader)? {
            verdicts.insert(
                machine.path.clone(),
                ModelCheck {
                    eligible_groups: check.groups.len(),
                    flagged_groups: check.flagged().count(),
                    idle_failures: check.idle_failures,
                },
            );
            checks.push(check);
        }
    }
    Ok((verdicts, checks))
}

pub(crate) fn check_machine(
    machine: &Machine,
    extracts: &mut Extracts,
) -> Result<Option<Check>, Error> {
    check_machine_with(machine, extracts, |path| togi_tool_trialfacts::read(path))
}

pub(crate) fn check_machine_with(
    machine: &Machine,
    extracts: &mut Extracts,
    reader: impl FnOnce(
        &Path,
    )
        -> Result<Vec<togi_tool_trialfacts::TrialFact>, togi_tool_trialfacts::ReadError>,
) -> Result<Option<Check>, Error> {
    let Some(facts) = machine
        .definition
        .facts
        .as_deref()
        .filter(|facts| !facts.is_empty())
    else {
        return Ok(None);
    };
    let failed = |message: String| Error::ModelCheck {
        machine: machine.name.clone(),
        message,
    };
    let model = machine
        .model(0)
        .map_err(|error| failed(error.to_string()))?;
    let (_, records) = extracts
        .load_with(&machine.path, &machine.definition, reader)
        .map_err(|error| failed(error.to_string()))?;
    let label = Path::new(&machine.name);
    let extract = label.parent().unwrap_or_else(|| Path::new("")).join(facts);
    togi_tool_modelcheck::check_records(label, &extract, &model, &records)
        .map(Some)
        .map_err(|error| failed(error.to_string()))
}

fn open_cache(
    options: &Options,
    simulator: &Simulator,
) -> Result<(Option<RunCache>, Option<Costs>), Error> {
    let Some(root) = &options.cache else {
        return Ok((None, None));
    };
    let bench = cache::current_exe_digest().map_err(|source| Error::ReadFile {
        what: "hash the bench executable",
        path: PathBuf::from("current executable"),
        source,
    })?;
    let key = cache::tree_key(simulator.digest(), &bench, options.max_boots.get());
    let runs = RunCache::open(root, &key).map_err(|source| Error::WriteFile {
        what: "open the run cache",
        path: root.clone(),
        source,
    })?;
    Ok((Some(runs), Some(Costs::load(root.join("costs.json")))))
}

enum Root {
    Temporary(tempfile::TempDir),
    Kept(PathBuf),
}

impl Root {
    fn path(&self) -> &Path {
        match self {
            Self::Temporary(dir) => dir.path(),
            Self::Kept(path) => path,
        }
    }

    fn kept(&self) -> Option<PathBuf> {
        match self {
            Self::Temporary(_) => None,
            Self::Kept(path) => Some(path.clone()),
        }
    }

    /// Removes a kept directory; a temporary one is removed when dropped.
    fn discard(&self) {
        if let Self::Kept(path) = self {
            let _ = fs::remove_dir_all(path);
        }
    }
}

fn run_root(keep: Option<&Path>) -> Result<Root, Error> {
    let failed = |source| Error::WriteFile {
        what: "create the run directory",
        path: keep.map(Path::to_path_buf).unwrap_or_default(),
        source,
    };
    match keep {
        Some(keep) => {
            fs::create_dir_all(keep).map_err(failed)?;
            let dir = tempfile::Builder::new()
                .prefix("bench-")
                .tempdir_in(keep)
                .map_err(failed)?;
            Ok(Root::Kept(dir.keep()))
        }
        None => tempfile::Builder::new()
            .prefix("togi-bench-runs-")
            .tempdir()
            .map(Root::Temporary)
            .map_err(failed),
    }
}

/// Sets each record's share of real answers over its whole scenario.
pub fn set_scenario_shares(records: &mut [Record]) {
    let mut totals: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    for record in records.iter() {
        let total = totals.entry(record.scenario.clone()).or_default();
        total.0 += record.real_answers;
        total.1 += record.trials;
    }
    for record in records {
        let (real, trials) = totals[&record.scenario];
        record.scenario_real_answer_share = share(real, trials);
    }
}

#[cfg(test)]
mod tests;
