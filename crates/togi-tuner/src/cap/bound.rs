use std::num::NonZeroU64;

use super::{Hours, Rate};

/// A one-sided Poisson upper rate bound: finite, or unbounded when the class has no exposure at all.
/// The variant order makes `Finite < Unbounded`.
#[derive(Clone, Copy, Debug, PartialEq, PartialOrd)]
pub enum Bound {
    Finite(Rate),
    Unbounded,
}

impl Bound {
    /// Whether the bound proves the rate is at most `limit`; an unbounded bound never does.
    pub fn within(self, limit: Rate) -> bool {
        match self {
            Self::Finite(rate) => rate <= limit,
            Self::Unbounded => false,
        }
    }

    /// The bound times a nonnegative factor. A product too large for a rate is unbounded.
    pub(super) fn scaled(self, factor: f64) -> Self {
        match self {
            Self::Finite(rate) => match Rate::per_hour(rate.get() * factor) {
                Ok(scaled) => Self::Finite(scaled),
                Err(_) => Self::Unbounded,
            },
            Self::Unbounded => Self::Unbounded,
        }
    }
}

/// A probability strictly between 0 and 1.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Confidence(f64);

#[derive(Clone, Copy, Debug, PartialEq, thiserror::Error)]
#[error("confidence {0} is not strictly between 0 and 1")]
pub struct InvalidConfidence(pub f64);

impl Confidence {
    /// The spec's one-sided 95% level.
    pub const ONE_SIDED_95: Self = Self(0.95);

    pub fn new(confidence: f64) -> Result<Self, InvalidConfidence> {
        if confidence > 0.0 && confidence < 1.0 {
            Ok(Self(confidence))
        } else {
            Err(InvalidConfidence(confidence))
        }
    }

    pub const fn get(self) -> f64 {
        self.0
    }
}

/// The exact one-sided 95% Poisson upper rate for `failures` in `hours`:
/// `chi_square_quantile(0.95, 2(k+1)) / (2T)`, which for `k = 0` is `-ln(0.05) / T`. No exposure
/// bounds nothing.
pub fn poisson_upper(failures: u32, hours: Hours) -> Bound {
    if hours.get() == 0.0 {
        return Bound::Unbounded;
    }
    let confidence = Confidence::ONE_SIDED_95;
    let numerator = if failures == 0 {
        -(1.0 - confidence.get()).ln()
    } else {
        chi_square_quantile(confidence, degrees_of_freedom(failures)) / 2.0
    };
    match Rate::per_hour(numerator / hours.get()) {
        Ok(rate) => Bound::Finite(rate),
        Err(_) => Bound::Unbounded,
    }
}

fn degrees_of_freedom(failures: u32) -> NonZeroU64 {
    NonZeroU64::new(2 * (u64::from(failures) + 1)).unwrap_or(NonZeroU64::MIN)
}

/// The `confidence` quantile of the chi-square distribution: the `x` with `P(a, x/2) = confidence`,
/// `a = df/2` and `P` the regularized lower incomplete gamma function, found by bisection on its
/// monotone cumulative distribution.
pub fn chi_square_quantile(confidence: Confidence, degrees_of_freedom: NonZeroU64) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "degrees of freedom stay below 2^34, far under the 2^53 float mantissa"
    )]
    let shape = degrees_of_freedom.get() as f64 / 2.0;
    let target = confidence.get();

    let mut low = 0.0_f64;
    let mut high = shape.max(1.0) * 2.0;
    // The distribution reaches any probability below 1 within a few multiples of its shape.
    for _ in 0..MAX_DOUBLINGS {
        if regularized_lower_gamma(shape, high) >= target {
            break;
        }
        low = high;
        high *= 2.0;
    }
    for _ in 0..MAX_BISECTIONS {
        let middle = low + (high - low) / 2.0;
        if middle <= low || middle >= high {
            break;
        }
        if regularized_lower_gamma(shape, middle) < target {
            low = middle;
        } else {
            high = middle;
        }
    }
    2.0 * (low + (high - low) / 2.0)
}

const MAX_DOUBLINGS: u32 = 64;
const MAX_BISECTIONS: u32 = 400;
const MAX_TERMS: u32 = 1_000_000;
const EPSILON: f64 = 1e-16;
const TINY: f64 = 1e-300;

/// `P(a, x)`: the series below `a + 1`, the continued fraction for the upper tail above it.
fn regularized_lower_gamma(shape: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    let prefactor = (shape * x.ln() - x - ln_gamma(shape)).exp();
    if x < shape + 1.0 {
        let mut term = 1.0 / shape;
        let mut sum = term;
        let mut denominator = shape;
        for _ in 0..MAX_TERMS {
            denominator += 1.0;
            term *= x / denominator;
            sum += term;
            if term < sum * EPSILON {
                break;
            }
        }
        (sum * prefactor).min(1.0)
    } else {
        (1.0 - prefactor * upper_fraction(shape, x)).max(0.0)
    }
}

/// The modified Lentz evaluation of the continued fraction for `Γ(a, x)` without its prefactor.
fn upper_fraction(shape: f64, point: f64) -> f64 {
    let mut step = point + 1.0 - shape;
    let mut ratio_c = 1.0 / TINY;
    let mut ratio_d = 1.0 / step;
    let mut fraction = ratio_d;
    for index in 1..=MAX_TERMS {
        let index = f64::from(index);
        let coefficient = -index * (index - shape);
        step += 2.0;
        ratio_d = coefficient * ratio_d + step;
        if ratio_d.abs() < TINY {
            ratio_d = TINY;
        }
        ratio_c = step + coefficient / ratio_c;
        if ratio_c.abs() < TINY {
            ratio_c = TINY;
        }
        ratio_d = 1.0 / ratio_d;
        let delta = ratio_d * ratio_c;
        fraction *= delta;
        if (delta - 1.0).abs() < EPSILON {
            break;
        }
    }
    fraction
}

/// `ln Γ(x)` for `x >= 0.5` by the Lanczos approximation (g = 7, n = 9), good to about 1e-15.
fn ln_gamma(x: f64) -> f64 {
    const G: f64 = 7.0;
    const COEFFICIENTS: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    let x = x - 1.0;
    let mut sum = COEFFICIENTS[0];
    for (index, coefficient) in COEFFICIENTS.iter().enumerate().skip(1) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "the index is at most 8, exact in a float"
        )]
        let offset = index as f64;
        sum += coefficient / (x + offset);
    }
    let t = x + G + 0.5;
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + sum.ln()
}
