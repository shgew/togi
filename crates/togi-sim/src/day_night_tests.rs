use std::time::Duration;

use test_case::test_case;
use togi_machine::{CcdId, PerCore, Regime, TargetOffset, WorkloadId};

use super::{FailureCause, FailureCurve, SCENARIO_IDS, Scenario, TrialClass, TruthState};

const HOUR: Duration = Duration::from_hours(1);
const FIVE_MINUTES: Duration = Duration::from_mins(5);
const LAMBDA: f64 = 1.0 / 3000.0;

fn profile(scenario: &Scenario, offset: i64) -> Vec<TargetOffset> {
    vec![TargetOffset::new(offset).unwrap(); scenario.ccd_count()]
}

fn full_class(scenario: &Scenario) -> &TrialClass {
    scenario
        .daily_mix()
        .iter()
        .map(|block| &block.class)
        .find(|class| class.loaded().len() == scenario.topology().len())
        .unwrap()
}

#[test]
fn delayed_onset_requires_more_than_five_minutes_of_the_same_condition() {
    let scenario = Scenario::builtin("delayed-onset", 7).unwrap();
    let class = full_class(&scenario);
    let offsets = profile(&scenario, -40);
    let mut state = TruthState::cold(2).unwrap();
    assert_eq!(
        scenario
            .evaluate(class, &offsets, &state, FIVE_MINUTES, 0)
            .unwrap()
            .co_total(),
        0.0
    );
    state.advance(Some(class), FIVE_MINUTES).unwrap();
    assert_eq!(
        scenario
            .evaluate(class, &offsets, &state, FIVE_MINUTES, 0)
            .unwrap()
            .co_total(),
        0.0
    );
    state.advance(Some(class), FIVE_MINUTES).unwrap();
    assert!(
        scenario
            .evaluate(class, &offsets, &state, Duration::from_secs(60), 0)
            .unwrap()
            .co_total()
            > 0.0
    );
    state.advance(None, Duration::from_secs(1)).unwrap();
    assert_eq!(
        scenario
            .evaluate(class, &offsets, &state, FIVE_MINUTES, 0)
            .unwrap()
            .co_total(),
        0.0
    );
}

#[test]
fn delayed_onset_identity_includes_intended_duration_not_exposed_prefix() {
    let scenario = Scenario::builtin("delayed-onset", 7).unwrap();
    let class = full_class(&scenario);
    let offsets = profile(&scenario, -40);
    let mut state = TruthState::cold(2).unwrap();
    state.advance(Some(class), Duration::from_mins(10)).unwrap();
    let other_duration = class.with_duration(Duration::from_mins(20)).unwrap();
    assert_ne!(class, &other_duration);
    assert_eq!(
        scenario
            .evaluate(&other_duration, &offsets, &state, FIVE_MINUTES, 0)
            .unwrap()
            .co_total(),
        0.0
    );
    assert!(
        scenario
            .evaluate(class, &offsets, &state, FIVE_MINUTES, 0)
            .unwrap()
            .co_total()
            > 0.0
    );
}

#[test]
fn thermal_history_survives_a_short_sleep_but_not_a_long_sleep() {
    let scenario = Scenario::builtin("thermal-history", 7).unwrap();
    let class = full_class(&scenario);
    let offsets = profile(&scenario, -40);
    let mut warm = TruthState::cold(2).unwrap();
    assert_eq!(
        scenario
            .evaluate(class, &offsets, &warm, FIVE_MINUTES, 0)
            .unwrap()
            .co_total(),
        0.0
    );
    assert_eq!(
        scenario
            .evaluate(class, &offsets, &warm, Duration::from_mins(10), 0)
            .unwrap()
            .co_total(),
        0.0
    );
    assert!(
        scenario
            .evaluate(class, &offsets, &warm, Duration::from_mins(20), 0)
            .unwrap()
            .co_total()
            > 0.0
    );
    warm.advance(Some(class), Duration::from_mins(20)).unwrap();
    let ccd = CcdId::new(0).unwrap();
    let before = warm.heat_seconds(ccd).unwrap();
    warm.advance(None, Duration::from_secs(60)).unwrap();
    assert!(warm.heat_seconds(ccd).unwrap() < before);
    assert!(
        scenario
            .evaluate(class, &offsets, &warm, FIVE_MINUTES, 0)
            .unwrap()
            .co_total()
            > 0.0
    );
    warm.advance(None, Duration::from_hours(12)).unwrap();
    assert_eq!(
        scenario
            .evaluate(class, &offsets, &warm, FIVE_MINUTES, 0)
            .unwrap()
            .co_total(),
        0.0
    );
}

