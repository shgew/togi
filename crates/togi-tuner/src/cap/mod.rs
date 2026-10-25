//! Cap S, margin from measured slope (`docs/spec/tuner.md`, Cap S): exact Poisson upper rate bounds,
//! the profile-likelihood lower endpoint of a class's hazard slope, the CCD slope that folds those
//! endpoints, and the cap that projects every adequately exposed class's bound to the CCD's share of the
//! hazard budget.
//!
//! Every function is pure and takes its thresholds explicitly; the named constants are the spec values,
//! so the simulator can run the same rules under other fallback slopes.

mod bound;
mod limit;
mod slope;

use togi_machine::TargetOffset;

pub use bound::{Bound, Confidence, InvalidConfidence, chi_square_quantile, poisson_upper};
pub use limit::{
    Binding, Cap, CapLimits, EDGE_MARGIN, Refresh, Uncovered, UncoveredReason, Witness, cap,
    project,
};
pub use slope::{
    Beta, InvalidBeta, InvalidCutoff, LikelihoodCutoff, NoDecay, Slope, SlopeFit, fit_slope,
};

/// Eligible post-warm-up exposure in hours: finite and never negative.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Hours(f64);

impl From<Hours> for f64 {
    fn from(value: Hours) -> Self {
        value.0
    }
}

impl TryFrom<f64> for Hours {
    type Error = InvalidHours;

    fn try_from(value: f64) -> Result<Self, InvalidHours> {
        Self::new(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[error("exposure {0} hours is not a finite nonnegative number")]
pub struct InvalidHours(pub f64);

impl Hours {
    pub const ZERO: Self = Self(0.0);

    pub fn new(hours: f64) -> Result<Self, InvalidHours> {
        if hours.is_finite() && hours >= 0.0 {
            // `-0.0` passes the comparison; adding `+0.0` stores it as `+0.0`.
            Ok(Self(hours + 0.0))
        } else {
            Err(InvalidHours(hours))
        }
    }

    pub const fn get(self) -> f64 {
        self.0
    }

    /// Cells merged at one offset add their exposure; an overflowing sum stays the largest finite value.
    fn saturating_add(self, other: Self) -> Self {
        Self((self.0 + other.0).min(f64::MAX))
    }
}

/// A hazard rate in events per applied hour: finite and never negative.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Rate(f64);

impl From<Rate> for f64 {
    fn from(value: Rate) -> Self {
        value.0
    }
}

impl TryFrom<f64> for Rate {
    type Error = InvalidRate;

    fn try_from(value: f64) -> Result<Self, InvalidRate> {
        Self::per_hour(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[error("rate {0} per hour is not a finite nonnegative number")]
pub struct InvalidRate(pub f64);

impl Rate {
    /// λ*: the machine's incremental hazard budget, one crash per 3,000 applied hours.
    pub const MACHINE_BUDGET: Self = Self(1.0 / 3000.0);

    pub fn per_hour(rate: f64) -> Result<Self, InvalidRate> {
        if rate.is_finite() && rate >= 0.0 {
            Ok(Self(rate + 0.0))
        } else {
            Err(InvalidRate(rate))
        }
    }

    pub const fn get(self) -> f64 {
        self.0
    }
}

/// Whether a cell's exposure meets the spec's adequate-exposure threshold (`docs/spec/tuner.md`,
/// Adequate exposure) or is only partial evidence kept for the slope.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Exposure {
    Adequate,
    Partial,
}

/// The recorded purpose of the trial a cell's exposure came from. Only mapping exposure enters the cap's
/// rate projection; every purpose contributes to the slope (`docs/spec/tuner.md`, Cap S).
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Purpose {
    Mapping,
    Qualification,
    Reproduction,
    Control,
}

/// One class's raw observations at one tested offset from one purpose: its failures and eligible hours.
/// Several cells may share an offset; their failures and hours add.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    pub offset: TargetOffset,
    pub purpose: Purpose,
    pub failures: u32,
    pub hours: Hours,
    pub exposure: Exposure,
}

/// Offsets are in [-50, 0], so every one converts to a float exactly.
fn offset_value(offset: TargetOffset) -> f64 {
    match i32::try_from(offset.get()) {
        Ok(value) => f64::from(value),
        Err(_) => unreachable!("target offsets are in -50–0"),
    }
}
