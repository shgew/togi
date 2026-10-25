#![expect(
    clippy::unwrap_used,
    reason = "integration test fixtures and assertions panic on invalid evidence"
)]

//! Cap S (`docs/spec/tuner.md`, Cap S: margin from measured slope), through the public API.

use std::collections::BTreeMap;
use std::num::{NonZeroU32, NonZeroU64};

use test_case::test_case;
use togi_machine::TargetOffset;
use togi_tuner::cap::{
    Beta, Binding, Bound, Cap, CapLimits, Cell, Confidence, Exposure, Hours, LikelihoodCutoff,
    NoDecay, Purpose, Rate, Refresh, Slope, SlopeFit, Uncovered, UncoveredReason, cap,
    chi_square_quantile, fit_slope, poisson_upper, project,
};

fn offset(value: i64) -> TargetOffset {
    TargetOffset::new(value).unwrap()
}

fn hours(value: f64) -> Hours {
    Hours::new(value).unwrap()
}

fn cell(at: i64, purpose: Purpose, failures: u32, time: f64, exposure: Exposure) -> Cell {
    Cell {
        offset: offset(at),
        purpose,
        failures,
        hours: hours(time),
        exposure,
    }
}

fn mapped(at: i64, failures: u32, time: f64) -> Cell {
    cell(at, Purpose::Mapping, failures, time, Exposure::Adequate)
}

fn finite(bound: Bound) -> f64 {
    match bound {
        Bound::Finite(rate) => rate.get(),
        Bound::Unbounded => panic!("expected a finite bound"),
    }
}

fn limits(ccds: u32) -> CapLimits {
    CapLimits::new(NonZeroU32::new(ccds).unwrap())
}

fn close(actual: f64, expected: f64, tolerance: f64) {
    assert!(
        (actual - expected).abs() <= tolerance * expected.abs(),
        "{actual} is not within {tolerance} of {expected}"
    );
}

