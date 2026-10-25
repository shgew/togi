use std::collections::{BTreeMap, BTreeSet};

use togi_machine::TargetOffset;

use super::{Cell, offset_value};

/// The log-hazard decay per count of depth in `rate(o) = exp(a - beta*o)`. Offsets are at most 0 and
/// deeper is more negative, so a positive beta means the hazard rises with depth. Finite, of any sign.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Beta(f64);

impl From<Beta> for f64 {
    fn from(value: Beta) -> Self {
        value.0
    }
}

impl TryFrom<f64> for Beta {
    type Error = InvalidBeta;

    fn try_from(value: f64) -> Result<Self, InvalidBeta> {
        Self::new(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[error("slope {0} is not finite")]
pub struct InvalidBeta(pub f64);

impl Beta {
    /// The fallback slope `ln 1.5` used before any class has failures at two offsets.
    pub const FALLBACK: Self = Self(0.405_465_108_108_164_4);

    pub fn new(beta: f64) -> Result<Self, InvalidBeta> {
        if beta.is_finite() {
            Ok(Self(beta))
        } else {
            Err(InvalidBeta(beta))
        }
    }

    pub const fn get(self) -> f64 {
        self.0
    }

    fn min(self, other: Self) -> Self {
        Self(self.0.min(other.0))
    }
}

/// The twice-log-likelihood loss a profile-likelihood interval admits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LikelihoodCutoff(f64);

#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[error("likelihood cutoff {0} is not a positive finite number")]
pub struct InvalidCutoff(pub f64);

impl LikelihoodCutoff {
    /// The spec's rounded-up chi-square 95% cutoff.
    pub const CHI_SQUARE_95: Self = Self(3.842);

    pub fn new(cutoff: f64) -> Result<Self, InvalidCutoff> {
        if cutoff.is_finite() && cutoff > 0.0 {
            Ok(Self(cutoff))
        } else {
            Err(InvalidCutoff(cutoff))
        }
    }

    pub const fn get(self) -> f64 {
        self.0
    }
}

/// Why a fitted class justifies no decay at all, which allows only CO 0.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NoDecay {
    /// The interval's lower endpoint could not be bounded.
    Unbounded,
    /// The lower endpoint is zero or negative.
    Nonpositive(Beta),
}

/// One class's slope evidence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SlopeFit {
    /// Failures at fewer than two distinct offsets with positive exposure: nothing to fit.
    Unmeasured,
    /// The maximum-likelihood slope and the lower endpoint of its two-sided interval, which is positive.
    Measured {
        estimate: Beta,
        lower: Beta,
    },
    NoJustifiedDecay(NoDecay),
}

/// The CCD's slope, labelled as the spec requires.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Slope {
    /// No class has failures at two offsets yet; the parameter is `ln 1.5` by default.
    Fallback(Beta),
    /// The minimum lower endpoint over every measured class and every earlier measurement: it can only
    /// fall, from the first measurement on.
    Measured { lower: Beta },
    /// A measurement supplied no positive lower endpoint. It stays, so the cap stays CO 0.
    NoJustifiedDecay(NoDecay),
}

impl Slope {
    /// The default fallback, `ln 1.5`.
    pub const FALLBACK: Self = Self::Fallback(Beta::FALLBACK);

    /// The slope to project with; none when the evidence justifies no decay.
    pub fn beta(self) -> Option<Beta> {
        match self {
            Self::Fallback(beta) | Self::Measured { lower: beta } => Some(beta),
            Self::NoJustifiedDecay(_) => None,
        }
    }

    /// Folds one class's fit into the slope. The first measurement replaces the fallback, flatter or
    /// steeper; later ones take the minimum and never raise it.
    #[must_use]
    pub fn absorb(self, fit: SlopeFit) -> Self {
        match (self, fit) {
            (Self::NoJustifiedDecay(no_decay), _) => Self::NoJustifiedDecay(no_decay),
            (Self::Fallback(_) | Self::Measured { .. }, SlopeFit::NoJustifiedDecay(no_decay)) => {
                Self::NoJustifiedDecay(no_decay)
            }
            (slope @ (Self::Fallback(_) | Self::Measured { .. }), SlopeFit::Unmeasured) => slope,
            (Self::Fallback(_), SlopeFit::Measured { lower, .. }) => Self::Measured { lower },
            (Self::Measured { lower: earlier }, SlopeFit::Measured { lower, .. }) => {
                Self::Measured {
                    lower: earlier.min(lower),
                }
            }
        }
    }

    /// Absorbs the fit of every class, of any purpose, in `classes`.
    #[must_use]
    pub fn measure<K>(self, classes: &BTreeMap<K, Vec<Cell>>, cutoff: LikelihoodCutoff) -> Self {
        classes
            .values()
            .fold(self, |slope, cells| slope.absorb(fit_slope(cells, cutoff)))
    }
}

/// One offset's merged exposure, in the form the likelihood needs.
struct Point {
    offset: f64,
    failures: f64,
    ln_hours: f64,
}

