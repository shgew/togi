#![expect(
    clippy::cast_precision_loss,
    reason = "event counts and hour totals are far below 2^52"
)]

//! Exact shares, the infinity-aware median and the one-sided Poisson diagnostic.

use serde::Serialize;

/// An exact count over its denominator; thresholds compare integers, never rounded ratios.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct Share {
    pub numerator: u64,
    pub denominator: u64,
}

impl Share {
    pub const fn new(numerator: u64, denominator: u64) -> Self {
        Self {
            numerator,
            denominator,
        }
    }

    pub fn count(items: impl IntoIterator<Item = bool>) -> Self {
        let mut result = Self::new(0, 0);
        for item in items {
            result.denominator += 1;
            result.numerator += u64::from(item);
        }
        result
    }

    /// Every counted item holds, and at least one item exists.
    pub const fn all(self) -> bool {
        self.denominator > 0 && self.numerator == self.denominator
    }
}

impl std::fmt::Display for Share {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.denominator == 0 {
            write!(f, "{}/0", self.numerator)
        } else {
            write!(
                f,
                "{}/{} ({:.1}%)",
                self.numerator,
                self.denominator,
                100.0 * self.numerator as f64 / self.denominator as f64
            )
        }
    }
}

/// Median with infinity for unconverged values; an even count averages the two middle values, so a
/// finite/infinite pair is infinite. An empty sample has no median.
pub fn median(values: &[f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    Some(if sorted.len() % 2 == 1 {
        sorted[middle]
    } else {
        f64::midpoint(sorted[middle - 1], sorted[middle])
    })
}

/// One-sided confidence level of the diagnostic.
pub const CONFIDENCE: f64 = 0.95;

/// The mean `mu` with `P(Poisson(mu) <= k) = 1 - confidence`: the one-sided upper confidence mean for
/// `k` observed events, by bisection on a monotone log-space tail.
pub fn poisson_upper_mean(k: u64, confidence: f64) -> f64 {
    let tail = 1.0 - confidence;
    let mut high = k as f64 + 4.0 * (k as f64 + 1.0).sqrt() + 8.0;
    while cumulative(k, high) > tail {
        high *= 2.0;
    }
    let mut low = 0.0_f64;
    for _ in 0..200 {
        let mid = f64::midpoint(low, high);
        if mid.to_bits() == low.to_bits() || mid.to_bits() == high.to_bits() {
            break;
        }
        if cumulative(k, mid) > tail {
            low = mid;
        } else {
            high = mid;
        }
    }
    high
}

/// `P(Poisson(mu) <= k)`, summing log terms `-mu + i ln mu - ln i!` with log-sum-exp so a large mean
/// does not underflow.
fn cumulative(k: u64, mu: f64) -> f64 {
    if mu <= 0.0 {
        return 1.0;
    }
    let log_mu = mu.ln();
    let mut term = -mu;
    let mut peak = term;
    let mut scaled_sum = 1.0;
    for i in 1..=k {
        term += log_mu - (i as f64).ln();
        if term <= peak {
            scaled_sum += (term - peak).exp();
        } else {
            scaled_sum = scaled_sum * (peak - term).exp() + 1.0;
            peak = term;
        }
    }
    (peak + scaled_sum.ln()).exp().min(1.0)
}

/// Upper one-sided Poisson rate for `k` events over `hours`; none without exposure.
///
/// Model-based diagnostic: it assumes a constant-rate Poisson process and is not a guarantee for an
/// adaptive policy.
pub fn poisson_upper_rate(k: u64, hours: f64) -> Option<f64> {
    (hours > 0.0 && hours.is_finite()).then(|| poisson_upper_mean(k, CONFIDENCE) / hours)
}