fn classes(entries: Vec<(&'static str, Vec<Cell>)>) -> BTreeMap<&'static str, Vec<Cell>> {
    entries.into_iter().collect()
}

fn single(cells: Vec<Cell>) -> BTreeMap<&'static str, Vec<Cell>> {
    classes(vec![("r1", cells)])
}

fn full(
    map: &BTreeMap<&'static str, Vec<Cell>>,
    slope: Slope,
    edge: Option<i64>,
    ccds: u32,
) -> Cap<&'static str> {
    cap(map, slope, edge.map(offset), Refresh::Full, &limits(ccds))
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

/// By hand, two CCDs: the per-CCD budget is (1/3000)/2 = 1.6667e-4 per hour. One mapping class tested
/// to -20 for 1000 h with no failure has b(x) = 2.99573/1000 = 2.99573e-3. With the fallback slope
/// ln 1.5 = 0.405465 the projection at s is b(x) * exp(0.405465 * (-20 - s)), which is at most the
/// budget when (-20 - s) <= ln(1.6667e-4 / 2.99573e-3) / 0.405465 = -2.8892 / 0.405465 = -7.126, so
/// s >= -12.874. The deepest integer is -12: -13 projects to 1.6667e-4 * exp(0.405465 * 0.126) > budget.
#[test]
fn fallback_slope_cap_matches_the_hand_calculation() {
    let map = single(vec![mapped(-20, 0, 1000.0)]);
    let capped = full(&map, Slope::FALLBACK, Some(-30), 2);
    assert_eq!(capped.offset, offset(-12));
    assert_eq!(capped.binding, Binding::Rate { class: "r1" });
    assert_eq!(capped.slope, Slope::FALLBACK);
    assert_eq!(capped.witnesses.len(), 1);
    assert_eq!(capped.witnesses[0].offset, offset(-20));
    close(finite(capped.witnesses[0].bound), 2.995_73e-3, 1e-5);
}

#[test]
fn edge_plus_five_binds_when_shallower_than_the_rate_limit() {
    // U = -14 gives U + 5 = -9, shallower than the rate-limited -12.
    let map = single(vec![mapped(-20, 0, 1000.0)]);
    let capped = full(&map, Slope::FALLBACK, Some(-14), 2);
    assert_eq!(capped.offset, offset(-9));
    assert_eq!(
        capped.binding,
        Binding::Edge {
            rate_limited: offset(-12)
        }
    );
    assert_eq!(capped.edge, Some(offset(-14)));
}

#[test]
fn edge_margin_never_goes_deeper_than_the_rate_limit() {
    // U = -17 gives U + 5 = -12, equal to the rate limit, which then binds.
    let map = single(vec![mapped(-20, 0, 1000.0)]);
    let capped = full(&map, Slope::FALLBACK, Some(-17), 2);
    assert_eq!(capped.offset, offset(-12));
    assert_eq!(capped.binding, Binding::Rate { class: "r1" });
}

#[test]
fn edge_margin_is_clamped_at_zero() {
    // U = -2 gives min(3, 0) = 0.
    let map = single(vec![mapped(-20, 0, 1000.0)]);
    let capped = full(&map, Slope::FALLBACK, Some(-2), 2);
    assert_eq!(capped.offset, TargetOffset::ZERO);
    assert_eq!(
        capped.binding,
        Binding::Edge {
            rate_limited: offset(-12)
        }
    );
}

#[test]
fn unknown_edge_caps_at_zero() {
    let map = single(vec![mapped(-20, 0, 1000.0)]);
    let capped = full(&map, Slope::FALLBACK, None, 2);
    assert_eq!(capped.offset, TargetOffset::ZERO);
    assert_eq!(capped.binding, Binding::UnknownEdge);
}

#[test]
fn no_adequate_class_caps_at_zero() {
    let map = single(vec![cell(-20, Purpose::Mapping, 0, 5.0, Exposure::Partial)]);
    let capped = full(&map, Slope::FALLBACK, Some(-30), 2);
    assert_eq!(capped.offset, TargetOffset::ZERO);
    assert_eq!(capped.binding, Binding::NoAdequateClass);
    assert_eq!(
        capped.uncovered,
        vec![Uncovered {
            class: "r1",
            reason: UncoveredReason::MappingInadequate
        }]
    );
}

#[test]
fn unbounded_witness_leaves_nothing_nonzero() {
    let map = single(vec![mapped(-20, 0, 0.0)]);
    let capped = full(&map, Slope::FALLBACK, Some(-30), 2);
    assert_eq!(capped.offset, TargetOffset::ZERO);
    assert_eq!(capped.binding, Binding::Rate { class: "r1" });
    assert_eq!(capped.witnesses[0].bound, Bound::Unbounded);
}

#[test]
fn a_bound_below_the_budget_everywhere_reaches_the_deepest_offset() {
    // 10 million hours without failure: b = 3e-7, projected 50 counts deeper stays far below 1.7e-4.
    let map = single(vec![mapped(-45, 0, 1.0e7)]);
    let capped = full(&map, Slope::FALLBACK, Some(-60 + 10), 1);
    assert_eq!(capped.offset, offset(-45));
    assert_eq!(
        capped.binding,
        Binding::Edge {
            rate_limited: offset(-50)
        }
    );
}

#[test]
fn the_class_with_the_larger_projection_binds() {
    let map = classes(vec![
        ("strong", vec![mapped(-20, 0, 1000.0)]),
        ("weak", vec![mapped(-20, 0, 100.0)]),
    ]);
    let capped = full(&map, Slope::FALLBACK, Some(-40), 2);
    assert_eq!(capped.binding, Binding::Rate { class: "weak" });
    // The weak class alone: b = 2.99573e-2; (-20 - s) <= ln(1.6667e-4 / 2.99573e-2) / 0.405465 = -12.8 so
    // s >= -7.2 and the deepest integer is -7.
    assert_eq!(capped.offset, offset(-7));
}

#[test]
fn the_deepest_adequate_offset_is_the_witness() {
    let map = single(vec![
        cell(-30, Purpose::Mapping, 0, 5.0, Exposure::Partial),
        mapped(-20, 1, 500.0),
        mapped(-10, 0, 2000.0),
    ]);
    let capped = full(&map, Slope::FALLBACK, Some(-40), 2);
    assert_eq!(capped.witnesses[0].offset, offset(-20));
    assert_eq!(capped.witnesses[0].failures, 1);
    assert_eq!(capped.witnesses[0].hours, hours(500.0));
}

#[test]
fn mapping_cells_at_one_offset_add() {
    let map = single(vec![mapped(-20, 1, 400.0), mapped(-20, 0, 600.0)]);
    let capped = full(&map, Slope::FALLBACK, Some(-40), 2);
    assert_eq!(capped.witnesses[0].failures, 1);
    assert_eq!(capped.witnesses[0].hours, hours(1000.0));
}

#[test]
fn projection_goes_both_directions_from_x() {
    let map = single(vec![mapped(-20, 0, 1000.0)]);
    let capped = full(&map, Slope::FALLBACK, Some(-30), 2);
    let witness = &capped.witnesses[0];
    let at_x = finite(witness.bound);
    // The fallback slope is ln 1.5, so five counts shift the bound by 1.5^5 = 7.59375.
    close(
        finite(project(witness, Beta::FALLBACK, offset(-25))),
        at_x * 7.593_75,
        1e-9,
    );
    close(
        finite(project(witness, Beta::FALLBACK, offset(-15))),
        at_x / 7.593_75,
        1e-9,
    );
    close(
        finite(project(witness, Beta::FALLBACK, offset(-20))),
        at_x,
        1e-12,
    );
}

#[test]
fn uncovered_classes_are_reported_with_their_reason() {
    let map = classes(vec![
        ("covered", vec![mapped(-20, 0, 1000.0)]),
        ("untested", vec![]),
        (
            "qualified",
            vec![cell(
                -20,
                Purpose::Qualification,
                0,
                9999.0,
                Exposure::Adequate,
            )],
        ),
        (
            "control",
            vec![cell(0, Purpose::Control, 0, 9999.0, Exposure::Adequate)],
        ),
        (
            "short",
            vec![cell(-20, Purpose::Mapping, 0, 3.0, Exposure::Partial)],
        ),
    ]);
    let capped = full(&map, Slope::FALLBACK, Some(-30), 2);
    assert_eq!(capped.witnesses.len(), 1);
    assert_eq!(capped.witnesses[0].class, "covered");
    assert_eq!(
        capped.uncovered,
        vec![
            Uncovered {
                class: "control",
                reason: UncoveredReason::NonMappingOnly
            },
            Uncovered {
                class: "qualified",
                reason: UncoveredReason::NonMappingOnly
            },
            Uncovered {
                class: "short",
                reason: UncoveredReason::MappingInadequate
            },
            Uncovered {
                class: "untested",
                reason: UncoveredReason::NoObservations
            },
        ]
    );
    // The qualification exposure, however long, never enters the projection.
    assert_eq!(capped.offset, offset(-12));
}

fn fit(cells: &[Cell]) -> SlopeFit {
    fit_slope(cells, LikelihoodCutoff::CHI_SQUARE_95)
}

fn lower(cells: &[Cell]) -> f64 {
    match fit(cells) {
        SlopeFit::Measured { lower, .. } => lower.get(),
        SlopeFit::Unmeasured => panic!("expected a measured slope, found none"),
        SlopeFit::NoJustifiedDecay(no_decay) => {
            panic!("expected a measured slope, found {no_decay:?}")
        }
    }
}

/// Counts that grow by `ratio` for every ten counts of depth, over equal exposure.
fn geometric(first: u32, ratio: u32, time: f64) -> Vec<Cell> {
    vec![
        mapped(0, first, time),
        mapped(-10, first * ratio, time),
        mapped(-20, first * ratio * ratio, time),
    ]
}

#[test]
fn the_fit_returns_the_lower_endpoint_of_the_profile_interval() {
    // Counts 4, 12, 36 triple per ten counts: the maximum likelihood slope is exactly ln 3 / 10.
    let cells = geometric(4, 3, 1000.0);
    let SlopeFit::Measured { estimate, lower } = fit(&cells) else {
        panic!("expected a measured slope");
    };
    close(estimate.get(), 3.0_f64.ln() / 10.0, 1e-9);
    assert!(lower.get() > 0.0 && lower.get() < estimate.get());

    // Independent check of the interval: with `a` profiled out, twice the log-likelihood loss at the
    // lower endpoint equals the 3.842 cutoff.
    let profile = |beta: f64| {
        let exposure: f64 = [0.0, -10.0, -20.0]
            .iter()
            .map(|o| 1000.0 * (-beta * o).exp())
            .sum();
        -52.0 * exposure.ln() - beta * (12.0 * -10.0 + 36.0 * -20.0)
    };
    close(
        2.0 * (profile(estimate.get()) - profile(lower.get())),
        3.842,
        1e-6,
    );
}

#[test]
fn a_flatter_first_measurement_replaces_the_fallback() {
    let flat = geometric_large(1000, 1.2);
    let flat_lower = lower(&flat);
    assert!(flat_lower > 0.0 && flat_lower < Beta::FALLBACK.get());
    let slope = Slope::FALLBACK.absorb(fit(&flat));
    assert_eq!(slope.beta().map(Beta::get), Some(flat_lower));
}

fn geometric_large(first: u32, ratio: f64) -> Vec<Cell> {
    let step = |power: i32| {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "test counts"
        )]
        let count = (f64::from(first) * ratio.powi(power)).round() as u32;
        count
    };
    vec![
        mapped(0, step(0), 1.0e5),
        mapped(-10, step(1), 1.0e5),
        mapped(-20, step(2), 1.0e5),
    ]
}

