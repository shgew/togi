//! The successor scorer: the complete-experiment check and every criterion of #1457 §4, recomputed from
//! run records alone.
//!
//! [`score`] takes a parsed registration, its explicit SHA-256 (a registration cannot hash itself), a
//! [`ScoreSelection`] (split, run profile, policy and optionally a subset of slopes) and the records.
//! It runs no simulation and reads no truth beyond what a record already carries.
//!
//! # Completeness
//!
//! The experiment is exactly the registration's scenario × seed × applicable mode × slope cells. A cell
//! is *complete* only if it holds one record that passes [`Record::validate`], matches the registration
//! and selection identities (registration id and hash, policy identity and source/parameter hashes,
//! revisions, binary/input hashes, split, run profile, topology, truth-epoch schedule, resources),
//! observes the full horizon and whose recorded outcome agrees with a recomputed `settled_at`. A
//! missing, duplicated, unexpected, invalid, incompatible, truncated or harness-error cell makes the
//! verdict [`Verdict::Incomplete`], never a pass. Descriptive fractions use the complete records as
//! their denominator, so they can be read for a partial experiment but never rescue it.
//!
//! # Criteria
//!
//! Safety (retained-profile safety, realized post-finish risk, learning contract) and usefulness
//! (benign depth, convergence, retention) are separate columns, judged per scenario, mode and slope with
//! no averaging across them. A `reduced-dev` profile is labelled
//! [`REDUCED_DEV_LABEL`](super::registration::REDUCED_DEV_LABEL) and
//! never yields a gate verdict. A seed that never finishes has no post-finish exposure and fails the
//! realized-risk rule (`T = 0` fails). The Poisson upper rate is a model-based diagnostic only.

#[cfg(test)]
#[path = "fixture.rs"]
pub(crate) mod fixture;
#[cfg(test)]
#[path = "score_integration_tests.rs"]
mod integration_tests;
pub mod learning;
pub mod measure;
pub mod rules;
pub mod stats;
#[cfg(test)]
#[path = "score_tests.rs"]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use togi_simrun::day_night::successor::{
    Outcome, PartialCounts, Record, RunProfile, Split, UnitMode,
};

pub use self::learning::{
    LearningAudit, PromotionView, RestorationStatus, RestorationView, Violation,
};
pub use self::measure::{CompleteOutcome, SeedMetrics};
pub use self::rules::{MAX_STEP, Rules, Scope};
pub use self::stats::{CONFIDENCE, Share, median, poisson_upper_mean, poisson_upper_rate};
use super::registration::{
    self, PolicyRole, PolicySource, REDUCED_DEV_LABEL, Registration, Scenario,
};