#[test]
fn thermal_exposure_is_invariant_under_slicing_the_same_condition() {
    let scenario = Scenario::builtin("thermal-history", 7).unwrap();
    let class = full_class(&scenario);
    let offsets = profile(&scenario, -30);
    let mut state = TruthState::cold(2).unwrap();
    let whole = scenario
        .evaluate(class, &offsets, &state, HOUR, 0)
        .unwrap()
        .co_total();
    let mut sliced = 0.0;
    for _ in 0..12 {
        sliced += scenario
            .evaluate(class, &offsets, &state, FIVE_MINUTES, 0)
            .unwrap()
            .co_total();
        state.advance(Some(class), FIVE_MINUTES).unwrap();
    }
    assert!((whole - sliced).abs() < whole * 1e-12);
}

#[test_case(1.2)]
#[test_case(6.0)]
fn curves_have_the_registered_exponential_slopes(growth: f64) {
    let curve = FailureCurve::new(20.0, growth, None, 0.01).unwrap();
    let a = curve.excess_per_hour(TargetOffset::new(-20).unwrap());
    let b = curve.excess_per_hour(TargetOffset::new(-21).unwrap());
    let c = curve.excess_per_hour(TargetOffset::new(-22).unwrap());
    // Subtraction of the stock rate changes ratios of rates, but not successive differences.
    assert!(((c - b) / (b - a) - growth).abs() < 1e-10);
}

#[test]
fn flattening_tail_is_continuous_and_flatter_away_from_the_edge() {
    let curve = FailureCurve::new(20.0, 6.0, Some(1.2), 0.01).unwrap();
    let rate = |depth: i64| curve.excess_per_hour(TargetOffset::new(-depth).unwrap());
    let central = (rate(22) - rate(21)) / (rate(21) - rate(20));
    let tail = (rate(27) - rate(26)) / (rate(26) - rate(25));
    assert!((central - 6.0).abs() < 1e-10);
    assert!((tail - 1.2).abs() < 1e-10);
    let left = FailureCurve::new(20.0 - 1e-9, 6.0, Some(1.2), 0.01).unwrap();
    let right = FailureCurve::new(20.0 + 1e-9, 6.0, Some(1.2), 0.01).unwrap();
    let boundary = TargetOffset::new(-23).unwrap();
    assert!((left.excess_per_hour(boundary) - right.excess_per_hour(boundary)).abs() < 1e-8);
}

#[test]
fn interaction_exists_only_with_both_ccds_loaded_and_is_jointly_constrained() {
    let scenario = Scenario::builtin("both-ccd-interaction", 9).unwrap();
    let state = TruthState::cold(2).unwrap();
    let offsets = profile(&scenario, -25);
    let both = scenario
        .evaluate(full_class(&scenario), &offsets, &state, HOUR, 0)
        .unwrap();
    assert!(both.interaction > 0.0);
    let alone = &scenario.daily_mix()[2].class;
    assert_eq!(
        scenario
            .evaluate(alone, &offsets, &state, HOUR, 0)
            .unwrap()
            .interaction,
        0.0
    );
    let one_stock = [TargetOffset::ZERO, offsets[1]];
    assert_eq!(
        scenario
            .evaluate(full_class(&scenario), &one_stock, &state, HOUR, 0)
            .unwrap()
            .interaction,
        0.0
    );
    let safe = scenario.safe_profile(0, LAMBDA).unwrap();
    assert!(safe.iter().any(|offset| offset.get() > -25));
}