#[test]
fn a_steeper_first_measurement_replaces_the_fallback() {
    let steep = geometric(1, 100, 1000.0);
    let steep_lower = lower(&steep);
    assert!(steep_lower > Beta::FALLBACK.get());
    let slope = Slope::FALLBACK.absorb(fit(&steep));
    assert_eq!(
        slope,
        Slope::Measured {
            lower: Beta::new(steep_lower).unwrap()
        }
    );
}

#[test]
fn a_later_measurement_can_only_lower_the_slope() {
    let steep = Slope::FALLBACK.absorb(fit(&geometric(1, 100, 1000.0)));
    let flat = steep.absorb(fit(&geometric_large(1000, 1.2)));
    let flat_beta = flat.beta().unwrap().get();
    assert!(flat_beta < steep.beta().unwrap().get());
    // A steeper class afterwards cannot raise it, nor can an unmeasured class.
    let again = flat
        .absorb(fit(&geometric(1, 100, 1000.0)))
        .absorb(SlopeFit::Unmeasured);
    assert_eq!(again, flat);
}

#[test]
fn failures_at_one_offset_are_not_a_measurement() {
    let cells = vec![
        mapped(0, 0, 1000.0),
        mapped(-10, 5, 1000.0),
        mapped(-20, 0, 1000.0),
    ];
    assert_eq!(fit(&cells), SlopeFit::Unmeasured);
    assert_eq!(Slope::FALLBACK.absorb(fit(&cells)), Slope::FALLBACK);
}

