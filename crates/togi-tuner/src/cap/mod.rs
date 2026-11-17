//! Exact Poisson upper rate bounds for daily evidence.

mod bound;

pub use bound::{Bound, Confidence, InvalidConfidence, chi_square_quantile, poisson_upper};
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
    /// The unchanged daily evidence goal: one crash per 3,000 applied hours.
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
