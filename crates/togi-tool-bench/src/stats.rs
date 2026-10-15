pub(crate) fn float(n: usize) -> f64 {
    f64::from(u32::try_from(n).unwrap_or(u32::MAX))
}

/// The `p`-quantile of an ascending slice by linear interpolation between neighbouring ranks; 0 for no values.
pub(crate) fn percentile(sorted: &[f64], p: f64) -> f64 {
    let Some(last) = sorted.len().checked_sub(1) else {
        return 0.0;
    };
    let at = p * float(last);
    let low = at.floor();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "`at` lies in [0, len - 1], so its floor is a valid index"
    )]
    let low_index = low as usize;
    let high_index = (low_index + 1).min(last);
    sorted[low_index] + (sorted[high_index] - sorted[low_index]) * (at - low)
}

/// The share of trials a real fact answered; 0 for no trials.
pub(crate) fn share(real: u32, trials: u32) -> f64 {
    if trials == 0 {
        0.0
    } else {
        f64::from(real) / f64::from(trials)
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn a_percentile_interpolates_between_neighbouring_ranks() {
        let values = [1.0, 2.0, 4.0, 8.0];
        assert!((percentile(&values, 0.0) - 1.0).abs() < 1e-12);
        assert!((percentile(&values, 0.5) - 3.0).abs() < 1e-12);
        assert!((percentile(&values, 1.0) - 8.0).abs() < 1e-12);
        assert!((percentile(&values, 0.25) - 1.75).abs() < 1e-12);
    }

    #[test]
    fn a_percentile_of_nothing_is_zero_and_of_one_value_is_that_value() {
        assert!(percentile(&[], 0.5).abs() < f64::EPSILON);
        assert!((percentile(&[7.0], 0.975) - 7.0).abs() < f64::EPSILON);
    }

    #[test]
    fn a_share_of_no_trials_is_zero() {
        assert!(share(0, 0).abs() < f64::EPSILON);
        assert!((share(1, 4) - 0.25).abs() < f64::EPSILON);
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            rng_seed: proptest::test_runner::RngSeed::Fixed(0x6265_6e63_6873_7461),
            failure_persistence: None,
            ..ProptestConfig::default()
        })]

        #[test]
        fn a_percentile_lies_within_the_values_and_never_decreases(
            mut values in prop::collection::vec(-1.0e6_f64..1.0e6, 1..40),
            p in 0.0_f64..=1.0,
            q in 0.0_f64..=1.0,
        ) {
            values.sort_by(f64::total_cmp);
            let (low, high) = if p <= q { (p, q) } else { (q, p) };
            let (a, b) = (percentile(&values, low), percentile(&values, high));
            prop_assert!(a >= values[0] && b <= values[values.len() - 1]);
            prop_assert!(a <= b + 1e-9);
        }
    }
}