#[test]
fn failures_without_exposure_are_not_a_measurement() {
    let cells = vec![mapped(0, 5, 1000.0), mapped(-10, 5, 0.0)];
    assert_eq!(fit(&cells), SlopeFit::Unmeasured);
}

#[test]
fn a_flat_class_justifies_no_decay() {
    let cells = vec![mapped(0, 5, 1000.0), mapped(-20, 5, 1000.0)];
    let SlopeFit::NoJustifiedDecay(NoDecay::Nonpositive(lower)) = fit(&cells) else {
        panic!("expected no justified decay");
    };
    assert!(lower.get() < 0.0);
}

#[test]
fn a_falling_hazard_justifies_no_decay() {
    let cells = vec![mapped(0, 30, 1000.0), mapped(-20, 3, 1000.0)];
    assert!(matches!(
        fit(&cells),
        SlopeFit::NoJustifiedDecay(NoDecay::Nonpositive(_))
    ));
}

#[test]
fn no_justified_decay_forces_zero_and_is_never_undone() {
    let flat = Slope::FALLBACK.absorb(fit(&[mapped(0, 5, 1000.0), mapped(-20, 5, 1000.0)]));
    assert!(matches!(flat, Slope::NoJustifiedDecay(_)));
    assert_eq!(flat.absorb(fit(&geometric(1, 100, 1000.0))), flat);

    let map = single(vec![mapped(-20, 0, 1000.0)]);
    let capped = full(&map, flat, Some(-30), 2);
    assert_eq!(capped.offset, TargetOffset::ZERO);
    assert_eq!(capped.binding, Binding::NoJustifiedDecay);
}

