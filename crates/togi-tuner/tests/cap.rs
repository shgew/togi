#![expect(
    clippy::unwrap_used,
    reason = "integration test fixtures and assertions panic on invalid evidence"
)]

//! Exact Poisson bounds used by daily evidence, through the public API.

use std::num::NonZeroU64;

use test_case::test_case;
use togi_tuner::cap::{Bound, Confidence, Hours, chi_square_quantile, poisson_upper};

fn hours(value: f64) -> Hours {
    Hours::new(value).unwrap()
}

fn finite(bound: Bound) -> f64 {
    match bound {
        Bound::Finite(rate) => rate.get(),
        Bound::Unbounded => panic!("expected a finite bound"),
    }
}

fn close(actual: f64, expected: f64, tolerance: f64) {
    assert!(
        (actual - expected).abs() <= tolerance * expected.abs(),
        "{actual} is not within {tolerance} of {expected}"
    );
}

#[test_case(2, 5.9915; "two degrees")]
#[test_case(4, 9.4877; "four degrees")]
#[test_case(10, 18.307; "ten degrees")]
#[test_case(40, 55.758; "forty degrees")]
#[test_case(1, 3.8415; "one degree")]
#[test_case(100, 124.34; "hundred degrees")]
fn chi_square_95_matches_the_table(degrees: u64, expected: f64) {
    let quantile = chi_square_quantile(Confidence::ONE_SIDED_95, NonZeroU64::new(degrees).unwrap());
    close(quantile, expected, 1e-4);
}

#[test]
fn chi_square_median_of_two_degrees_is_two_ln_two() {
    let median = Confidence::new(0.5).unwrap();
    close(
        chi_square_quantile(median, NonZeroU64::new(2).unwrap()),
        2.0 * 2.0_f64.ln(),
        1e-9,
    );
}

#[test]
fn no_failures_bound_is_minus_ln_005_over_hours() {
    close(
        finite(poisson_upper(0, hours(2.0))),
        -(0.05_f64).ln() / 2.0,
        1e-12,
    );
}

#[test]
fn failures_bound_is_chi_square_over_twice_hours() {
    // chi-square(0.95, 4) = 9.4877; /(2 * 1 h).
    close(finite(poisson_upper(1, hours(1.0))), 9.4877 / 2.0, 1e-4);
}

#[test_case(0; "no failures")]
#[test_case(3; "failures")]
fn zero_hours_is_an_unbounded_bound(failures: u32) {
    assert_eq!(poisson_upper(failures, Hours::ZERO), Bound::Unbounded);
}

#[test]
fn a_float_beyond_the_range_is_refused_as_exposure() {
    assert!(Hours::new(-1.0).is_err());
    assert!(Hours::new(f64::INFINITY).is_err());
    assert!(Hours::new(f64::NAN).is_err());
}
