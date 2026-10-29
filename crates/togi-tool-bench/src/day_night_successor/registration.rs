//! The successor day/night registration: a strict, object-only schema frozen before any
//! scoring against its criteria, separate from the Ruleset 13 registration.
//!
//! Every object rejects unknown fields and non-object values. [`Registration::parse`] decodes
//! and then [`Registration::validate`]s; validation refuses any change to the settled values,
//! so a registration that decodes but alters a threshold, seed range, run profile, budget or
//! calendar constant is not this experiment.
//!
//! # Schema
//!
//! - `id`, `version`: [`VERSION`] and a nonempty id. [`REGISTRATION_ID`] registers `ccd-uniform`
//!   only; a `per-core` registration needs a distinct id and exactly one mode, so the two never mix.
//! - `lambda_per_hour`: λ* as an exact [`Fraction`], 1/3000 CO-added crashes per active
//!   machine-hour, applied jointly and as λ*/n per tuning unit.
//! - `slopes`: the fallback slopes 1.5 and 2.0; a scenario passes only at every slope.
//! - `seeds`: dev 20001–20200 and holdout 30001–30200, 200 seeds per split and scenario.
//! - `holdout`: who may unseal it, the procedure and command, one confirmation per
//!   candidate and slope, spent once started, never inspected before the freeze.
//! - `calendar`: day 1 is the first day, 730 days of 8 active hours, three 8-hour nights per
//!   week, 365-day years; firmware changes precede day 181's active use.
//! - `scenarios`: each pins its topology, ordered daily mix (seconds summing to the active
//!   day), truth-epoch schedule, truth parameters, coverage class, the benign and retention
//!   applicability flags, an optional per-unit depth floor and an optional unit-mode list.
//!   Without a list a scenario applies every `unit_modes` entry.
//! - `criteria`: the settled §4 thresholds as exact fractions and integers ([`SETTLED_CRITERIA`]).
//! - `run_profiles`: `reduced-dev` (first 10 dev seeds, exploratory label, never a gate
//!   verdict) and `registered` (every seed of dev or holdout); one slope per invocation.
//! - `budgets`, `resources`: 1,800 and 43,200 elapsed seconds excluding compilation, the
//!   fixed job count and resource class.
//! - `tolerance`: computational tolerances, never a relaxation of the risk budget.
//! - `unit_modes`: the scored modes. Per-core needs its own registration, so it is refused.
//! - `sources`: simulator and report revisions, optional binary and input hashes, and every
//!   policy's resolved identity, source revision, source and parameter hashes and parameters.
//! - `commands`, `streams`, `continuation`: the exact commands, random stream domains and
//!   topology permutation, and the deterministic development continuation rule.
//! - `notes`: free text, each entry nonempty.
//!
//! [`Registration::seeds`] and [`Registration::cells`] are the only seed-selection rule: the
//! scorer expects exactly those scenario × seed × mode × slope cells.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use togi_machine::object_only;
use togi_simrun::day_night::successor::{RunProfile, Split, UnitMode};