#[test_case("single-core-binding", Regime::R1, Regime::R7)]
#[test_case("jointly-binding", Regime::R7, Regime::R1)]
#[test_case("r8-nonbinding", Regime::R7, Regime::R8)]
fn binding_scenarios_bind_the_named_class(name: &str, dangerous: Regime, mild: Regime) {
    let scenario = Scenario::builtin(name, 9).unwrap();
    let select = |regime| {
        scenario
            .night_classes()
            .iter()
            .find(|class| class.regime() == regime)
            .unwrap()
    };
    let state = TruthState::cold(2).unwrap();
    let offsets = profile(&scenario, -25);
    let dangerous = scenario
        .evaluate(select(dangerous), &offsets, &state, HOUR, 0)
        .unwrap()
        .co_per_ccd[0];
    let mild = scenario
        .evaluate(select(mild), &offsets, &state, HOUR, 0)
        .unwrap()
        .co_per_ccd[0];
    assert!(
        dangerous > mild,
        "{name}: dangerous={dangerous}, mild={mild}"
    );
}

#[test]
fn the_flatter_dangerous_class_is_more_dangerous_despite_a_flatter_slope() {
    let scenario = Scenario::builtin("flatter-dangerous-class", 3).unwrap();
    let state = TruthState::cold(2).unwrap();
    let class = |regime| {
        scenario
            .night_classes()
            .iter()
            .find(|class| class.regime() == regime)
            .unwrap()
    };
    let rate = |regime, offset| {
        scenario
            .evaluate(class(regime), &profile(&scenario, offset), &state, HOUR, 0)
            .unwrap()
            .co_per_ccd[0]
    };
    assert!(rate(Regime::R2, -20) > rate(Regime::R7, -20));
    let slope =
        |regime| (rate(regime, -22) - rate(regime, -21)) / (rate(regime, -21) - rate(regime, -20));
    assert!(slope(Regime::R2) < slope(Regime::R7));
}

#[test]
fn hidden_daily_workload_content_is_absent_from_the_night_menu() {
    let scenario = Scenario::builtin("hidden-daily", 3).unwrap();
    let hidden = WorkloadId::new(Regime::R2, 2).unwrap();
    assert!(
        scenario
            .daily_mix()
            .iter()
            .any(|block| block.class.workload() == hidden)
    );
    assert!(
        !scenario
            .night_classes()
            .iter()
            .any(|class| class.workload() == hidden)
    );
    assert!(scenario.hidden_daily());
}

#[test]
fn stock_background_is_not_co_hazard_and_does_not_change_true_safe() {
    let scenario = Scenario::builtin("stock-background", 3).unwrap();
    let state = TruthState::cold(2).unwrap();
    let stock = scenario
        .evaluate(
            full_class(&scenario),
            &profile(&scenario, 0),
            &state,
            HOUR,
            0,
        )
        .unwrap();
    assert_eq!(stock.co_total(), 0.0);
    assert_eq!(stock.background, LAMBDA);
    assert_eq!(
        scenario.safe_profile(0, LAMBDA).unwrap(),
        Scenario::builtin("non-cpu-correct", 3)
            .unwrap()
            .safe_profile(0, LAMBDA)
            .unwrap()
    );
}

#[test]
fn no_failure_at_the_write_limit_reaches_the_full_write_range() {
    let scenario = Scenario::builtin("no-write-limit-failure", 3).unwrap();
    let offsets = profile(&scenario, -50);
    let state = TruthState::cold(2).unwrap();
    assert_eq!(
        scenario
            .evaluate(full_class(&scenario), &offsets, &state, HOUR, 0)
            .unwrap()
            .co_total(),
        0.0
    );
    assert_eq!(scenario.safe_profile(0, LAMBDA).unwrap(), offsets);
}

#[test]
fn moving_and_firmware_caps_change_at_their_truth_epochs() {
    let moving = Scenario::builtin("moving-cap", 3).unwrap();
    assert_eq!(moving.cap_epoch(44), 0);
    assert_eq!(moving.cap_epoch(45), 1);
    assert_eq!(moving.cap_epoch(730), 10);
    assert_ne!(
        moving.safe_profile(0, LAMBDA).unwrap(),
        moving.safe_profile(450, LAMBDA).unwrap()
    );
    let firmware = Scenario::builtin("firmware-change", 3).unwrap();
    assert_eq!(firmware.transition_day(), Some(180));
    assert_eq!(firmware.cap_epoch(179), 0);
    assert_eq!(firmware.cap_epoch(180), 1);
    assert_ne!(
        firmware.safe_profile(179, LAMBDA).unwrap(),
        firmware.safe_profile(180, LAMBDA).unwrap()
    );
}