/// Fits `rate(o) = exp(a - beta*o)` by Poisson likelihood to the offsets with positive exposure; cells at
/// one offset merge first, so a warm-up failure counts with the same offset's later positive hours. Profiling
/// out `a` (`exp(a) = Σk / Σ T exp(-beta o)`) leaves
/// `loglik(beta) = -K ln Σ T_i exp(-beta o_i) - beta Σ k_i o_i` up to a constant, which is concave, so
/// the maximum and both ends of the interval where twice the loss is at most `cutoff` bracket cleanly.
/// Returns the lower endpoint.
pub fn fit_slope(cells: &[Cell], cutoff: LikelihoodCutoff) -> SlopeFit {
    let mut merged: BTreeMap<TargetOffset, (u64, f64)> = BTreeMap::new();
    for cell in cells {
        let entry = merged.entry(cell.offset).or_insert((0, 0.0));
        entry.0 += u64::from(cell.failures);
        entry.1 = (entry.1 + cell.hours.get()).min(f64::MAX);
    }
    merged.retain(|_, (_, hours)| *hours > 0.0);
    let failing: BTreeSet<TargetOffset> = merged
        .iter()
        .filter(|(_, (failures, _))| *failures > 0)
        .map(|(&offset, _)| offset)
        .collect();
    if failing.len() < 2 {
        return SlopeFit::Unmeasured;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "failure counts stay far below the 2^53 float mantissa"
    )]
    let points: Vec<Point> = merged
        .iter()
        .map(|(&offset, &(failures, hours))| Point {
            offset: offset_value(offset),
            failures: failures as f64,
            ln_hours: hours.ln(),
        })
        .collect();
    let model = Model::new(&points);
    let Some(estimate) = model.maximum() else {
        return SlopeFit::NoJustifiedDecay(NoDecay::Unbounded);
    };
    let Some(lower) = model.lower_endpoint(estimate, cutoff.get()) else {
        return SlopeFit::NoJustifiedDecay(NoDecay::Unbounded);
    };
    match (Beta::new(estimate), Beta::new(lower)) {
        (Ok(estimate), Ok(lower)) if lower.get() > 0.0 => SlopeFit::Measured { estimate, lower },
        (_, Ok(lower)) => SlopeFit::NoJustifiedDecay(NoDecay::Nonpositive(lower)),
        (_, Err(_)) => SlopeFit::NoJustifiedDecay(NoDecay::Unbounded),
    }
}

/// The profile log-likelihood and its derivative over a class's points.
struct Model<'a> {
    points: &'a [Point],
    total_failures: f64,
    failure_offset_sum: f64,
}

/// Bracketing doubles a step this many times, far beyond any slope a depth range of 50 can produce.
const MAX_DOUBLINGS: u32 = 64;
const MAX_BISECTIONS: u32 = 400;

impl<'a> Model<'a> {
    fn new(points: &'a [Point]) -> Self {
        Self {
            points,
            total_failures: points.iter().map(|point| point.failures).sum(),
            failure_offset_sum: points
                .iter()
                .map(|point| point.failures * point.offset)
                .sum(),
        }
    }

    /// `ln Σ T_i exp(-beta o_i)`, stably.
    fn log_exposure(&self, beta: f64) -> f64 {
        let terms = |point: &Point| point.ln_hours - beta * point.offset;
        let largest = self
            .points
            .iter()
            .map(terms)
            .fold(f64::NEG_INFINITY, f64::max);
        largest
            + self
                .points
                .iter()
                .map(|point| (terms(point) - largest).exp())
                .sum::<f64>()
                .ln()
    }

    fn log_likelihood(&self, beta: f64) -> f64 {
        -self.total_failures * self.log_exposure(beta) - beta * self.failure_offset_sum
    }

    /// The derivative of the log-likelihood: K times the exposure-weighted mean offset, less the sum of
    /// failure offsets. It falls as beta rises.
    fn slope_of_likelihood(&self, beta: f64) -> f64 {
        let log_total = self.log_exposure(beta);
        let mean: f64 = self
            .points
            .iter()
            .map(|point| point.offset * (point.ln_hours - beta * point.offset - log_total).exp())
            .sum();
        self.total_failures * mean - self.failure_offset_sum
    }

    /// The maximum-likelihood beta; with failures at two offsets the mean failure offset lies strictly
    /// inside the tested offsets, so it is finite.
    fn maximum(&self) -> Option<f64> {
        let mut rising = -1.0;
        let mut falling = 1.0;
        for _ in 0..MAX_DOUBLINGS {
            if self.slope_of_likelihood(rising) > 0.0 {
                break;
            }
            rising *= 2.0;
        }
        for _ in 0..MAX_DOUBLINGS {
            if self.slope_of_likelihood(falling) < 0.0 {
                break;
            }
            falling *= 2.0;
        }
        if self.slope_of_likelihood(rising) <= 0.0 || self.slope_of_likelihood(falling) >= 0.0 {
            return None;
        }
        Some(bisect(rising, falling, |beta| {
            self.slope_of_likelihood(beta) > 0.0
        }))
    }

    /// The beta below the maximum where twice the loss first exceeds `cutoff`, taking the side outside
    /// the interval so rounding never moves it toward a steeper slope.
    fn lower_endpoint(&self, estimate: f64, cutoff: f64) -> Option<f64> {
        let best = self.log_likelihood(estimate);
        let outside = |beta: f64| 2.0 * (best - self.log_likelihood(beta)) > cutoff;
        let mut step = 1.0;
        let mut candidate = estimate - step;
        for _ in 0..MAX_DOUBLINGS {
            if outside(candidate) {
                let found = bisect(estimate, candidate, |beta| !outside(beta));
                return Some(found);
            }
            step *= 2.0;
            candidate = estimate - step;
        }
        None
    }
}

/// Bisects between `start`, where `keeps` holds, and `end`, where it fails, and returns the last point
/// where it failed.
fn bisect(start: f64, end: f64, keeps: impl Fn(f64) -> bool) -> f64 {
    let mut inside = start;
    let mut outside = end;
    for _ in 0..MAX_BISECTIONS {
        let middle = inside + (outside - inside) / 2.0;
        let between = middle > inside.min(outside) && middle < inside.max(outside);
        if !between {
            break;
        }
        if keeps(middle) {
            inside = middle;
        } else {
            outside = middle;
        }
    }
    outside
}
