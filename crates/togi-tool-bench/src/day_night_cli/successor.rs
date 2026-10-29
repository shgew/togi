//! Registration-driven successor runs and retained-record scoring, separate from Ruleset 13.

use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use serde::Serialize;
use sha2::{Digest as _, Sha256};
use togi_sim::day_night::successor::{EpochKind, Scenario};
use togi_simrun::day_night::{CalendarConfig, Policy, PolicyKind, RunError};
use togi_simrun::day_night::successor::{Provenance, Record, RunConfig, RunProfile, Split, UnitMode};
use togi_tool_bench::day_night_successor::registration::{
    self, PolicyRole, Registration, SourceIdentity, REDUCED_DEV_LABEL,
};
use togi_tool_bench::day_night_successor::{render, score};
use score::{ScoreSelection, Verdict};

use super::{DayNightArgs, Mode, Profile};

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
    #[error("decode registration identity")]
    Decode(serde_json::Error),
    #[error("validate successor registration")]
    Registration(#[from] registration::Error),
    #[error("invalid successor invocation: {0}")]
    Invalid(&'static str),
    #[error("holdout execution is refused; use the separately authorized once-only procedure {procedure:?} (authority: {authority}); this command does not unseal or spend holdout")]
    Holdout { procedure: String, authority: String },
    #[error("validate registered successor truth")]
    Truth(#[from] togi_sim::day_night::TruthError),
    #[error("write successor artifacts")]
    Io(#[from] io::Error),
    #[error("encode successor artifacts")]
    Encode(#[from] serde_json::Error),
    #[error("read successor record file {path}")]
    ReadFile { path: PathBuf, #[source] source: io::Error },
    #[error("decode successor record file {path}")]
    RecordsFile { path: PathBuf, #[source] source: score::ReadError },
    #[error("score successor records")]
    Score(#[from] score::ScoreError),
}

struct Plan {
    configs: Vec<RunConfig>,
    policy: PolicyKind,
    budget: Duration,
    started: Instant,
    jobs: u32,
    profile: RunProfile,
    registration_sha256: String,
    policy_identity: String,
    policy_source_revision: String,
    policy_source_sha256: String,
    compilation_seconds: f64,
    slope: f64,
}

#[derive(Debug, Serialize)]
struct Manifest {
    registration_id: String,
    registration_sha256: String,
    policy_identity: String,
    policy_source_revision: String,
    policy_source_sha256: String,
    run_profile: RunProfile,
    split: Split,
    unit_mode: UnitMode,
    slope: Option<f64>,
    label: String,
    jobs: u32,
    resource_class: String,
    compilation_seconds: f64,
    invocation_wall_seconds: f64,
    summed_run_wall_seconds: f64,
    maximum_run_wall_seconds: f64,
    planned_count: usize,
    retained_count: usize,
    completion_count: usize,
    trial_launches: u64,
    qualification_launches: u64,
    qualification_hours: f64,
    budget_seconds: f64,
    budget_expired: bool,
    harness_errors: Vec<String>,
    // One invocation supplies only one slope; comparator criteria never pass the candidate gate.
    gate_verdict: Option<String>,
}

struct Execution {
    manifest: Manifest,
    records: Vec<Record>,
}

fn binary_hash() -> Result<String, Error> {
    let mut binary = File::open(std::env::current_exe()?)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 65_536];
    loop {
        let count = match binary.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        };
        digest.update(&buffer[..count]);
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut hash = String::with_capacity(64);
    for byte in digest.finalize() {
        hash.push(char::from(HEX[usize::from(byte >> 4)]));
        hash.push(char::from(HEX[usize::from(byte & 15)]));
    }
    Ok(hash)
}

pub(super) fn execute(args: &DayNightArgs, bytes: &[u8]) -> Result<bool, Error> {
    let started = Instant::now();
    let registration = Registration::parse(bytes)?;
    if !args.score_from.is_empty() {
        let mut stdout = io::stdout().lock();
        return score_files(args, &registration, bytes, &|path| std::fs::read(path), &mut stdout);
    }
    let mut plan = plan(args, &registration, bytes)?;
    plan.started = started;
    let binary_sha256 = binary_hash()?;
    if registration.sources.binary_sha256.as_ref().is_some_and(|pinned| pinned != &binary_sha256) {
        return Err(Error::Invalid("executing binary SHA256 differs from the registration"));
    }
    let input_sha256 = registration::input_sha256(&registration)?;
    if registration.sources.input_sha256.as_ref().is_some_and(|pinned| pinned != &input_sha256) {
        return Err(Error::Invalid("resolved input SHA256 differs from the registration"));
    }
    for config in &mut plan.configs {
        config.provenance.binary_sha256 = Some(binary_sha256.clone());
        config.provenance.input_sha256 = Some(input_sha256.clone());
    }
    let path = args.out.as_ref().ok_or(Error::Invalid("successor requires --out FILE to retain partial artifacts"))?;
    let mut stdout = io::stdout().lock();
    writeln!(stdout, "Registration: {} v{} SHA256 {}", registration.id, registration.version, plan.registration_sha256)?;
    writeln!(stdout, "Policy: {} source revision {} SHA256 {}", plan.policy_identity, plan.policy_source_revision, plan.policy_source_sha256)?;
    if plan.profile == RunProfile::ReducedDev {
        writeln!(stdout, "{REDUCED_DEV_LABEL}")?;
    }
    writeln!(stdout, "Comparator run: per-criterion diagnostics; no candidate gate verdict")?;
    let mut output = io::BufWriter::new(File::create(path)?);
    let execution = execute_plan(plan, &registration, &togi_simrun::day_night::successor::run, &mut output)?;
    output.flush()?;
    let mut manifest_file = io::BufWriter::new(File::create(manifest_path(path))?);
    serde_json::to_writer_pretty(&mut manifest_file, &execution.manifest)?;
    writeln!(manifest_file)?;
    manifest_file.flush()?;
    report_execution(args, &registration, &execution, &mut stdout)
}

fn report_execution(
    args: &DayNightArgs,
    registration: &Registration,
    execution: &Execution,
    output: &mut dyn Write,
) -> Result<bool, Error> {
    let manifest = &execution.manifest;
    writeln!(output, "Completed {}/{}; retained {}; run wall {:.3}s; sum {:.3}s; max {:.3}s; compilation {:.3}s; jobs {}",
        manifest.completion_count, manifest.planned_count, manifest.retained_count,
        manifest.invocation_wall_seconds, manifest.summed_run_wall_seconds,
        manifest.maximum_run_wall_seconds, manifest.compilation_seconds, manifest.jobs)?;
    if manifest.budget_expired {
        writeln!(output, "Execution budget expired: partial artifacts retained; no verdict")?;
    }
    for error in &manifest.harness_errors {
        writeln!(output, "Harness error: {error}")?;
    }
    let interrupted = manifest.budget_expired || manifest.completion_count != manifest.planned_count
        || !manifest.harness_errors.is_empty();
    if interrupted {
        writeln!(output, "Interrupted or invalid execution: retained records are lower-bound evidence; scorer skipped; no verdict")?;
        return Ok(true);
    }
    let mut selection = score_selection(args, registration)?;
    selection.slopes = Some(vec![manifest.slope.ok_or(Error::Invalid("missing evaluation slope"))?]);
    let result = render_score(registration, &manifest.registration_sha256, &selection, &execution.records, output)?;
    // A successful single-slope run is not an incomplete execution merely because the other
    // slope's separately retained records are required for the experiment-level score.
    Ok(!result.faults.is_empty() || result.complete_cells != result.expected_cells)
}

fn manifest_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".manifest.json");
    PathBuf::from(name)
}

fn score_selection(args: &DayNightArgs, registration: &Registration) -> Result<ScoreSelection, Error> {
    if registration.id != registration::REGISTRATION_ID || registration.version != registration::VERSION {
        return Err(Error::Invalid("unsupported successor registration id/version"));
    }
    match args.split {
        super::Split::Dev => {}
        super::Split::Holdout | super::Split::All => return Err(Error::Holdout {
            procedure: registration.holdout.procedure.clone(),
            authority: registration.holdout.authorized_by.clone(),
        }),
    }
    let run_profile = match args.run_profile.ok_or(Error::Invalid("successor requires --run-profile reduced-dev|registered"))? {
        Profile::ReducedDev => RunProfile::ReducedDev,
        Profile::Registered => RunProfile::Registered,
    };
    if matches!(args.mode, Some(Mode::PerCore)) || registration.unit_modes.iter().any(|mode| mode.mode != UnitMode::CcdUniform) {
        return Err(Error::Invalid("this registration requires ccd-uniform mode; per-core policy is not evaluated"));
    }
    let [name] = args.policies.as_slice() else {
        return Err(Error::Invalid("successor requires exactly one --policy"));
    };
    if !matches!(name.as_str(), "co-zero" | "ruleset13" | "successor") {
        return Err(Error::Invalid("successor supports co-zero, ruleset13, or retained successor records; fixed-margin is frozen-registration only"));
    }
    let source = registration.policy(name).ok_or(Error::Invalid("policy is not registered"))?;
    if matches!(&source.source, SourceIdentity::Pending {}) {
        return Err(Error::Invalid("pending policy source cannot execute or be scored"));
    }
    if args.slope.is_some_and(|slope| !registration.slopes.contains(&slope)) {
        return Err(Error::Invalid("slope is not registered"));
    }
    Ok(ScoreSelection {
        split: Split::Dev, run_profile, policy: name.clone(),
        slopes: args.slope.map(|slope| vec![slope]),
    })
}

fn score_files(
    args: &DayNightArgs,
    registration: &Registration,
    bytes: &[u8],
    read_file: &impl Fn(&Path) -> io::Result<Vec<u8>>,
    output: &mut dyn Write,
) -> Result<bool, Error> {
    let selection = score_selection(args, registration)?;
    if args.out.is_some() || args.jobs.is_some() {
        return Err(Error::Invalid("--score-from does not run a policy: omit --out and --jobs"));
    }
    let sha = registration::sha256_hex(bytes);
    let source = registration.policy(&selection.policy).ok_or(Error::Invalid("policy is not registered"))?;
    let SourceIdentity::Pinned { revision, sha256 } = &source.source else {
        return Err(Error::Invalid("pending policy source cannot be scored"));
    };
    writeln!(output, "Registration: {} v{} SHA256 {}", registration.id, registration.version, sha)?;
    writeln!(output, "Policy: {} source revision {} SHA256 {}", source.identity, revision, sha256)?;
    let mut records = Vec::new();
    for path in &args.score_from {
        let input = read_file(path).map_err(|source| Error::ReadFile { path: path.clone(), source })?;
        records.extend(score::read_records(&input).map_err(|source| Error::RecordsFile { path: path.clone(), source })?);
    }
    let result = render_score(registration, &sha, &selection, &records, output)?;
    Ok(matches!(result.verdict, Verdict::Fail | Verdict::Incomplete))
}

fn render_score(
    registration: &Registration,
    sha: &str,
    selection: &ScoreSelection,
    records: &[Record],
    output: &mut dyn Write,
) -> Result<score::Score, Error> {
    let result = score::score(registration, sha, selection, records)?;
    write!(output, "{}", render::render_seeds(&result))?;
    if result.selected_slopes.len() != result.registered_slopes.len() {
        writeln!(output, "Single-slope criteria only: no registered gate verdict. Combine every registered slope's records with repeated --score-from FILE and omit --slope.")?;
    }
    Ok(result)
}

fn plan(args: &DayNightArgs, registration: &Registration, bytes: &[u8]) -> Result<Plan, Error> {
    let started = Instant::now();
    let selection = score_selection(args, registration)?;
    let profile = selection.run_profile;
    let name = &selection.policy;
    let source = registration.policy(name).ok_or(Error::Invalid("policy is not registered"))?;
    if source.role != PolicyRole::Comparator {
        return Err(Error::Invalid("candidate execution is not implemented; select co-zero or ruleset13"));
    }
    let (source_revision, source_sha256) = match &source.source {
        SourceIdentity::Pinned { revision, sha256 } => (revision, sha256),
        SourceIdentity::Pending {} => return Err(Error::Invalid("pending policy source cannot execute")),
    };
    let (policy, slope) = match name.as_str() {
        "co-zero" => (PolicyKind::CoZero, args.slope.unwrap_or(registration.slopes[0])),
        "ruleset13" => {
            let slope = args.slope.ok_or(Error::Invalid("ruleset13 requires --slope"))?;
            if !registration.slopes.contains(&slope) {
                return Err(Error::Invalid("slope is not registered"));
            }
            (PolicyKind::Ruleset13 { fallback_slope: slope }, slope)
        }
        _ => return Err(Error::Invalid("successor supports co-zero and ruleset13 only; fixed-margin is frozen-registration only")),
    };
    let jobs = registration.resources.jobs;
    if args.jobs.is_some_and(|requested| u32::try_from(requested.get()).ok() != Some(jobs)) {
        return Err(Error::Invalid("--jobs must equal the registration's fixed job count"));
    }
    if !args.compilation_seconds.is_finite() || args.compilation_seconds < 0.0 {
        return Err(Error::Invalid("compilation seconds must be finite and nonnegative"));
    }
    let sha = registration::sha256_hex(bytes);
    let calendar = CalendarConfig {
        days: registration.calendar.days,
        year_days: registration.calendar.year_days,
        lambda_per_hour: registration.lambda(),
        active_hours: f64::from(registration.calendar.daily_active_hours),
        night_hours: f64::from(registration.calendar.night_hours),
        night_weekdays: registration.calendar.night_weekdays.clone(),
        firmware_change_day: None,
        daily_mix: None,
        ..CalendarConfig::default()
    };
    let mut configs = Vec::new();
    for scenario in &registration.scenarios {
        for seed in registration.seeds(Split::Dev, profile)? {
            let truth = Scenario::builtin(&scenario.name, seed)?;
            validate_truth(scenario, &truth, &calendar)?;
            let daily = truth.daily_mix().iter().zip(&scenario.daily_mix).map(|(block, mix)| {
                (block.class.clone(), Duration::from_secs(u64::from(mix.seconds)))
            }).collect();
            configs.push(RunConfig {
                calendar: calendar.clone(),
                provenance: Provenance {
                    registration_id: registration.id.clone(), registration_sha256: sha.clone(),
                    source_revision: source_revision.clone(),
                    runner_revision: registration.sources.runner_revision.clone(),
                    simulator_revision: registration.sources.simulator_revision.clone(),
                    report_revision: registration.sources.report_revision.clone(),
                    binary_sha256: registration.sources.binary_sha256.clone(),
                    input_sha256: registration.sources.input_sha256.clone(),
                    policy_identity: source.identity.clone(),
                    policy_source_sha256: source_sha256.clone(),
                    parameters_sha256: source.parameters_sha256.clone(), slope,
                    split: Split::Dev, seed, scenario: scenario.name.clone(),
                    unit_mode: UnitMode::CcdUniform, topology: Vec::new(), membership: Vec::new(), run_profile: profile,
                },
                epochs: Vec::new(), daily, jobs,
                resource_class: registration.resources.resource_class.clone(),
                compilation_seconds: args.compilation_seconds, deadline: None,
                tolerance: registration.tolerance.admissibility,
            });
        }
    }
    Ok(Plan {
        configs, policy, jobs, profile, started, slope,
        budget: Duration::from_secs(u64::from(registration.budget_seconds(Split::Dev, profile))),
        registration_sha256: sha, policy_identity: source.identity.clone(),
        policy_source_revision: source_revision.clone(),
        policy_source_sha256: source_sha256.clone(), compilation_seconds: args.compilation_seconds,
    })
}

fn validate_truth(registered: &registration::Scenario, truth: &Scenario, calendar: &CalendarConfig) -> Result<(), Error> {
    let cores = usize::from(registered.topology.ccds) * usize::from(registered.topology.cores_per_ccd);
    if cores != truth.topology().len() || registered.daily_mix.len() != truth.daily_mix().len() {
        return Err(Error::Invalid("registered topology/daily mix differs from successor truth"));
    }
    for (index, (entry, block)) in registered.daily_mix.iter().zip(truth.daily_mix()).enumerate() {
        if entry.class != format!("catalog-daily-{index}") || (f64::from(entry.seconds) - block.fraction * calendar.active_hours * 3600.0).abs() > 1e-9 {
            return Err(Error::Invalid("registered ordered daily mix differs from successor truth"));
        }
    }
    let mut epochs = Vec::new();
    for day in 1..=calendar.days {
        let epoch = truth.epoch(day)?;
        if epoch.first_day == day {
            let (id, context) = match epoch.kind {
                EpochKind::Initial => ("initial".to_owned(), "ctx-initial"),
                EpochKind::Moving => (format!("moving-{}", epoch.index), "ctx-initial"),
                EpochKind::Firmware => ("firmware-1".to_owned(), "ctx-firmware-1"),
            };
            epochs.push((id, day, context));
        }
    }
    if epochs.len() != registered.epochs.len() || epochs.iter().zip(&registered.epochs).any(|((id, day, context), epoch)| {
        id != &epoch.id || day != &epoch.start_day || *context != epoch.context
    }) {
        return Err(Error::Invalid("registered truth epoch schedule/context differs from successor truth"));
    }
    Ok(())
}

fn execute_plan(
    plan: Plan,
    registration: &Registration,
    executor: &(impl Fn(RunConfig, &mut dyn Policy) -> Result<Record, RunError> + Sync),
    output: &mut dyn Write,
) -> Result<Execution, Error> {
    let started = plan.started;
    let deadline = started + plan.budget;
    let planned_count = plan.configs.len();
    let queue = Mutex::new(plan.configs.into_iter());
    let (sender, receiver) = mpsc::channel();
    let mut records = Vec::with_capacity(planned_count);
    let mut manifest = Manifest {
        registration_id: registration.id.clone(), registration_sha256: plan.registration_sha256,
        policy_identity: plan.policy_identity, policy_source_sha256: plan.policy_source_sha256,
        policy_source_revision: plan.policy_source_revision,
        run_profile: plan.profile,
        split: Split::Dev, unit_mode: UnitMode::CcdUniform, slope: Some(plan.slope),
        label: match plan.profile { RunProfile::ReducedDev => REDUCED_DEV_LABEL, RunProfile::Registered => "REGISTERED PROFILE — SINGLE-SLOPE COMPARATOR CRITERIA" }.to_owned(),
        jobs: plan.jobs, resource_class: registration.resources.resource_class.clone(),
        compilation_seconds: plan.compilation_seconds, invocation_wall_seconds: 0.0,
        summed_run_wall_seconds: 0.0, maximum_run_wall_seconds: 0.0,
        planned_count, retained_count: 0, completion_count: 0, trial_launches: 0,
        qualification_launches: 0, qualification_hours: 0.0,
        budget_seconds: plan.budget.as_secs_f64(), budget_expired: false,
        harness_errors: Vec::new(), gate_verdict: None,
    };
    std::thread::scope(|scope| -> Result<(), Error> {
        let mut workers = Vec::new();
        for _ in 0..plan.jobs {
            let queue = &queue;
            let sender = sender.clone();
            let policy = plan.policy;
            workers.push(scope.spawn(move || {
                loop {
                    if Instant::now() >= deadline { break; }
                    let config = match queue.lock() {
                        Ok(mut queue) => queue.next(),
                        Err(_) => break,
                    };
                    let Some(mut config) = config else { break; };
                    config.deadline = Some(deadline);
                    let identity = format!("{} seed {}", config.provenance.scenario, config.provenance.seed);
                    let result = policy.build(&config.calendar).and_then(|mut policy| executor(config, policy.as_mut()));
                    if sender.send((identity, result)).is_err() { break; }
                }
            }));
        }
        drop(sender);
        let mut write_error = None;
        for (identity, result) in receiver {
            match result {
                Ok(record) => {
                    manifest.retained_count += 1;
                    manifest.completion_count += usize::from(record.outcome.is_complete());
                    manifest.summed_run_wall_seconds += record.runtime.wall_seconds;
                    manifest.maximum_run_wall_seconds = manifest.maximum_run_wall_seconds.max(record.runtime.wall_seconds);
                    manifest.trial_launches += record.runtime.trial_launches;
                    manifest.qualification_launches += record.units.iter().map(|unit| unit.qualification_launches).sum::<u64>();
                    manifest.qualification_hours += record.units.iter().map(|unit| unit.qualification_hours).sum::<f64>();
                    if write_error.is_none() {
                        let write = serde_json::to_writer(&mut *output, &record).map_err(Error::Encode)
                            .and_then(|()| writeln!(output).map_err(Error::Io))
                            .and_then(|()| output.flush().map_err(Error::Io));
                        if let Err(error) = write { write_error = Some(error); }
                    }
                    records.push(record);
                }
                Err(error) => manifest.harness_errors.push(format!("{identity}: {}", togi_machine::error_chain(&error))),
            }
        }
        for worker in workers {
            if worker.join().is_err() { manifest.harness_errors.push("successor worker panicked".to_owned()); }
        }
        if let Some(error) = write_error { return Err(error); }
        Ok(())
    })?;
    manifest.invocation_wall_seconds = started.elapsed().as_secs_f64();
    manifest.budget_expired = Instant::now() >= deadline;
    Ok(Execution { manifest, records })
}

#[cfg(test)]
#[path = "successor_tests.rs"]
mod tests;