/// What a score run selects. Everything else comes from the registration.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ScoreSelection {
    pub split: Split,
    pub run_profile: RunProfile,
    /// The registration's policy selector name, such as `successor`.
    pub policy: String,
    /// A subset of the registered slopes; `None` selects all. A registered gate needs all of them.
    pub slopes: Option<Vec<f64>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ScoreError {
    #[error("registration: {0}")]
    Registration(#[from] registration::Error),
    #[error("policy {0:?} is not registered")]
    UnknownPolicy(String),
    #[error("policy {0:?} has no frozen source identity")]
    PendingPolicy(String),
    #[error("slope {0} is not a registered slope")]
    UnregisteredSlope(f64),
    #[error("registration_sha256 must be 64 lower-case hex digits")]
    RegistrationHash,
}

#[derive(Debug, thiserror::Error)]
#[error("line {line}: {source}")]
pub struct ReadError {
    pub line: usize,
    pub source: serde_json::Error,
}

/// Strictly decodes JSON Lines of records in memory: one object per nonempty line, unknown or duplicate
/// fields rejected. Decoding does not validate; [`score`] validates every record.
pub fn read_records(input: &[u8]) -> Result<Vec<Record>, ReadError> {
    input
        .split(|byte| *byte == b'\n')
        .enumerate()
        .filter(|(_, line)| !line.iter().all(u8::is_ascii_whitespace))
        .map(|(index, line)| {
            serde_json::from_slice(line).map_err(|source| ReadError {
                line: index + 1,
                source,
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum Verdict {
    Pass,
    Fail,
    Incomplete,
    /// A reduced development read: never a gate verdict.
    ExploratoryReduced,
    /// Comparator criteria are descriptive and cannot pass the candidate's gate.
    Diagnostic,
}

impl Verdict {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Incomplete => "INCOMPLETE",
            Self::ExploratoryReduced => REDUCED_DEV_LABEL,
            Self::Diagnostic => "DIAGNOSTIC — NOT A CANDIDATE GATE",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum Column {
    Safety,
    Usefulness,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub enum GroupStatus {
    Incomplete,
    Met,
    NotMet,
}

/// One registered criterion on one scenario, mode and slope, with its exact numerator and denominator.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Criterion {
    pub name: &'static str,
    pub column: Column,
    /// The registered rule in words.
    pub rule: String,
    /// The exact measured fraction, when the criterion is a fraction of seeds.
    pub share: Option<Share>,
    pub measured: String,
    pub passed: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct OutcomeCounts {
    pub converged: u64,
    pub unconverged_crash_free: u64,
    pub unconverged_with_co_crashes: u64,
}

/// Measured diagnostics beside the gate; none of them decides a verdict.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Diagnostics {
    pub outcomes: OutcomeCounts,
    /// Post-finish true CO daily crashes, active hours and integrated expected failures over complete seeds.
    pub k: u64,
    pub t: f64,
    pub h: f64,
    /// One-sided 95% Poisson upper mean and rate for `k` over `t`: a model-based diagnostic, not a guarantee
    /// for an adaptive policy.
    pub poisson_upper_mean: f64,
    pub poisson_upper_rate: Option<f64>,
    pub zero_co_learning_crashes: Share,
    pub zero_total_daily_interruptions: Share,
    pub zero_daily_co_crashes: Share,
    pub mean_unit_total_depth: Option<f64>,
    pub mean_core_depth: Option<f64>,
    pub median_settled_day: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Group {
    pub scenario: String,
    pub unit_mode: UnitMode,
    pub slope: f64,
    pub scope: Scope,
    pub expected_seeds: Vec<u64>,
    /// Complete seeds, ascending.
    pub seeds: Vec<SeedMetrics>,
    pub criteria: Vec<Criterion>,
    pub safety_met: bool,
    pub usefulness_met: bool,
    pub status: GroupStatus,
    pub diagnostics: Diagnostics,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum FaultKind {
    Missing,
    Duplicate {
        copies: usize,
    },
    /// A record outside the expected scenario × seed × mode × slope set.
    Unexpected,
    /// `Record::validate` refused the record.
    Invalid(String),
    /// Identity, calendar, topology or schedule disagrees with the registration or selection.
    Incompatible(Vec<String>),
    /// The recorded outcome contradicts the recomputation.
    Inconsistent(String),
    Truncated {
        observed_until_day: f64,
        reason: String,
        lower_bound: PartialCounts,
    },
    HarnessError {
        observed_until_day: f64,
        error: String,
    },
}

impl FaultKind {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Duplicate { .. } => "duplicate",
            Self::Unexpected => "unexpected",
            Self::Invalid(_) => "invalid",
            Self::Incompatible(_) => "incompatible",
            Self::Inconsistent(_) => "inconsistent",
            Self::Truncated { .. } => "truncated",
            Self::HarnessError { .. } => "harness-error",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Fault {
    pub scenario: String,
    pub seed: u64,
    pub unit_mode: UnitMode,
    pub slope: f64,
    pub kind: FaultKind,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RuntimeSummary {
    pub records: u64,
    pub complete: u64,
    /// Sum of distinct recorded invocation elapsed times; unavailable when any timing is absent.
    pub invocation_wall_seconds: Option<f64>,
    pub invocations: Vec<togi_simrun::day_night::successor::InvocationTiming>,
    /// Sum of every run's wall seconds; not elapsed time, which concurrency shortens.
    pub summed_run_wall_seconds: f64,
    pub max_run_wall_seconds: f64,
    /// Compilation is excluded from the budget; the largest recorded value is the one build.
    pub compilation_seconds: f64,
    pub trial_launches: u64,
    pub jobs: Vec<u32>,
    pub resource_classes: Vec<String>,
    /// Registered execution budget for one policy/slope invocation, excluding compilation.
    pub budget_seconds: u32,
    pub intended_seconds: f64,
    pub observed_seconds: f64,
    pub eligible_seconds: f64,
    pub onset_active_seconds: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Score {
    pub registration_id: String,
    pub registration_sha256: String,
    pub policy: String,
    pub policy_identity: String,
    pub policy_role: PolicyRole,
    pub split: Split,
    pub run_profile: RunProfile,
    pub verdict: Verdict,
    /// The exploratory label of a reduced run; a registered run has none.
    pub label: Option<String>,
    pub registered_slopes: Vec<f64>,
    pub selected_slopes: Vec<f64>,
    pub expected_cells: u64,
    pub complete_cells: u64,
    /// Experiment-level reasons the experiment is incomplete besides per-cell faults.
    pub issues: Vec<String>,
    pub faults: Vec<Fault>,
    pub groups: Vec<Group>,
    pub runtime: RuntimeSummary,
}

impl Score {
    pub fn complete(&self) -> bool {
        self.faults.is_empty()
            && self.issues.is_empty()
            && self.expected_cells == self.complete_cells
    }

    /// Scenarios whose every selected mode and slope meets every criterion; never an aggregate verdict.
    pub fn scenario_status(&self, scenario: &str) -> GroupStatus {
        let mut status = GroupStatus::Met;
        for group in self
            .groups
            .iter()
            .filter(|group| group.scenario == scenario)
        {
            match group.status {
                GroupStatus::Incomplete => return GroupStatus::Incomplete,
                GroupStatus::NotMet => status = GroupStatus::NotMet,
                GroupStatus::Met => {}
            }
        }
        status
    }
}

type Key = (String, u64, UnitMode, usize);

struct Context<'a> {
    registration: &'a Registration,
    sha256: &'a str,
    selection: &'a ScoreSelection,
    policy: &'a PolicySource,
    source_revision: &'a str,
    source_sha256: &'a str,
    rules: Rules,
    registered: bool,
}

fn lower_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn slope_index(slopes: &[f64], slope: f64) -> Option<usize> {
    slopes
        .iter()
        .position(|registered| registered.to_bits() == slope.to_bits())
}

/// Scores records only against a strictly validated successor registration.
pub fn score(
    registration: &Registration,
    registration_sha256: &str,
    selection: &ScoreSelection,
    records: &[Record],
) -> Result<Score, ScoreError> {
    registration.validate()?;
    score_validated(registration, registration_sha256, selection, records)
}

/// Internal criterion fixtures use smaller cell sets without changing the public registration.
pub(super) fn score_validated(
    registration: &Registration,
    registration_sha256: &str,
    selection: &ScoreSelection,
    records: &[Record],
) -> Result<Score, ScoreError> {
    if !lower_hex(registration_sha256) {
        return Err(ScoreError::RegistrationHash);
    }
    let policy = registration
        .policy(&selection.policy)
        .ok_or_else(|| ScoreError::UnknownPolicy(selection.policy.clone()))?;
    let registration::SourceIdentity::Pinned {
        revision: source_revision,
        sha256: source_sha256,
    } = &policy.source
    else {
        return Err(ScoreError::PendingPolicy(policy.name.clone()));
    };
    let spec = registration
        .run_profile(selection.run_profile)
        .ok_or(registration::Error::Invalid("run profile not registered"))?;
    let selected: Vec<usize> = match &selection.slopes {
        None => (0..registration.slopes.len()).collect(),
        Some(slopes) => {
            let mut indices = BTreeSet::new();
            for slope in slopes {
                indices.insert(
                    slope_index(&registration.slopes, *slope)
                        .ok_or(ScoreError::UnregisteredSlope(*slope))?,
                );
            }
            indices.into_iter().collect()
        }
    };
    let cells = registration.cells(selection.split, selection.run_profile)?;
    let context = Context {
        registration,
        sha256: registration_sha256,
        selection,
        policy,
        source_revision,
        source_sha256,
        rules: Rules::new(registration),
        registered: selection.run_profile == RunProfile::Registered,
    };
    let mut expected: BTreeSet<Key> = BTreeSet::new();
    for cell in cells {
        if let Some(slope) = slope_index(&registration.slopes, cell.slope)
            && selected.contains(&slope)
        {
            expected.insert((cell.scenario, cell.seed, cell.unit_mode, slope));
        }
    }

    // Bucket records by cell; anything off the expected set is a fault of its own.
    let mut buckets: BTreeMap<Key, Vec<&Record>> = BTreeMap::new();
    let mut faults = Vec::new();
    for record in records {
        let provenance = &record.provenance;
        let key = slope_index(&registration.slopes, provenance.slope).map(|slope| {
            (
                provenance.scenario.clone(),
                provenance.seed,
                provenance.unit_mode,
                slope,
            )
        });
        match key {
            Some(key) if expected.contains(&key) => buckets.entry(key).or_default().push(record),
            Some(_) | None => faults.push(Fault {
                scenario: provenance.scenario.clone(),
                seed: provenance.seed,
                unit_mode: provenance.unit_mode,
                slope: provenance.slope,
                kind: FaultKind::Unexpected,
            }),
        }
    }

    let mut measured: BTreeMap<Key, SeedMetrics> = BTreeMap::new();
    let mut runtime = RuntimeAccumulator::default();
    for record in records {
        runtime.add(record);
    }
    let mut hashes = (BTreeSet::new(), BTreeSet::new());
    for key in &expected {
        let slope = registration.slopes[key.3];
        let fault = |kind| Fault {
            scenario: key.0.clone(),
            seed: key.1,
            unit_mode: key.2,
            slope,
            kind,
        };
        let Some(scenario) = registration.scenario(&key.0) else {
            continue;
        };
        match buckets.get(key).map(Vec::as_slice) {
            None | Some([]) => faults.push(fault(FaultKind::Missing)),
            Some([record]) => match context.inspect(record, scenario) {
                Ok(metrics) => {
                    hashes.0.extend(record.provenance.binary_sha256.clone());
                    hashes.1.extend(record.provenance.input_sha256.clone());
                    measured.insert(key.clone(), metrics);
                }
                Err(kind) => faults.push(fault(kind)),
            },
            Some(copies) => faults.push(fault(FaultKind::Duplicate {
                copies: copies.len(),
            })),
        }
    }
    faults.sort_by(|a, b| {
        a.scenario
            .cmp(&b.scenario)
            .then(a.seed.cmp(&b.seed))
            .then(a.unit_mode.cmp(&b.unit_mode))
            .then(a.slope.total_cmp(&b.slope))
    });

    let mut issues = Vec::new();
    issues.extend(
        runtime
            .timing_conflicts
            .iter()
            .map(|id| format!("inconsistent invocation timing: {id}")),
    );
    // Only a gate profile needs every registered slope; a reduced read is one slope by design.
    if spec.gate_verdict && selected.len() != registration.slopes.len() {
        issues.push(format!("slope selection covers {} of {} registered slopes; a registered gate needs all of them", selected.len(), registration.slopes.len()));
    }
    if registration.sources.binary_sha256.is_none() && hashes.0.len() > 1 {
        issues.push(format!(
            "records come from {} different binaries",
            hashes.0.len()
        ));
    }
    if registration.sources.input_sha256.is_none() && hashes.1.len() > 1 {
        issues.push(format!(
            "records come from {} different input sets",
            hashes.1.len()
        ));
    }

    let mut groups = Vec::new();
    for scenario in &registration.scenarios {
        let scope = Scope::of(scenario);
        for unit_mode in registration.modes(scenario) {
            for &slope in &selected {
                let seeds = registration.seeds(selection.split, selection.run_profile)?;
                let complete: Vec<SeedMetrics> = seeds
                    .iter()
                    .filter_map(|seed| {
                        measured
                            .get(&(scenario.name.clone(), *seed, unit_mode, slope))
                            .cloned()
                    })
                    .collect();
                groups.push(build_group(
                    &context.rules,
                    scenario.name.clone(),
                    unit_mode,
                    registration.slopes[slope],
                    scope,
                    seeds,
                    complete,
                ));
            }
        }
    }

    let expected_cells = expected.len() as u64;
    let complete_cells = measured.len() as u64;
    let all_complete = faults.is_empty() && issues.is_empty() && expected_cells == complete_cells;
    let gate = spec.gate_verdict && selection.run_profile == RunProfile::Registered;
    let verdict = if !all_complete {
        Verdict::Incomplete
    } else if !gate {
        Verdict::ExploratoryReduced
    } else if policy.role == PolicyRole::Comparator {
        Verdict::Diagnostic
    } else if groups.iter().all(|group| group.status == GroupStatus::Met) {
        Verdict::Pass
    } else {
        Verdict::Fail
    };
    Ok(Score {
        registration_id: registration.id.clone(),
        registration_sha256: registration_sha256.to_owned(),
        policy: policy.name.clone(),
        policy_identity: policy.identity.clone(),
        policy_role: policy.role,
        split: selection.split,
        run_profile: selection.run_profile,
        verdict,
        label: spec.label.clone(),
        registered_slopes: registration.slopes.clone(),
        selected_slopes: selected
            .iter()
            .map(|index| registration.slopes[*index])
            .collect(),
        expected_cells,
        complete_cells,
        issues,
        faults,
        groups,
        runtime: runtime.finish(
            registration.budget_seconds(selection.split, selection.run_profile),
            complete_cells,
        ),
    })
}

impl Context<'_> {
    /// Judges one record that sits in an expected cell.
    fn inspect(&self, record: &Record, scenario: &Scenario) -> Result<SeedMetrics, FaultKind> {
        record
            .validate()
            .map_err(|error| FaultKind::Invalid(error.to_string()))?;
        let reasons = self.incompatibilities(record, scenario);
        if !reasons.is_empty() {
            return Err(FaultKind::Incompatible(reasons));
        }
        match &record.outcome {
            Outcome::Truncated {
                observed_until_day,
                reason,
                lower_bound,
            } => {
                return Err(FaultKind::Truncated {
                    observed_until_day: *observed_until_day,
                    reason: reason.clone(),
                    lower_bound: lower_bound.clone(),
                });
            }
            Outcome::HarnessError {
                observed_until_day,
                error,
            } => {
                return Err(FaultKind::HarnessError {
                    observed_until_day: *observed_until_day,
                    error: error.clone(),
                });
            }
            Outcome::CompleteConverged { observed_until_day }
            | Outcome::CompleteUnconvergedCrashFree { observed_until_day }
            | Outcome::CompleteUnconvergedWithCoCrashes { observed_until_day } => {
                if !self
                    .rules
                    .same_day(*observed_until_day, self.rules.horizon_end())
                {
                    return Err(FaultKind::Incompatible(vec![format!(
                        "a complete record observes until day {observed_until_day}, not the registered horizon end {}",
                        self.rules.horizon_end()
                    )]));
                }
            }
        }
        measure::measure(record, &self.rules, Scope::of(scenario)).map_err(FaultKind::Inconsistent)
    }

    fn incompatibilities(&self, record: &Record, scenario: &Scenario) -> Vec<String> {
        let registration = self.registration;
        let provenance = &record.provenance;
        let mut reasons = Vec::new();
        let mut check = |ok: bool, what: &str| {
            if !ok {
                reasons.push(what.to_owned());
            }
        };
        check(
            provenance.registration_id == registration.id,
            "registration id differs",
        );
        check(
            provenance.registration_sha256 == self.sha256,
            "registration hash differs",
        );
        check(
            provenance.split == self.selection.split,
            "split differs from the selection",
        );
        check(
            provenance.run_profile == self.selection.run_profile,
            "run profile differs from the selection",
        );
        check(
            provenance.policy_identity == self.policy.identity,
            "policy identity differs from the registered resolved identity",
        );
        check(
            provenance.source_revision == self.source_revision,
            "policy source revision differs",
        );
        check(
            provenance.policy_source_sha256 == self.source_sha256,
            "policy source hash differs",
        );
        check(
            provenance.parameters_sha256 == self.policy.parameters_sha256,
            "policy parameters hash differs",
        );
        check(
            provenance.simulator_revision == registration.sources.simulator_revision,
            "simulator revision differs",
        );
        check(
            provenance.report_revision == registration.sources.report_revision,
            "report revision differs",
        );
        check(
            provenance.runner_revision == registration.sources.runner_revision,
            "runner revision differs",
        );
        for (recorded, pinned, name) in [
            (
                &provenance.binary_sha256,
                &registration.sources.binary_sha256,
                "binary",
            ),
            (
                &provenance.input_sha256,
                &registration.sources.input_sha256,
                "input",
            ),
        ] {
            if self.registered {
                check(
                    recorded.is_some(),
                    &format!("a registered score requires the {name} hash"),
                );
            }
            if let Some(pinned) = pinned {
                check(
                    recorded.as_ref().is_none_or(|hash| hash == pinned)
                        && (!self.registered || recorded.is_some()),
                    &format!("{name} hash differs from the pinned hash"),
                );
            }
        }
        check(
            record.runtime.jobs == registration.resources.jobs
                && record.runtime.resource_class == registration.resources.resource_class,
            "runtime jobs or resource class differ from the registered resources",
        );
        check(
            record.runtime.invocation.as_ref().is_none_or(|invocation| {
                invocation.elapsed_seconds
                    <= f64::from(
                        registration
                            .budget_seconds(self.selection.split, self.selection.run_profile),
                    )
            }),
            "invocation wall time exceeds the registered budget",
        );
        check(
            record.policy.allowance.is_some() || self.policy.role == PolicyRole::Comparator,
            "a candidate policy report carries no allowance ledger",
        );
        check(
            self.policy.role == PolicyRole::Comparator
                || record.events.iter().all(|event| {
                    event.period == togi_simrun::day_night::successor::Period::Night
                        || event.policy_event.classification
                            != togi_simrun::day_night::successor::Classification::Unreported
                }),
            "a candidate daily event carries no policy classification",
        );

        // Topology and unit dimensions come from the scenario.
        let topology = scenario.topology;
        let cores = usize::from(topology.ccds) * usize::from(topology.cores_per_ccd);
        let ccds: BTreeSet<_> = provenance.topology.iter().map(|core| core.ccd).collect();
        let balanced = ccds.iter().all(|ccd| {
            provenance
                .topology
                .iter()
                .filter(|core| core.ccd == *ccd)
                .count()
                == usize::from(topology.cores_per_ccd)
        });
        check(
            provenance.topology.len() == cores
                && ccds.len() == usize::from(topology.ccds)
                && balanced,
            "topology differs from the scenario's CCDs and cores",
        );
        let units = match provenance.unit_mode {
            UnitMode::CcdUniform => usize::from(topology.ccds),
            UnitMode::PerCore => cores,
        };
        check(
            provenance.membership.len() == units && record.units.len() == units,
            "unit membership does not match the unit mode's unit count",
        );

        // Truth-epoch schedule.
        let scheduled = &scenario.epochs;
        let schedule_ok =
            record.epochs.len() == scheduled.len()
                && record.epochs.iter().zip(scheduled).enumerate().all(
                    |(index, (epoch, wanted))| {
                        let end = scheduled
                            .get(index + 1)
                            .map_or(self.rules.horizon_end(), |next| f64::from(next.start_day));
                        epoch.id == wanted.id
                            && epoch.context == wanted.context
                            && epoch.reference.tolerance.to_bits()
                                == registration.tolerance.admissibility.to_bits()
                            && self
                                .rules
                                .same_day(epoch.start_day, f64::from(wanted.start_day))
                            && self.rules.same_day(epoch.end_day, end)
                    },
                );
        check(
            schedule_ok,
            "truth-epoch schedule differs from the registered scenario",
        );
        reasons
    }
}

#[derive(Default)]
struct RuntimeAccumulator {
    records: u64,
    summed: f64,
    max: f64,
    compilation: f64,
    invocations: BTreeMap<String, f64>,
    timed_records: u64,
    timing_conflicts: BTreeSet<String>,
    launches: u64,
    jobs: BTreeSet<u32>,
    classes: BTreeSet<String>,
    exposure: (f64, f64, f64, f64),
}

impl RuntimeAccumulator {
    fn add(&mut self, record: &Record) {
        let runtime = &record.runtime;
        self.records += 1;
        self.summed += runtime.wall_seconds;
        self.max = self.max.max(runtime.wall_seconds);
        self.compilation = self.compilation.max(runtime.compilation_seconds);
        if let Some(invocation) = &runtime.invocation {
            self.timed_records += 1;
            if let Some(previous) = self.invocations.get(&invocation.id) {
                if previous.to_bits() != invocation.elapsed_seconds.to_bits() {
                    self.timing_conflicts.insert(invocation.id.clone());
                }
            } else {
                self.invocations
                    .insert(invocation.id.clone(), invocation.elapsed_seconds);
            }
        }
        self.launches += runtime.trial_launches;
        self.jobs.insert(runtime.jobs);
        self.classes.insert(runtime.resource_class.clone());
        let exposure = &record.exposure;
        self.exposure.0 += exposure.intended_seconds;
        self.exposure.1 += exposure.observed_seconds;
        self.exposure.2 += exposure.eligible_seconds;
        self.exposure.3 += exposure.onset_active_seconds;
    }

    fn finish(self, budget_seconds: u32, complete: u64) -> RuntimeSummary {
        let invocation_wall_seconds = (self.timed_records == self.records
            && !self.invocations.is_empty()
            && self.timing_conflicts.is_empty())
        .then(|| self.invocations.values().sum());
        RuntimeSummary {
            records: self.records,
            complete,
            invocation_wall_seconds,
            invocations: self
                .invocations
                .into_iter()
                .map(
                    |(id, elapsed_seconds)| togi_simrun::day_night::successor::InvocationTiming {
                        id,
                        elapsed_seconds,
                    },
                )
                .collect(),
            summed_run_wall_seconds: self.summed,
            max_run_wall_seconds: self.max,
            compilation_seconds: self.compilation,
            trial_launches: self.launches,
            jobs: self.jobs.into_iter().collect(),
            resource_classes: self.classes.into_iter().collect(),
            budget_seconds,
            intended_seconds: self.exposure.0,
            observed_seconds: self.exposure.1,
            eligible_seconds: self.exposure.2,
            onset_active_seconds: self.exposure.3,
        }
    }
}

fn mean(values: impl Iterator<Item = f64>) -> Option<f64> {
    let (sum, count) = values.fold((0.0, 0_u32), |(sum, count), value| (sum + value, count + 1));
    (count > 0).then(|| sum / f64::from(count))
}

fn build_group(
    rules: &Rules,
    scenario: String,
    unit_mode: UnitMode,
    slope: f64,
    scope: Scope,
    expected_seeds: Vec<u64>,
    seeds: Vec<SeedMetrics>,
) -> Group {
    let criteria = &rules.criteria;
    let count =
        |predicate: &dyn Fn(&SeedMetrics) -> bool| Share::count(seeds.iter().map(predicate));
    let mut list: Vec<Criterion> = Vec::new();
    let fraction = |share: Share, wanted: registration::Fraction| {
        wanted.met_by(share.numerator, share.denominator)
    };
    let percent =
        |wanted: registration::Fraction| format!("{}/{}", wanted.numerator, wanted.denominator);

    let final_in_a = count(&|seed| seed.final_in_a);
    list.push(Criterion {
        name: "retained-profile-in-A",
        column: Column::Safety,
        rule: format!(
            "final profile in A on at least {} of complete seeds",
            percent(criteria.final_in_admissible_fraction)
        ),
        share: Some(final_in_a),
        measured: final_in_a.to_string(),
        passed: fraction(final_in_a, criteria.final_in_admissible_fraction),
    });
    let held = count(&|seed| seed.post_finish.all_in_a);
    list.push(Criterion { name: "held-after-finish-in-A", column: Column::Safety, rule: format!("every profile held after all units first finish is in A on at least {} of complete seeds", percent(criteria.held_after_finish_fraction)), share: Some(held), measured: held.to_string(), passed: fraction(held, criteria.held_after_finish_fraction) });
    let rate = count(&|seed| seed.risk.rate_ok);
    list.push(Criterion {
        name: "post-finish-risk",
        column: Column::Safety,
        rule: format!(
            "H/T <= {} lambda on every complete seed; T = 0 fails",
            percent(criteria.post_finish_rate_of_lambda)
        ),
        share: Some(rate),
        measured: rate.to_string(),
        passed: rate.all(),
    });
    let learning = count(&|seed| seed.learning.reported && seed.learning.violations.is_empty());
    list.push(Criterion { name: "learning-contract", column: Column::Safety, rule: "allowance evidence or verified fixed-stock non-applicability is established, with no allowance, stop, step, hold, qualification or restoration violation on any complete seed".into(), share: Some(learning), measured: learning.to_string(), passed: learning.all() });

    if scope.benign {
        let benign = count(&|seed| seed.benign_ok);
        list.push(Criterion { name: "benign-depth", column: Column::Usefulness, rule: format!("final profile in A with unit-total depth >= ceil({} M) and every unit at its floor on at least {} of complete seeds", percent(criteria.useful_depth_of_reference), percent(criteria.useful_fraction)), share: Some(benign), measured: benign.to_string(), passed: fraction(benign, criteria.useful_fraction) });
    }
    let horizon = f64::from(criteria.settled_horizon_day);
    let settled = count(&|seed| seed.settled_at.is_some_and(|day| day <= horizon));
    list.push(Criterion {
        name: "convergence",
        column: Column::Usefulness,
        rule: format!(
            "a finite joint settled day <= {} on at least {} of complete seeds",
            criteria.settled_horizon_day,
            percent(criteria.settled_fraction)
        ),
        share: Some(settled),
        measured: settled.to_string(),
        passed: fraction(settled, criteria.settled_fraction),
    });
    let mut settled_days: Vec<f64> = seeds.iter().map(SeedMetrics::settled_or_infinite).collect();
    settled_days.resize(expected_seeds.len().max(seeds.len()), f64::INFINITY);
    let median_settled = median(&settled_days);
    if scope.benign {
        let bound = f64::from(criteria.benign_median_settled_day);
        list.push(Criterion { name: "benign-median-settled-day", column: Column::Usefulness, rule: format!("median joint settled day <= {} over all registered seeds, infinity for unconverged or missing", criteria.benign_median_settled_day), share: None, measured: median_settled.map_or_else(|| "no seeds".into(), |day| if day.is_finite() { format!("{day}") } else { "infinity".into() }), passed: median_settled.is_some_and(|day| day <= bound) });
    }
    if scope.retention {
        let retained = count(&|seed| seed.retention_ok);
        list.push(Criterion { name: "retention", column: Column::Usefulness, rule: format!("every unit within {} of its joint-finish depth and the final profile in A on at least {} of complete seeds; never-finished fails", criteria.retention_max_loss, percent(criteria.retention_fraction)), share: Some(retained), measured: retained.to_string(), passed: fraction(retained, criteria.retention_fraction) });
    }

    let met = |column| {
        list.iter()
            .filter(|item| item.column == column)
            .all(|item| item.passed)
    };
    let (safety_met, usefulness_met) = (met(Column::Safety), met(Column::Usefulness));
    let status = if seeds.len() != expected_seeds.len() {
        GroupStatus::Incomplete
    } else if safety_met && usefulness_met {
        GroupStatus::Met
    } else {
        GroupStatus::NotMet
    };

    let (k, t, h) = seeds.iter().fold((0_u64, 0.0, 0.0), |(k, t, h), seed| {
        (k + seed.risk.k, t + seed.risk.t, h + seed.risk.h)
    });
    let mut outcomes = OutcomeCounts::default();
    for seed in &seeds {
        match seed.outcome {
            CompleteOutcome::Converged => outcomes.converged += 1,
            CompleteOutcome::UnconvergedCrashFree => outcomes.unconverged_crash_free += 1,
            CompleteOutcome::UnconvergedWithCoCrashes => outcomes.unconverged_with_co_crashes += 1,
        }
    }
    let diagnostics = Diagnostics {
        outcomes,
        k,
        t,
        h,
        poisson_upper_mean: poisson_upper_mean(k, CONFIDENCE),
        poisson_upper_rate: poisson_upper_rate(k, t),
        zero_co_learning_crashes: count(&|seed| seed.risk.learning_k == 0),
        zero_total_daily_interruptions: count(&|seed| seed.events.day().total == 0),
        zero_daily_co_crashes: count(&|seed| seed.daily_co_crashes == 0),
        mean_unit_total_depth: mean(seeds.iter().map(|seed| f64::from(seed.depth.final_total))),
        mean_core_depth: mean(seeds.iter().filter_map(|seed| seed.depth.final_core_mean)),
        median_settled_day: median_settled,
    };
    Group {
        scenario,
        unit_mode,
        slope,
        scope,
        expected_seeds,
        seeds,
        criteria: list,
        safety_met,
        usefulness_met,
        status,
        diagnostics,
    }
}