pub const REGISTRATION_ID: &str = "day-night-successor";
pub const VERSION: u32 = 1;
pub const LAMBDA_PER_HOUR: Fraction = Fraction {
    numerator: 1,
    denominator: 3000,
};
pub const SLOPES: [f64; 2] = [1.5, 2.0];
pub const DEV_SEEDS: SeedRange = SeedRange {
    first: 20001,
    count: 200,
};
pub const HOLDOUT_SEEDS: SeedRange = SeedRange {
    first: 30001,
    count: 200,
};
pub const REDUCED_DEV_SEED_COUNT: u32 = 10;
pub const REDUCED_DEV_LABEL: &str = "EXPLORATORY REDUCED DEV — NOT A REGISTERED SCORE";
pub const REDUCED_DEV_BUDGET_SECONDS: u32 = 1800;
pub const REGISTERED_BUDGET_SECONDS: u32 = 43_200;
pub const CANDIDATE_POLICY: &str = "successor";
pub const NO_WRITE_LIMIT_SCENARIO: &str = "no-write-limit-failure";
pub const NO_WRITE_LIMIT_FLOOR: u8 = 44;
/// Named members of the frozen benign list; the third is the new within-CCD heterogeneous fixture.
pub const BENIGN_NAMED: [&str; 2] = ["no-write-limit-failure", "edge-deep"];
pub const BENIGN_COUNT: usize = 3;
pub const RETENTION_SCENARIOS: [&str; 1] = ["stock-background"];
/// Upper bound on any registered computational tolerance.
pub const MAX_TOLERANCE: f64 = 1e-6;
/// The Ruleset 13 challenge families, kept as successor challenges.
pub const HISTORICAL_SCENARIOS: [&str; 22] = [
    "edge-shallow",
    "edge-deep",
    "no-write-limit-failure",
    "slope-flat",
    "slope-steep",
    "flattening-tail",
    "flatter-dangerous-class",
    "delayed-onset",
    "thermal-history",
    "hidden-daily",
    "partial-only",
    "single-core-binding",
    "jointly-binding",
    "r8-nonbinding",
    "both-ccd-interaction",
    "stock-background",
    "single-ccd",
    "moving-cap",
    "firmware-change",
    "non-cpu-correct",
    "non-cpu-wrong",
    "non-cpu-missing",
];
pub const SETTLED_CALENDAR: (u32, u32, u32, u32, usize, u32, u32) = (1, 730, 8, 8, 3, 365, 181);

pub const SETTLED_CRITERIA: Criteria = Criteria {
    final_in_admissible_fraction: Fraction {
        numerator: 1,
        denominator: 1,
    },
    held_after_finish_fraction: Fraction {
        numerator: 95,
        denominator: 100,
    },
    safety_grace_days: 0,
    post_finish_rate_of_lambda: Fraction {
        numerator: 1,
        denominator: 1,
    },
    poisson_diagnostic_confidence: Fraction {
        numerator: 95,
        denominator: 100,
    },
    allowance_limit: 2,
    first_step_clean_stock_hours: 50,
    clean_hold_hours: 50,
    restoration_clean_stock_hours: 50,
    ceiling_margin: 5,
    operational_max_depth: 45,
    physical_max_depth: 50,
    useful_fraction: Fraction {
        numerator: 90,
        denominator: 100,
    },
    useful_depth_of_reference: Fraction {
        numerator: 80,
        denominator: 100,
    },
    min_unit_depth: 5,
    settled_fraction: Fraction {
        numerator: 90,
        denominator: 100,
    },
    settled_horizon_day: 730,
    benign_median_settled_day: 180,
    retention_fraction: Fraction {
        numerator: 90,
        denominator: 100,
    },
    retention_max_loss: 1,
};