#[test]
fn draws_repeat_and_report_the_same_sources_the_integrator_uses() {
    let scenario = Scenario::builtin("slope-steep", 3).unwrap();
    let class = full_class(&scenario);
    let state = TruthState::cold(2).unwrap();
    let offsets = profile(&scenario, -40);
    let first = scenario
        .draw(class, &offsets, &state, HOUR, 0, 7, 42)
        .unwrap();
    assert_eq!(
        first,
        scenario
            .draw(class, &offsets, &state, HOUR, 0, 7, 42)
            .unwrap()
    );
    let failure = first.unwrap();
    assert!(failure.elapsed < HOUR);
    assert!(matches!(failure.cause, FailureCause::Ccd(_)));
    let exposure = scenario
        .evaluate(class, &offsets, &state, failure.elapsed, 0)
        .unwrap();
    assert!(exposure.co_total() > 0.0);
}

#[test]
fn class_round_trip_preserves_roles_phase_placement_and_duration() {
    let scenario = Scenario::builtin("partial-only", 3).unwrap();
    let class = scenario
        .night_classes()
        .iter()
        .find(|class| class.regime() == Regime::R8)
        .unwrap();
    let spec = class
        .to_spec("a label is not truth".to_owned(), PerCore::default(), 2, 7)
        .unwrap();
    spec.check_roles(scenario.topology()).unwrap();
    assert_eq!(&TrialClass::from_spec(&spec).unwrap(), class);
    assert_eq!(class.roles(), spec.roles);
}

#[test]
fn all_registered_scenarios_have_finite_rates_and_feasible_joint_safe_profiles() {
    // Exhaust the exported registered scenario domain, rather than a separate fixture list.
    for name in SCENARIO_IDS {
        let scenario = Scenario::builtin(name, 13).unwrap();
        let safe = scenario.safe_profile(0, LAMBDA).unwrap();
        let mut per_ccd = vec![0.0; scenario.ccd_count()];
        let mut joint = 0.0;
        assert_eq!(
            scenario
                .daily_mix()
                .iter()
                .map(|block| block.fraction)
                .sum::<f64>(),
            1.0,
            "{name}"
        );
        let mut periodic = TruthState::cold(scenario.ccd_count()).unwrap();
        // Two 24-hour cycles leave less than exp(-96) of the cold-start thermal error.
        for _ in 0..2 {
            for block in scenario.daily_mix() {
                periodic
                    .advance(Some(&block.class), block.class.duration())
                    .unwrap();
            }
            periodic.advance(None, Duration::from_hours(16)).unwrap();
        }
        for block in scenario.daily_mix() {
            assert!(block.class.duration() > FIVE_MINUTES, "{name}");
            let exposure = scenario
                .evaluate(&block.class, &safe, &periodic, block.class.duration(), 0)
                .unwrap();
            for (total, count) in per_ccd.iter_mut().zip(exposure.co_per_ccd) {
                *total += count / 8.0;
            }
            joint += exposure.interaction / 8.0;
            periodic
                .advance(Some(&block.class), block.class.duration())
                .unwrap();
        }
        for rate in &per_ccd {
            assert!(
                rate.is_finite()
                    && *rate
                        <= LAMBDA / f64::from(u32::try_from(scenario.ccd_count()).unwrap()) + 1e-15,
                "{name}: {per_ccd:?}"
            );
        }
        assert!(
            per_ccd.iter().sum::<f64>() + joint <= LAMBDA + 1e-15,
            "{name}: {per_ccd:?} + {joint}"
        );
    }
}

#[test_case(f64::NAN, 2.0, None, 0.01)]
#[test_case(20.0, f64::INFINITY, None, 0.01)]
#[test_case(20.0, 2.0, Some(f64::NAN), 0.01)]
#[test_case(20.0, 2.0, None, -0.01)]
#[test_case(20.0, 2.0, None, f64::MAX)]
fn invalid_or_unrepresentable_curves_are_refused(
    edge: f64,
    growth: f64,
    tail: Option<f64>,
    rate: f64,
) {
    assert!(FailureCurve::new(edge, growth, tail, rate).is_err());
}

