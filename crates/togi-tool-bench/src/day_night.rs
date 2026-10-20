//! Preregistered day/night experiments, separate from the legacy session benchmark.
//!
//! Comparators are diagnostic. The real day/night policy plugs into `togi-simrun`; this module
//! owns the frozen experiment and evaluates its per-scenario tails without pooling CCDs.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use togi_machine::object_only;

object_only! {
    #[derive(Debug, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Registration {
        pub id: String,
        pub version: u32,
        pub policy_status: String,
        pub lambda_per_hour: f64,
        pub policy_slopes: Vec<f64>,
        pub seeds: Seeds,
        pub calendar: RegisteredCalendar,
        pub thresholds: Thresholds,
        pub censored: String,
        pub tail_criteria: String,
        pub scenarios: Vec<RegisteredScenario>,
        pub notes: Vec<String>,
    }

    #[derive(Debug, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Seeds {
        pub dev: SeedRange,
        pub holdout: SeedRange,
    }

    #[derive(Debug, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct SeedRange {
        pub first: u64,
        pub count: u32,
    }

    #[derive(Debug, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct RegisteredCalendar {
        pub days: u32,
        pub daily_hours: u32,
        pub night_hours: u32,
        pub night_weekdays: Vec<u8>,
        pub year_days: u32,
        pub firmware_change_day: u32,
        pub sleep: String,
    }

    #[derive(Debug, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct Thresholds {
        pub covered_crash_free_fraction: f64,
        pub hidden_year_one_max_crashes_per_ccd: u32,
        pub hidden_year_one_fraction: f64,
        pub overshoot_one_count_fraction: f64,
        pub overshoot_absolute_max_counts: u8,
        pub median_convergence_days: u32,
        pub retention_max_lost_counts: u8,
        pub retention_fraction: f64,
        pub fixed_cap_max_steps_per_ccd: u32,
    }

    #[derive(Debug, Serialize)]
    #[serde(deny_unknown_fields)]
    pub struct RegisteredScenario {
        pub name: String,
        pub daily_class: DailyCoverage,
        pub cap: CapKind,
        #[serde(default)]
        pub retention: bool,
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DailyCoverage {
    Covered,
    Hidden,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CapKind {
    Fixed,
    Moving,
    Firmware,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("read day/night registration {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("decode day/night registration")]
    Decode(#[from] serde_json::Error),
    #[error("invalid day/night registration: {0}")]
    Invalid(&'static str),
}

impl Registration {
    pub fn load(path: &Path) -> Result<Self, Error> {
        let bytes = std::fs::read(path).map_err(|source| Error::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&bytes)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let registration: Self = serde_json::from_slice(bytes)?;
        registration.validate()?;
        Ok(registration)
    }

    fn validate(&self) -> Result<(), Error> {
        if self.id.is_empty() || self.version != 1 || self.scenarios.is_empty() {
            return Err(Error::Invalid("identity, version or scenarios"));
        }
        if !self.lambda_per_hour.is_finite() || self.lambda_per_hour <= 0.0 {
            return Err(Error::Invalid("lambda must be finite and positive"));
        }
        if self.policy_slopes.is_empty()
            || self
                .policy_slopes
                .iter()
                .any(|slope| !slope.is_finite() || *slope <= 1.0)
        {
            return Err(Error::Invalid(
                "policy slopes must be finite and greater than one",
            ));
        }
        let dev = self.seeds.dev.end()?;
        let holdout = self.seeds.holdout.end()?;
        if self.seeds.dev.first <= holdout && self.seeds.holdout.first <= dev {
            return Err(Error::Invalid("dev and holdout overlap"));
        }
        let calendar = &self.calendar;
        if calendar.days == 0
            || calendar.year_days == 0
            || calendar.year_days.checked_mul(2) != Some(calendar.days)
            || calendar.daily_hours == 0
            || calendar.night_hours == 0
            || calendar
                .daily_hours
                .checked_add(calendar.night_hours)
                .is_none_or(|hours| hours > 24)
            || calendar.night_weekdays.is_empty()
            || calendar.night_weekdays.iter().any(|day| *day > 6)
            || calendar
                .night_weekdays
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                != calendar.night_weekdays.len()
            || calendar.firmware_change_day >= calendar.days
        {
            return Err(Error::Invalid("calendar"));
        }
        let thresholds = &self.thresholds;
        for fraction in [
            thresholds.covered_crash_free_fraction,
            thresholds.hidden_year_one_fraction,
            thresholds.overshoot_one_count_fraction,
            thresholds.retention_fraction,
        ] {
            if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
                return Err(Error::Invalid("tail fractions must be in [0, 1]"));
            }
        }
        let mut names = BTreeSet::new();
        if self
            .scenarios
            .iter()
            .any(|scenario| scenario.name.is_empty() || !names.insert(&scenario.name))
        {
            return Err(Error::Invalid("scenario names must be nonempty and unique"));
        }
        Ok(())
    }
}

impl SeedRange {
    fn end(&self) -> Result<u64, Error> {
        self.count
            .checked_sub(1)
            .and_then(|count| self.first.checked_add(u64::from(count)))
            .ok_or(Error::Invalid(
                "seed ranges must be nonempty and representable",
            ))
    }

    pub fn values(&self) -> Result<std::ops::RangeInclusive<u64>, Error> {
        Ok(self.first..=self.end()?)
    }
}
