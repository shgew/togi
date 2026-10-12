use serde::Serialize;

/// The central 99% of a binomial distribution: the least and greatest failure counts that are not in a
/// tail holding 0.5% of the probability. Both ends are inclusive.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct Interval {
    pub low: u32,
    pub high: u32,
}

impl Interval {
    pub const fn contains(self, count: u32) -> bool {
        self.low <= count && count <= self.high
    }
}

const TAIL: f64 = 0.005;
const BISECTIONS: u32 = 52;

/// `ln C(n, k)` for each `k` in `0..=n`, from a compensated table of `ln(i!)`.
pub(crate) fn log_choose(n: u32) -> Vec<f64> {
    let mut factorial = Vec::with_capacity(n as usize + 1);
    let (mut sum, mut lost) = (0.0_f64, 0.0_f64);
    factorial.push(0.0);
    for i in 1..=n {
        let term = f64::from(i).ln();
        let next = sum + term;
        lost += if sum.abs() >= term.abs() {
            (sum - next) + term
        } else {
            (term - next) + sum
        };
        sum = next;
        factorial.push(sum + lost);
    }
    let whole = factorial[n as usize];
    (0..=n)
        .map(|k| whole - factorial[k as usize] - factorial[(n - k) as usize])
        .collect()
}

/// The interval of `Binomial(n, p)`; `weights` is scratch of length `n + 1` and `choose` is
/// `log_choose(n)`.
pub(crate) fn interval_with_weights(
    n: u32,
    p: f64,
    weights: &mut [f64],
    choose: &[f64],
) -> Interval {
    if p <= 0.0 {
        return Interval { low: 0, high: 0 };
    }
    if p >= 1.0 {
        return Interval { low: n, high: n };
    }
    let (log_p, log_q) = (p.ln(), (-p).ln_1p());
    let mut largest = f64::NEG_INFINITY;
    for (k, weight) in (0..=n).zip(weights.iter_mut()) {
        *weight = choose[k as usize] + f64::from(k) * log_p + f64::from(n - k) * log_q;
        largest = largest.max(*weight);
    }
    let mut total = 0.0;
    for weight in weights.iter_mut() {
        *weight = (*weight - largest).exp();
        total += *weight;
    }
    let mut interval = Interval { low: n, high: n };
    let mut cumulative = 0.0;
    let mut lower_found = false;
    for (k, weight) in (0..=n).zip(weights.iter()) {
        cumulative += weight;
        if !lower_found && cumulative >= TAIL * total {
            interval.low = k;
            lower_found = true;
        }
        if cumulative >= (1.0 - TAIL) * total {
            interval.high = k;
            break;
        }
    }
    interval
}

/// The closed range of failure probabilities whose interval holds `k` of `n`: at `p` outside it, the
/// observed count falls outside the interval. `choose` is `log_choose(n)`.
pub(crate) fn probability_bounds(n: u32, k: u32, choose: &[f64]) -> (f64, f64) {
    let mut weights = vec![0.0; n as usize + 1];
    let mut bounds = (0.0, 1.0);
    if k > 0 {
        let (mut low, mut high) = (0.0, 1.0);
        for _ in 0..BISECTIONS {
            let mid = f64::midpoint(low, high);
            if interval_with_weights(n, mid, &mut weights, choose).high < k {
                low = mid;
            } else {
                high = mid;
            }
        }
        bounds.0 = high;
    }
    if k < n {
        let (mut low, mut high) = (0.0, 1.0);
        for _ in 0..BISECTIONS {
            let mid = f64::midpoint(low, high);
            if interval_with_weights(n, mid, &mut weights, choose).low > k {
                high = mid;
            } else {
                low = mid;
            }
        }
        bounds.1 = low;
    }
    bounds
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use proptest::test_runner::RngSeed;

    fn binomial_interval(n: u32, p: f64) -> Interval {
        let mut weights = vec![0.0; n as usize + 1];
        interval_with_weights(n, p, &mut weights, &log_choose(n))
    }

    fn interval(low: u32, high: u32) -> Interval {
        Interval { low, high }
    }

    #[test]
    fn known_intervals_hold_at_the_boundaries_of_p() {
        for (n, p, want) in [
            (10, 0.0, interval(0, 0)),
            (10, 1e-12, interval(0, 0)),
            (10, 1.0, interval(10, 10)),
            (10, 1.0 - 1e-12, interval(10, 10)),
            (10, 0.5, interval(1, 9)),
            (10, 0.1, interval(0, 4)),
            (10_000, 0.5, interval(4871, 5129)),
        ] {
            assert_eq!(binomial_interval(n, p), want, "n={n} p={p}");
        }
    }

    #[test]
    fn log_choose_matches_exact_binomial_coefficients() {
        let choose = log_choose(20);
        let mut exact = 1.0_f64;
        for (k, value) in (0_u32..=20).zip(&choose) {
            assert!(
                (value - exact.ln()).abs() < 1e-12,
                "k={k}: {value} vs {}",
                exact.ln()
            );
            exact = exact * f64::from(20 - k) / f64::from(k + 1);
        }
    }

    #[test]
    fn bounds_of_a_count_at_either_end_are_open_on_that_side() {
        let choose = log_choose(12);
        assert_eq!(probability_bounds(12, 0, &choose).0, 0.0);
        assert_eq!(probability_bounds(12, 12, &choose).1, 1.0);
        let (low, high) = probability_bounds(12, 5, &choose);
        assert!(0.0 < low && low < 5.0 / 12.0 && 5.0 / 12.0 < high && high < 1.0);
    }

    fn fixed() -> ProptestConfig {
        ProptestConfig {
            cases: 256,
            rng_seed: RngSeed::Fixed(0x6d6f_6465_6c63_686b),
            failure_persistence: None,
            ..ProptestConfig::default()
        }
    }

    proptest! {
        #![proptest_config(fixed())]

        #[test]
        fn the_interval_is_ordered_inside_n_and_holds_the_mean(n in 1_u32..400, p in 0.0_f64..1.0) {
            let got = binomial_interval(n, p);
            let mean = f64::from(n) * p;
            prop_assert!(got.low <= got.high && got.high <= n);
            prop_assert!(f64::from(got.low) <= mean.ceil() && f64::from(got.high) >= mean.floor());
        }

        #[test]
        fn a_count_is_inside_the_interval_exactly_when_p_is_inside_its_bounds(
            n in 10_u32..80,
            pick in 0_u32..80,
            p in 0.0_f64..1.0,
        ) {
            let k = pick % (n + 1);
            let choose = log_choose(n);
            let (low, high) = probability_bounds(n, k, &choose);
            let mut weights = vec![0.0; n as usize + 1];
            let inside = interval_with_weights(n, p, &mut weights, &choose).contains(k);
            prop_assert_eq!(inside, low <= p && p <= high);
        }
    }
}