#[test]
fn invalid_lambda_and_topology_are_refused() {
    let scenario = Scenario::builtin("single-ccd", 3).unwrap();
    assert_eq!(scenario.ccd_count(), 1);
    assert!(scenario.safe_profile(0, f64::NAN).is_err());
    assert!(scenario.safe_profile(0, 0.0).is_err());
    assert!(TruthState::cold(0).is_err());
    assert!(TruthState::cold(3).is_err());
    let state = TruthState::cold(2).unwrap();
    assert!(
        scenario
            .evaluate(
                full_class(&scenario),
                &profile(&scenario, -20),
                &state,
                HOUR,
                0
            )
            .is_err()
    );
}

#[test]
fn extremely_fast_failure_still_advances_the_calendar_by_a_nanosecond() {
    let scenario = Scenario::builtin("slope-steep", 3).unwrap();
    let state = TruthState::cold(2).unwrap();
    let failure = scenario
        .draw(
            full_class(&scenario),
            &profile(&scenario, -50),
            &state,
            HOUR,
            0,
            7,
            42,
        )
        .unwrap()
        .unwrap();
    assert_eq!(failure.elapsed, Duration::from_nanos(1));
}

#[test]
fn partial_only_hazard_binds_a_proper_subset_not_full_ccd_or_r8() {
    let scenario = Scenario::builtin("partial-only", 9).unwrap();
    let partial = scenario
        .night_classes()
        .iter()
        .find(|class| class.regime() == Regime::R7 && class.loaded().len() == 4)
        .unwrap();
    let full = scenario
        .night_classes()
        .iter()
        .find(|class| class.regime() == Regime::R7 && class.loaded().len() == 8)
        .unwrap();
    let r8 = scenario
        .night_classes()
        .iter()
        .find(|class| class.regime() == Regime::R8)
        .unwrap();
    assert_eq!(r8.loaded().len(), 8);
    let state = TruthState::cold(2).unwrap();
    let offsets = profile(&scenario, -25);
    let rate = |class| {
        scenario
            .evaluate(class, &offsets, &state, HOUR, 0)
            .unwrap()
            .co_per_ccd[0]
    };
    assert!(rate(partial) > rate(full));
    assert!(rate(partial) > rate(r8));
    assert!(
        scenario
            .daily_mix()
            .iter()
            .any(|block| block.class.regime() == Regime::R7 && block.class.loaded().len() == 4)
    );
}

#[test]
fn covered_daily_classes_include_exact_duration_and_roles_in_the_night_menu() {
    for name in SCENARIO_IDS {
        let scenario = Scenario::builtin(name, 9).unwrap();
        let hidden = WorkloadId::new(Regime::R2, 2).unwrap();
        for block in scenario.daily_mix() {
            if scenario.hidden_daily() && block.class.workload() == hidden {
                continue;
            }
            assert!(
                scenario.night_classes().contains(&block.class),
                "{name}: {:?}",
                block.class
            );
        }
    }
}

#[test]
fn safe_profile_integrates_duration_and_onset_instead_of_steady_hot_rates() {
    let scenario = Scenario::builtin("delayed-onset", 9).unwrap();
    let fractions = [0.25; 4];
    let short = scenario
        .safe_profile_for(0, LAMBDA, Duration::from_mins(20), &fractions)
        .unwrap();
    assert_eq!(short, profile(&scenario, -50));
    assert_ne!(scenario.safe_profile(0, LAMBDA).unwrap(), short);
    assert_eq!(
        scenario.safe_profile(0, LAMBDA).unwrap(),
        scenario
            .safe_profile_for(0, LAMBDA, Duration::from_hours(8), &fractions)
            .unwrap()
    );
}

#[test]
fn configurable_safe_reference_refuses_invalid_calendar_values() {
    let scenario = Scenario::builtin("thermal-history", 9).unwrap();
    assert!(
        scenario
            .safe_profile_for(0, LAMBDA, Duration::ZERO, &[0.25; 4])
            .is_err()
    );
    assert!(
        scenario
            .safe_profile_for(0, LAMBDA, Duration::from_hours(25), &[0.25; 4])
            .is_err()
    );
    assert!(
        scenario
            .safe_profile_for(0, LAMBDA, HOUR, &[0.5; 4])
            .is_err()
    );
    assert!(
        scenario
            .safe_profile_for(0, LAMBDA, HOUR, &[f64::NAN; 4])
            .is_err()
    );
    assert!(
        scenario
            .safe_profile_for(0, LAMBDA, HOUR, &[0.5; 2])
            .is_err()
    );
}