#[test]
fn every_purpose_contributes_to_the_slope_but_not_to_the_projection() {
    let steep = geometric(1, 100, 1000.0)
        .into_iter()
        .map(|cell| Cell {
            purpose: Purpose::Qualification,
            ..cell
        })
        .collect::<Vec<_>>();
    let map = classes(vec![("r1", vec![mapped(-20, 0, 1000.0)]), ("r3", steep)]);
    let slope = Slope::FALLBACK.measure(&map, LikelihoodCutoff::CHI_SQUARE_95);
    assert!(matches!(slope, Slope::Measured { .. }));
    let capped = full(&map, slope, Some(-30), 2);
    assert_eq!(capped.witnesses.len(), 1);
    assert_eq!(
        capped.uncovered,
        vec![Uncovered {
            class: "r3",
            reason: UncoveredReason::NonMappingOnly
        }]
    );
}

#[test]
fn a_non_mapping_slope_update_cannot_deepen_the_cap() {
    // By hand, two CCDs: 10 million failure-free hours at -20 give b(x) = 2.99573e-7 against the budget
    // 1.6667e-4, a ratio of 556.3. At the fallback slope the rate limit reaches
    // -20 - ln(556.3) / 0.405465 = -35.6, so -35. A flatter measured slope of 0.1 reaches -83, past the
    // -50 floor, so the recomputed S is the edge cap U + 5 = -45: deeper than the previous -35.
    let map = single(vec![mapped(-20, 0, 1.0e7)]);
    let previous = full(&map, Slope::FALLBACK, Some(-50), 2);
    assert_eq!(previous.offset, offset(-35));
    let flat = Slope::Measured {
        lower: Beta::new(0.1).unwrap(),
    };
    let recomputed = full(&map, flat, Some(-50), 2);
    assert_eq!(recomputed.offset, offset(-45));

    let refresh = Refresh::NonMappingSlopeOnly {
        previous: previous.offset,
    };
    let retained = cap(&map, flat, Some(offset(-50)), refresh, &limits(2));
    assert_eq!(retained.offset, offset(-35));
    assert_eq!(
        retained.binding,
        Binding::Retained {
            recomputed: offset(-45)
        }
    );
}

#[test]
fn a_non_mapping_slope_update_may_still_make_the_cap_shallower() {
    let map = single(vec![mapped(-20, 0, 1000.0)]);
    let refresh = Refresh::NonMappingSlopeOnly {
        previous: offset(-30),
    };
    let updated = cap(
        &map,
        Slope::FALLBACK,
        Some(offset(-40)),
        refresh,
        &limits(2),
    );
    assert_eq!(updated.offset, offset(-12));
    assert_eq!(updated.binding, Binding::Rate { class: "r1" });
}

#[test]
fn custom_limits_change_the_cap() {
    // A one-CCD machine has twice the per-CCD budget, so the cap sits deeper than with two.
    let map = single(vec![mapped(-20, 0, 1000.0)]);
    let one = full(&map, Slope::FALLBACK, Some(-40), 1);
    let two = full(&map, Slope::FALLBACK, Some(-40), 2);
    assert!(one.offset < two.offset);
    let tight = CapLimits {
        machine_budget: Rate::per_hour(1.0 / 6000.0).unwrap(),
        ..limits(1)
    };
    assert_eq!(
        cap(
            &map,
            Slope::FALLBACK,
            Some(offset(-40)),
            Refresh::Full,
            &tight
        )
        .offset,
        two.offset
    );
}