object_only! {
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Registration {
        pub id: String,
        pub version: u32,
        pub lambda_per_hour: Fraction,
        pub slopes: Vec<f64>,
        pub seeds: Seeds,
        pub holdout: Holdout,
        pub calendar: Calendar,
        pub scenarios: Vec<Scenario>,
        pub criteria: Criteria,
        pub run_profiles: Vec<RunProfileSpec>,
        pub budgets: Budgets,
        pub resources: Resources,
        pub tolerance: Tolerance,
        pub unit_modes: Vec<UnitModeSpec>,
        pub sources: Sources,
        pub commands: Commands,
        pub streams: Streams,
        pub continuation: Continuation,
        pub notes: Vec<String>,
    }

    /// An exact nonnegative rational; thresholds compare by cross-multiplication.
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Fraction {
        pub numerator: u32,
        pub denominator: u32,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Seeds {
        pub dev: SeedRange,
        pub holdout: SeedRange,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct SeedRange {
        pub first: u64,
        pub count: u32,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Holdout {
        /// The role permitted to unseal the holdout.
        pub authorized_by: String,
        pub procedure: String,
        pub command: String,
        pub confirmations_per_slope: u32,
        /// A started confirmation is spent, including an interrupted one.
        pub spent_when_started: bool,
        /// Per-seed truth, partial tables or outcomes may not be inspected before the freeze.
        pub inspection_before_freeze: bool,
    }
}

object_only! {
    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Calendar {
        pub first_day: u32,
        pub days: u32,
        pub daily_active_hours: u32,
        pub night_hours: u32,
        /// Zero-based weekdays with an operator night, ascending and distinct.
        pub night_weekdays: Vec<u8>,
        pub year_days: u32,
        /// One-based day whose active use first sees the new firmware.
        pub firmware_change_day: u32,
        pub sleep: String,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Scenario {
        pub name: String,
        pub description: String,
        pub topology: Topology,
        /// Ordered active-day mix shared by the truth reference and candidate exposure.
        pub daily_mix: Vec<MixEntry>,
        pub epochs: Vec<TruthEpoch>,
        pub transition: Transition,
        pub parameters: Vec<Parameter>,
        pub coverage: Coverage,
        pub benign: bool,
        pub retention: bool,
        /// Extra minimum retained depth on every unit, beyond the general useful-depth rule.
        pub unit_depth_floor: Option<u8>,
        /// Explicit modes; absent means every registered mode.
        pub modes: Option<Vec<UnitMode>>,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Topology {
        pub ccds: u8,
        pub cores_per_ccd: u8,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct MixEntry {
        pub class: String,
        pub seconds: u32,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct TruthEpoch {
        pub id: String,
        /// One-based first day; the epoch runs until the next epoch or the horizon.
        pub start_day: u32,
        pub context: String,
        pub description: String,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Parameter {
        pub name: String,
        pub value: f64,
    }
}

object_only! {
    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Criteria {
        pub final_in_admissible_fraction: Fraction,
        pub held_after_finish_fraction: Fraction,
        pub safety_grace_days: u32,
        /// Per complete seed, post-finish H/T must not exceed this multiple of λ*.
        pub post_finish_rate_of_lambda: Fraction,
        pub poisson_diagnostic_confidence: Fraction,
        pub allowance_limit: u32,
        pub first_step_clean_stock_hours: u32,
        pub clean_hold_hours: u32,
        pub restoration_clean_stock_hours: u32,
        /// Ceiling C = min(U + margin, 0) in CO, retained as depth.
        pub ceiling_margin: u8,
        pub operational_max_depth: u8,
        pub physical_max_depth: u8,
        pub useful_fraction: Fraction,
        /// Required unit-depth sum is the ceiling of this fraction of M.
        pub useful_depth_of_reference: Fraction,
        pub min_unit_depth: u8,
        pub settled_fraction: Fraction,
        pub settled_horizon_day: u32,
        pub benign_median_settled_day: u32,
        pub retention_fraction: Fraction,
        pub retention_max_loss: u8,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct RunProfileSpec {
        pub profile: RunProfile,
        pub splits: Vec<Split>,
        pub seeds: SeedSelection,
        pub slopes_per_invocation: u32,
        pub gate_verdict: bool,
        pub label: Option<String>,
    }

    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
    pub enum SeedSelection {
        First { count: u32 },
        All {},
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Budgets {
        pub reduced_dev_seconds: u32,
        pub registered_seconds: u32,
        pub holdout_seconds: u32,
        pub compilation_excluded: bool,
    }

    #[derive(Clone, Debug, Eq, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Resources {
        pub jobs: u32,
        pub resource_class: String,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Tolerance {
        /// Absolute tolerance on hazard ratios against 1.
        pub admissibility: f64,
        /// Absolute tolerance on hour and day accounting.
        pub hours: f64,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct UnitModeSpec {
        pub mode: UnitMode,
        pub grouping: String,
    }
}

object_only! {
    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Sources {
        pub simulator_revision: String,
        pub report_revision: String,
        pub binary_sha256: Option<String>,
        pub input_sha256: Option<String>,
        pub policies: Vec<PolicySource>,
    }

    #[derive(Clone, Debug, PartialEq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct PolicySource {
        /// The selector name, such as `successor`; never an unversioned moving alias.
        pub name: String,
        /// Resolved identity printed and recorded in every run's provenance.
        pub identity: String,
        pub role: PolicyRole,
        pub source_revision: String,
        pub source_sha256: String,
        pub parameters_sha256: String,
        pub parameters: Vec<Parameter>,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Commands {
        pub reduced_dev: String,
        pub registered: String,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Streams {
        pub domains: Vec<StreamDomain>,
        pub topology_permutation: String,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct StreamDomain {
        pub name: String,
        pub tag: String,
    }

    #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Continuation {
        /// Deterministic rule for resuming an expired development invocation.
        pub dev_rule: String,
        pub holdout_resumable: bool,
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transition {
    Fixed,
    Moving,
    Firmware,
}

/// Covered scenarios expose every daily class at night; adversarial ones hide or delay a class.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Coverage {
    Covered,
    Adversarial,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PolicyRole {
    Candidate,
    Comparator,
}

/// One expected run record.
#[derive(Clone, Debug, PartialEq)]
pub struct Cell {
    pub scenario: String,
    pub seed: u64,
    pub unit_mode: UnitMode,
    pub slope: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("decode successor registration")]
    Decode(#[from] serde_json::Error),
    #[error("invalid successor registration: {0}")]
    Invalid(&'static str),
    #[error("successor registration changes a settled value: {0}")]
    Settled(&'static str),
}

/// Lower-case hex SHA-256, the form of `registration_sha256` in run provenance.
pub fn sha256_hex(bytes: &[u8]) -> String {
    crate::suite::hex(&Sha256::digest(bytes))
}

impl Fraction {
    pub fn value(self) -> f64 {
        f64::from(self.numerator) / f64::from(self.denominator)
    }

    /// Whether `count / total >= self`, exactly. An empty denominator never meets a threshold.
    pub fn met_by(self, count: u64, total: u64) -> bool {
        total > 0
            && u128::from(count) * u128::from(self.denominator)
                >= u128::from(self.numerator) * u128::from(total)
    }

    /// `ceil(value * self)`, exactly.
    pub fn ceil_of(self, value: u32) -> u64 {
        (u64::from(value) * u64::from(self.numerator)).div_ceil(u64::from(self.denominator))
    }
}

impl SeedRange {
    pub fn values(self) -> Result<std::ops::RangeInclusive<u64>, Error> {
        let last = self
            .count
            .checked_sub(1)
            .and_then(|count| self.first.checked_add(u64::from(count)))
            .ok_or(Error::Invalid(
                "seed range must be nonempty and representable",
            ))?;
        Ok(self.first..=last)
    }
}

impl Registration {
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let registration: Self = serde_json::from_slice(bytes)?;
        registration.validate()?;
        Ok(registration)
    }

    pub fn lambda(&self) -> f64 {
        self.lambda_per_hour.value()
    }

    pub fn scenario(&self, name: &str) -> Option<&Scenario> {
        self.scenarios.iter().find(|scenario| scenario.name == name)
    }

    pub fn policy(&self, name: &str) -> Option<&PolicySource> {
        self.sources
            .policies
            .iter()
            .find(|policy| policy.name == name)
    }

    pub fn run_profile(&self, profile: RunProfile) -> Option<&RunProfileSpec> {
        self.run_profiles
            .iter()
            .find(|spec| spec.profile == profile)
    }

    /// Modes a scenario applies: its explicit list, otherwise every registered mode.
    pub fn modes(&self, scenario: &Scenario) -> Vec<UnitMode> {
        scenario
            .modes
            .clone()
            .unwrap_or_else(|| self.unit_modes.iter().map(|spec| spec.mode).collect())
    }

    /// The registered seeds for a split under a run profile, in ascending order.
    pub fn seeds(&self, split: Split, profile: RunProfile) -> Result<Vec<u64>, Error> {
        let spec = self
            .run_profile(profile)
            .ok_or(Error::Invalid("run profile not registered"))?;
        if !spec.splits.contains(&split) {
            return Err(Error::Invalid("split not registered for run profile"));
        }
        let range = match split {
            Split::Dev => self.seeds.dev,
            Split::Holdout => self.seeds.holdout,
        };
        let all = range.values()?;
        Ok(match spec.seeds {
            SeedSelection::All {} => all.collect(),
            SeedSelection::First { count } => all.take(count as usize).collect(),
        })
    }

    /// Every expected cell: scenario × seed × applicable mode × registered slope.
    pub fn cells(&self, split: Split, profile: RunProfile) -> Result<Vec<Cell>, Error> {
        let seeds = self.seeds(split, profile)?;
        let mut cells = Vec::new();
        for scenario in &self.scenarios {
            for unit_mode in self.modes(scenario) {
                for &slope in &self.slopes {
                    cells.extend(seeds.iter().map(|&seed| Cell {
                        scenario: scenario.name.clone(),
                        seed,
                        unit_mode,
                        slope,
                    }));
                }
            }
        }
        Ok(cells)
    }

    /// Elapsed-time budget for one policy/slope invocation, excluding compilation.
    pub fn budget_seconds(&self, split: Split, profile: RunProfile) -> u32 {
        match (profile, split) {
            (RunProfile::ReducedDev, _) => self.budgets.reduced_dev_seconds,
            (RunProfile::Registered, Split::Dev) => self.budgets.registered_seconds,
            (RunProfile::Registered, Split::Holdout) => self.budgets.holdout_seconds,
        }
    }

    pub fn validate(&self) -> Result<(), Error> {
        if blank(&self.id) || self.version != VERSION {
            return Err(Error::Settled("identity or version"));
        }
        if self.lambda_per_hour != LAMBDA_PER_HOUR {
            return Err(Error::Settled("lambda"));
        }
        if self.slopes.iter().any(|slope| !slope.is_finite()) {
            return Err(Error::Invalid("slopes must be finite"));
        }
        if self.slopes != SLOPES {
            return Err(Error::Settled("slopes"));
        }
        if self.seeds.dev != DEV_SEEDS || self.seeds.holdout != HOLDOUT_SEEDS {
            return Err(Error::Settled("seed ranges"));
        }
        self.validate_holdout()?;
        self.validate_calendar()?;
        if self.criteria != SETTLED_CRITERIA {
            return Err(Error::Settled("criteria"));
        }
        self.validate_profiles()?;
        self.validate_operations()?;
        self.validate_modes()?;
        self.validate_sources()?;
        self.validate_scenarios()?;
        if self.notes.iter().any(|note| note.trim().is_empty()) {
            return Err(Error::Invalid("notes must be nonempty"));
        }
        Ok(())
    }

    fn validate_holdout(&self) -> Result<(), Error> {
        let holdout = &self.holdout;
        if blank(&holdout.authorized_by) || blank(&holdout.procedure) {
            return Err(Error::Invalid("holdout authority and procedure"));
        }
        if !command(
            &holdout.command,
            &["--run-profile registered", "--split holdout"],
        ) {
            return Err(Error::Invalid("holdout command"));
        }
        if holdout.confirmations_per_slope != 1
            || !holdout.spent_when_started
            || holdout.inspection_before_freeze
        {
            return Err(Error::Settled("holdout once-only rule"));
        }
        Ok(())
    }

    fn validate_calendar(&self) -> Result<(), Error> {
        let calendar = &self.calendar;
        let weekdays = &calendar.night_weekdays;
        if weekdays.iter().any(|day| *day > 6) || !weekdays.windows(2).all(|pair| pair[0] < pair[1])
        {
            return Err(Error::Invalid(
                "night weekdays must be ascending, distinct and at most 6",
            ));
        }
        if blank(&calendar.sleep) {
            return Err(Error::Invalid("calendar sleep"));
        }
        let shape = (
            calendar.first_day,
            calendar.days,
            calendar.daily_active_hours,
            calendar.night_hours,
            weekdays.len(),
            calendar.year_days,
            calendar.firmware_change_day,
        );
        if shape != SETTLED_CALENDAR {
            return Err(Error::Settled("calendar"));
        }
        Ok(())
    }

    fn validate_profiles(&self) -> Result<(), Error> {
        let [reduced, registered] = self.run_profiles.as_slice() else {
            return Err(Error::Settled("run profiles"));
        };
        let reduced_ok = reduced.profile == RunProfile::ReducedDev
            && reduced.splits == [Split::Dev]
            && reduced.seeds
                == SeedSelection::First {
                    count: REDUCED_DEV_SEED_COUNT,
                }
            && reduced.slopes_per_invocation == 1
            && !reduced.gate_verdict
            && reduced.label.as_deref() == Some(REDUCED_DEV_LABEL);
        let registered_ok = registered.profile == RunProfile::Registered
            && registered.splits == [Split::Dev, Split::Holdout]
            && registered.seeds == SeedSelection::All {}
            && registered.slopes_per_invocation == 1
            && registered.gate_verdict
            && registered.label.is_none();
        if !reduced_ok || !registered_ok {
            return Err(Error::Settled("run profiles"));
        }
        Ok(())
    }

    fn validate_operations(&self) -> Result<(), Error> {
        let budgets = self.budgets;
        if budgets.reduced_dev_seconds != REDUCED_DEV_BUDGET_SECONDS
            || budgets.registered_seconds != REGISTERED_BUDGET_SECONDS
            || budgets.holdout_seconds != REGISTERED_BUDGET_SECONDS
            || !budgets.compilation_excluded
        {
            return Err(Error::Settled("budgets"));
        }
        if self.resources.jobs == 0 || blank(&self.resources.resource_class) {
            return Err(Error::Invalid("resources"));
        }
        for tolerance in [self.tolerance.admissibility, self.tolerance.hours] {
            if !tolerance.is_finite() || !(0.0..=MAX_TOLERANCE).contains(&tolerance) {
                return Err(Error::Invalid(
                    "tolerance must be finite, nonnegative and computational",
                ));
            }
        }
        if !command(
            &self.commands.reduced_dev,
            &["--run-profile reduced-dev", "--split dev"],
        ) || !command(
            &self.commands.registered,
            &["--run-profile registered", "--split dev"],
        ) {
            return Err(Error::Invalid("commands"));
        }
        let streams = &self.streams;
        if streams.domains.is_empty()
            || blank(&streams.topology_permutation)
            || streams
                .domains
                .iter()
                .any(|domain| blank(&domain.name) || blank(&domain.tag))
            || !unique(streams.domains.iter().map(|domain| &domain.name))
            || !unique(streams.domains.iter().map(|domain| &domain.tag))
        {
            return Err(Error::Invalid("stream domains"));
        }
        if blank(&self.continuation.dev_rule) {
            return Err(Error::Invalid("continuation rule"));
        }
        if self.continuation.holdout_resumable {
            return Err(Error::Settled("holdout cannot resume"));
        }
        Ok(())
    }

    fn validate_modes(&self) -> Result<(), Error> {
        if self.unit_modes.is_empty()
            || self.unit_modes.iter().any(|spec| blank(&spec.grouping))
            || !unique(self.unit_modes.iter().map(|spec| spec.mode))
        {
            return Err(Error::Invalid("unit modes"));
        }
        let expected = if self.id == REGISTRATION_ID {
            UnitMode::CcdUniform
        } else {
            UnitMode::PerCore
        };
        if self.unit_modes.len() != 1 || self.unit_modes[0].mode != expected {
            return Err(Error::Settled(
                "one unit mode per registration: the current id is ccd-uniform only; a per-core registration needs its own id",
            ));
        }
        Ok(())
    }

    fn validate_sources(&self) -> Result<(), Error> {
        let sources = &self.sources;
        if !revision(&sources.simulator_revision) || !revision(&sources.report_revision) {
            return Err(Error::Invalid("simulator and report revisions"));
        }
        if [&sources.binary_sha256, &sources.input_sha256]
            .into_iter()
            .flatten()
            .any(|hash| !sha256(hash))
        {
            return Err(Error::Invalid("binary and input hashes"));
        }
        let policies = &sources.policies;
        if policies.is_empty()
            || !unique(policies.iter().map(|policy| &policy.name))
            || !unique(policies.iter().map(|policy| &policy.identity))
        {
            return Err(Error::Invalid("policy names and identities must be unique"));
        }
        for policy in policies {
            if blank(&policy.name)
                || blank(&policy.identity)
                || !revision(&policy.source_revision)
                || !sha256(&policy.source_sha256)
                || !sha256(&policy.parameters_sha256)
            {
                return Err(Error::Invalid("policy source identity"));
            }
            if policy.parameters.is_empty() {
                return Err(Error::Invalid("policy parameters"));
            }
            parameters(&policy.parameters)?;
        }
        let mut candidates = policies
            .iter()
            .filter(|policy| policy.role == PolicyRole::Candidate);
        match (candidates.next(), candidates.next()) {
            (Some(candidate), None) if candidate.name == CANDIDATE_POLICY => Ok(()),
            _ => Err(Error::Invalid("exactly one candidate, named successor")),
        }
    }

    fn validate_scenarios(&self) -> Result<(), Error> {
        if !unique(self.scenarios.iter().map(|scenario| &scenario.name)) {
            return Err(Error::Invalid("scenario names must be unique"));
        }
        if HISTORICAL_SCENARIOS
            .iter()
            .any(|name| self.scenario(name).is_none())
        {
            return Err(Error::Settled("historical scenario families"));
        }
        let registered: BTreeSet<_> = self.unit_modes.iter().map(|spec| spec.mode).collect();
        for scenario in &self.scenarios {
            self.validate_scenario(scenario, &registered)?;
        }
        let benign: Vec<_> = self
            .scenarios
            .iter()
            .filter(|scenario| scenario.benign)
            .collect();
        let new_benign = benign
            .iter()
            .filter(|scenario| !HISTORICAL_SCENARIOS.contains(&scenario.name.as_str()))
            .count();
        if benign.len() != BENIGN_COUNT
            || new_benign != 1
            || BENIGN_NAMED
                .iter()
                .any(|name| !benign.iter().any(|scenario| scenario.name == *name))
        {
            return Err(Error::Settled("benign applicability"));
        }
        let retention: Vec<_> = self
            .scenarios
            .iter()
            .filter(|scenario| scenario.retention)
            .map(|scenario| scenario.name.as_str())
            .collect();
        if retention != RETENTION_SCENARIOS {
            return Err(Error::Settled("retention applicability"));
        }
        let fixed_transitions = [
            ("firmware-change", Transition::Firmware),
            ("moving-cap", Transition::Moving),
        ];
        if fixed_transitions.iter().any(|(name, transition)| {
            self.scenario(name)
                .is_none_or(|scenario| scenario.transition != *transition)
        }) {
            return Err(Error::Settled("truth transitions"));
        }
        Ok(())
    }

    fn validate_scenario(
        &self,
        scenario: &Scenario,
        registered: &BTreeSet<UnitMode>,
    ) -> Result<(), Error> {
        if blank(&scenario.name) || blank(&scenario.description) {
            return Err(Error::Invalid("scenario name and description"));
        }
        let topology = scenario.topology;
        if topology.ccds == 0
            || topology.cores_per_ccd == 0
            || u32::from(topology.ccds) * u32::from(topology.cores_per_ccd) > 16
        {
            return Err(Error::Invalid("scenario topology"));
        }
        let active_seconds = u64::from(self.calendar.daily_active_hours) * 3600;
        if scenario.daily_mix.is_empty()
            || scenario
                .daily_mix
                .iter()
                .any(|entry| blank(&entry.class) || entry.seconds == 0)
            || scenario
                .daily_mix
                .iter()
                .map(|entry| u64::from(entry.seconds))
                .sum::<u64>()
                != active_seconds
        {
            return Err(Error::Invalid("daily mix must fill the active day"));
        }
        let epochs = &scenario.epochs;
        if epochs
            .first()
            .is_none_or(|epoch| epoch.start_day != self.calendar.first_day)
            || !epochs
                .windows(2)
                .all(|pair| pair[0].start_day < pair[1].start_day)
            || epochs.iter().any(|epoch| {
                epoch.start_day > self.calendar.days
                    || blank(&epoch.id)
                    || blank(&epoch.context)
                    || blank(&epoch.description)
            })
            || !unique(epochs.iter().map(|epoch| &epoch.id))
        {
            return Err(Error::Invalid("truth epoch schedule"));
        }
        let schedule_ok = match scenario.transition {
            Transition::Fixed => epochs.len() == 1,
            Transition::Moving => epochs.len() >= 2,
            Transition::Firmware => {
                epochs.len() == 2 && epochs[1].start_day == self.calendar.firmware_change_day
            }
        };
        if !schedule_ok {
            return Err(Error::Invalid("truth epochs do not match the transition"));
        }
        parameters(&scenario.parameters)?;
        if scenario.benign && scenario.coverage != Coverage::Covered {
            return Err(Error::Invalid(
                "adversarial scenarios cannot carry the benign depth demand",
            ));
        }
        let floor = (scenario.name == NO_WRITE_LIMIT_SCENARIO).then_some(NO_WRITE_LIMIT_FLOOR);
        if scenario.unit_depth_floor != floor {
            return Err(Error::Settled("unit depth floor"));
        }
        if let Some(modes) = &scenario.modes
            && (modes.is_empty()
                || !unique(modes.iter())
                || modes.iter().any(|mode| !registered.contains(mode)))
        {
            return Err(Error::Invalid(
                "scenario modes must be distinct registered modes",
            ));
        }
        Ok(())
    }
}

fn blank(text: &str) -> bool {
    text.trim().is_empty()
}

fn unique<T: Ord>(items: impl IntoIterator<Item = T>) -> bool {
    let mut seen = BTreeSet::new();
    items.into_iter().all(|item| seen.insert(item))
}

fn lower_hex(text: &str, len: usize) -> bool {
    text.len() == len
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn revision(text: &str) -> bool {
    lower_hex(text, 40)
}

fn sha256(text: &str) -> bool {
    lower_hex(text, 64)
}

fn command(text: &str, required: &[&str]) -> bool {
    text.starts_with("just bench-day-night ")
        && text.contains("--registration ")
        && required.iter().all(|part| text.contains(part))
}

fn parameters(parameters: &[Parameter]) -> Result<(), Error> {
    if parameters
        .iter()
        .any(|parameter| blank(&parameter.name) || !parameter.value.is_finite())
        || !unique(parameters.iter().map(|parameter| &parameter.name))
    {
        return Err(Error::Invalid(
            "parameters must be named, unique and finite",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "registration_tests.rs"]
pub(crate) mod tests;
