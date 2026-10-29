use std::time::Duration;

use test_case::test_case;
use togi_machine::{CcdId, Condition, CoreId, PerCore, TrialRole};

use super::{FailureCause, InteractionId, TruthState};
use crate::day_night::successor::{Depth, LAMBDA, Profile, Scenario};
use crate::day_night::{TrialClass, TruthError};

const DAY: u32 = 1;
const HOUR_S: f64 = 3600.0;

fn secs(seconds: u64) -> Duration {
    Duration::from_secs(seconds)
}

fn scenario(id: &str) -> Scenario {
    Scenario::builtin(id, 7).unwrap()
}

fn cold(scenario: &Scenario) -> TruthState {
    TruthState::cold(scenario.topology()).unwrap()
}

/// The first daily class that loads every physical core; its intended duration is two hours.
fn full_class(scenario: &Scenario) -> TrialClass {
    scenario
        .daily_mix()
        .iter()
        .map(|block| &block.class)
        .find(|class| class.loaded().len() == scenario.topology().len())
        .unwrap()
        .clone()
}

fn uniform(scenario: &Scenario, depth: u8) -> Profile {
    Profile::uniform(scenario.topology(), Depth::new(depth).unwrap())
}

fn with_core(scenario: &Scenario, base: u8, core: CoreId, depth: u8) -> Profile {
    let mut depths = PerCore::default();
    for info in scenario.topology() {
        let value = if info.core == core { depth } else { base };
        depths.set(info.core, Depth::new(value).unwrap());
    }
    Profile::new(depths, scenario.topology()).unwrap()
}

fn co(
    scenario: &Scenario,
    class: &TrialClass,
    profile: &Profile,
    state: &TruthState,
    run: Duration,
) -> f64 {
    scenario
        .evaluate(class, profile, state, run, DAY)
        .unwrap()
        .co_total()
}

fn assert_close(left: f64, right: f64) {
    assert!(
        (left - right).abs() <= 1e-9 * left.abs().max(right.abs()),
        "{left} is not {right}"
    );
}

fn assert_close_duration(left: Duration, right: Duration) {
    assert!(
        left.abs_diff(right) <= Duration::from_micros(10),
        "{left:?} is not {right:?}"
    );
}

#[test_case(0, 599, false; "a fresh run inside the delay")]
#[test_case(0, 600, false; "a fresh run ending exactly at the delay")]
#[test_case(0, 601, true; "a fresh run one second past the delay")]
#[test_case(599, 1, false; "a continued run ending exactly at the delay")]
#[test_case(599, 2, true; "a continued run crossing the delay")]
#[test_case(600, 1, true; "a continued run starting at the delay")]
#[test_case(601, 1, true; "a continued run past the delay")]
fn legacy_delay_is_six_hundred_uninterrupted_seconds(prior: u64, run: u64, exposed: bool) {
    let scenario = scenario("successor-delayed-onset");
    let class = full_class(&scenario);
    let profile = uniform(&scenario, 40);
    let mut state = cold(&scenario);
    state.advance(Some(&class), secs(prior)).unwrap();
    assert_eq!(
        co(&scenario, &class, &profile, &state, secs(run)) > 0.0,
        exposed
    );
}

#[test_case(599, false; "inside the delay")]
#[test_case(600, false; "exactly at the delay")]
#[test_case(601, true; "past the delay")]
fn only_weak_cores_wait_for_the_new_delay(run: u64, weak_exposed: bool) {
    let scenario = scenario("successor-weak-core-delayed");
    let class = full_class(&scenario);
    let profile = uniform(&scenario, 45);
    let exposure = scenario
        .evaluate(&class, &profile, &cold(&scenario), secs(run), DAY)
        .unwrap();
    for info in scenario.topology() {
        let count = exposure.co_per_core.get(info.core).unwrap();
        if scenario.weak.contains(&info.core) {
            assert_eq!(count > 0.0, weak_exposed, "weak core {}", info.core.get());
        } else {
            let rate = scenario
                .core_rate(&class, info.core, Depth::new(45).unwrap(), DAY)
                .unwrap();
            assert_close(count, rate * secs(run).as_secs_f64() / HOUR_S);
        }
    }
}

#[test]
fn a_new_delay_never_delays_joint_sources() {
    let scenario = scenario("successor-combined-adversary");
    let class = full_class(&scenario);
    let profile = uniform(&scenario, 45);
    let exposure = scenario
        .evaluate(&class, &profile, &cold(&scenario), secs(100), DAY)
        .unwrap();
    assert_eq!(scenario.joint_pairs().len(), 2);
    assert_eq!(exposure.interactions.len(), 2);
    for entry in &exposure.interactions {
        assert_close(entry.expected_failures, 4.0 * LAMBDA * 100.0 / HOUR_S);
    }
    assert_close(
        exposure.interaction_total(),
        2.0 * 4.0 * LAMBDA * 100.0 / HOUR_S,
    );
    for &core in &scenario.weak {
        assert_eq!(exposure.co_per_core.get(core), Some(0.0));
    }
}

#[test]
fn every_registered_pair_is_reported_even_when_unloaded_or_stock() {
    let scenario = scenario("successor-core-interactions");
    let expected = scenario
        .joint_pairs()
        .iter()
        .map(|&(first, second)| InteractionId::new(first, second).unwrap())
        .collect::<Vec<_>>();
    let night = scenario.night_classes()[0].clone();
    for (class, depth) in [(night, 45), (full_class(&scenario), 0)] {
        let exposure = scenario
            .evaluate(
                &class,
                &uniform(&scenario, depth),
                &cold(&scenario),
                secs(600),
                DAY,
            )
            .unwrap();
        let sources = exposure
            .interactions
            .iter()
            .map(|entry| entry.source)
            .collect::<Vec<_>>();
        assert_eq!(sources, expected);
        assert_eq!(exposure.interaction_total(), 0.0);
    }
}

#[test]
fn interaction_ids_are_ordered_distinct_pairs() {
    let low = CoreId::new(2).unwrap();
    let high = CoreId::new(9).unwrap();
    let id = InteractionId::new(high, low).unwrap();
    assert_eq!(id, InteractionId::new(low, high).unwrap());
    assert_eq!(id.pair(), (low, high));
    assert_eq!(
        InteractionId::new(low, low).unwrap_err(),
        TruthError::Invalid("interaction endpoints")
    );
}

#[test]
fn background_is_independent_of_core_delays_and_is_not_co_exposure() {
    let scenario = scenario("successor-stock-background");
    let class = full_class(&scenario);
    let profile = uniform(&scenario, 0);
    let mut state = cold(&scenario);
    let fresh = scenario
        .evaluate(&class, &profile, &state, secs(3600), DAY)
        .unwrap();
    assert_close(fresh.background, LAMBDA);
    assert_eq!(fresh.co_total(), 0.0);
    assert_close(fresh.failure_probability(), -(-LAMBDA).exp_m1());
    state.advance(Some(&class), secs(700)).unwrap();
    let warm = scenario
        .evaluate(&class, &profile, &state, secs(3600), DAY)
        .unwrap();
    assert_eq!(warm, fresh);
}

#[test]
fn ordinary_scenarios_expose_from_the_first_second_without_subtracting_the_delay() {
    let scenario = scenario("successor-edge-shallow");
    let class = full_class(&scenario);
    let profile = uniform(&scenario, 40);
    let state = cold(&scenario);
    assert!(co(&scenario, &class, &profile, &state, secs(1)) > 0.0);
    assert_close(
        co(&scenario, &class, &profile, &state, secs(600)),
        2.0 * co(&scenario, &class, &profile, &state, secs(300)),
    );
}

#[test]
fn legacy_curves_split_equally_among_loaded_cores_of_a_ccd() {
    let scenario = scenario("successor-edge-shallow");
    let class = full_class(&scenario);
    let exposure = scenario
        .evaluate(
            &class,
            &uniform(&scenario, 40),
            &cold(&scenario),
            secs(600),
            DAY,
        )
        .unwrap();
    let first = exposure
        .co_per_core
        .get(scenario.topology()[0].core)
        .unwrap();
    assert!(first > 0.0);
    for info in scenario.topology() {
        assert_close(exposure.co_per_core.get(info.core).unwrap(), first);
    }
}

#[test_case(729, false; "inside the cold-start warmup")]
#[test_case(730, true; "past the cold-start warmup")]
fn cold_thermal_onset_is_the_time_to_reach_the_heat_threshold(run: u64, exposed: bool) {
    let scenario = scenario("successor-thermal-history");
    let class = full_class(&scenario);
    let profile = uniform(&scenario, 40);
    assert_eq!(
        co(&scenario, &class, &profile, &cold(&scenario), secs(run)) > 0.0,
        exposed
    );
}

#[test]
fn warm_history_exposes_at_once_until_enough_sleep_cools_it() {
    let scenario = scenario("successor-thermal-history");
    let class = full_class(&scenario);
    let profile = uniform(&scenario, 40);
    let mut state = cold(&scenario);
    state.advance(Some(&class), secs(20 * 60)).unwrap();
    let ccd = CcdId::new(0).unwrap();
    let heat = state.heat_seconds(ccd).unwrap();
    assert!(co(&scenario, &class, &profile, &state, secs(300)) > 0.0);
    state.advance(None, secs(60)).unwrap();
    assert_close(
        state.heat_seconds(ccd).unwrap(),
        heat * (-60.0_f64 / 1800.0).exp(),
    );
    assert!(co(&scenario, &class, &profile, &state, secs(300)) > 0.0);
    state.advance(None, secs(12 * 3600)).unwrap();
    assert_eq!(co(&scenario, &class, &profile, &state, secs(300)), 0.0);
}

#[test]
fn only_loaded_ccds_heat_and_sleep_ends_the_uninterrupted_clock() {
    let scenario = scenario("successor-thermal-history");
    let single = scenario.night_classes()[0].clone();
    assert_eq!(single.loaded().len(), 1);
    let mut state = cold(&scenario);
    state.advance(Some(&single), secs(900)).unwrap();
    let loaded = scenario.topology()[0].ccd;
    let idle = scenario
        .topology()
        .iter()
        .map(|info| info.ccd)
        .find(|ccd| *ccd != loaded)
        .unwrap();
    assert_close(
        state.heat_seconds(loaded).unwrap(),
        1800.0 * (1.0 - (-0.5_f64).exp()),
    );
    assert_eq!(state.heat_seconds(idle), Some(0.0));
    assert_eq!(state.heat_seconds(CcdId::new(9).unwrap()), None);

    let delayed = self::scenario("successor-delayed-onset");
    let class = full_class(&delayed);
    let profile = uniform(&delayed, 40);
    let mut run = cold(&delayed);
    run.advance(Some(&class), secs(700)).unwrap();
    run.advance(None, Duration::ZERO).unwrap();
    assert_eq!(co(&delayed, &class, &profile, &run, secs(600)), 0.0);
}

#[test]
fn explicit_interruption_restarts_the_delay() {
    let scenario = scenario("successor-delayed-onset");
    let class = full_class(&scenario);
    let profile = uniform(&scenario, 40);
    let mut state = cold(&scenario);
    state
        .advance_profile(Some(&class), &profile, secs(700))
        .unwrap();
    assert!(co(&scenario, &class, &profile, &state, secs(1)) > 0.0);
    state.interrupt();
    assert_eq!(co(&scenario, &class, &profile, &state, secs(600)), 0.0);
    assert!(co(&scenario, &class, &profile, &state, secs(601)) > 0.0);
}

#[test]
fn interruption_keeps_thermal_heat() {
    let scenario = scenario("successor-thermal-history");
    let class = full_class(&scenario);
    let profile = uniform(&scenario, 40);
    let ccd = CcdId::new(0).unwrap();
    let mut state = cold(&scenario);
    state
        .advance_profile(Some(&class), &profile, secs(20 * 60))
        .unwrap();
    let heat = state.heat_seconds(ccd);
    state.interrupt();
    assert_eq!(state.heat_seconds(ccd), heat);
    assert!(co(&scenario, &class, &profile, &state, secs(1)) > 0.0);
}

#[test]
fn zero_time_advance_of_the_same_class_continues_but_another_class_restarts() {
    let scenario = scenario("successor-delayed-onset");
    let class = full_class(&scenario);
    let other = scenario.night_classes()[0].clone();
    let profile = uniform(&scenario, 40);
    let mut state = cold(&scenario);
    state.advance(Some(&class), secs(700)).unwrap();
    state.advance(Some(&class), Duration::ZERO).unwrap();
    assert!(co(&scenario, &class, &profile, &state, secs(1)) > 0.0);
    state.advance(Some(&other), Duration::ZERO).unwrap();
    assert_eq!(co(&scenario, &class, &profile, &state, secs(600)), 0.0);
}

#[derive(Clone, Copy, Debug)]
enum Change {
    Duration,
    Class,
    Condition,
    Placement,
}

fn changed(scenario: &Scenario, class: &TrialClass, change: Change) -> TrialClass {
    let spec = || {
        class
            .to_spec(String::new(), PerCore::default(), 0, 0)
            .unwrap()
    };
    let result = match change {
        Change::Duration => class.with_duration(secs(20 * 60)).unwrap(),
        Change::Class => scenario.daily_mix()[2].class.clone(),
        Change::Condition => {
            let mut spec = spec();
            spec.condition = if spec.condition == Condition::Alone {
                Condition::Together
            } else {
                Condition::Alone
            };
            TrialClass::from_spec(&spec).unwrap()
        }
        Change::Placement => {
            let mut spec = spec();
            spec.roles.reverse();
            spec.cores = TrialRole::cores(&spec.roles);
            spec.cpus = TrialRole::cpus(&spec.roles);
            TrialClass::from_spec(&spec).unwrap()
        }
    };
    assert_ne!(&result, class, "{change:?} must change the exact class");
    result
}

#[test_case(Change::Duration; "intended duration")]
#[test_case(Change::Class; "workload class")]
#[test_case(Change::Condition; "condition")]
#[test_case(Change::Placement; "role placement")]
fn any_exact_class_change_restarts_the_delay(change: Change) {
    let scenario = scenario("successor-delayed-onset");
    let class = full_class(&scenario);
    let other = changed(&scenario, &class, change);
    let profile = uniform(&scenario, 40);
    let mut state = cold(&scenario);
    state
        .advance_profile(Some(&class), &profile, secs(700))
        .unwrap();
    assert_eq!(co(&scenario, &other, &profile, &state, secs(600)), 0.0);
    let mut control = cold(&scenario);
    control
        .advance_profile(Some(&other), &profile, secs(700))
        .unwrap();
    assert!(co(&scenario, &other, &profile, &control, secs(1)) > 0.0);
}

#[test]
fn any_full_profile_change_restarts_the_delay() {
    let scenario = scenario("successor-delayed-onset");
    let class = full_class(&scenario);
    let base = uniform(&scenario, 40);
    let one_core = with_core(&scenario, 40, scenario.topology()[3].core, 41);
    for other in [uniform(&scenario, 41), one_core] {
        let mut state = cold(&scenario);
        state
            .advance_profile(Some(&class), &base, secs(700))
            .unwrap();
        assert!(co(&scenario, &class, &base, &state, secs(1)) > 0.0);
        assert_eq!(co(&scenario, &class, &other, &state, secs(600)), 0.0);
        state
            .advance_profile(Some(&class), &other, secs(1))
            .unwrap();
        assert_eq!(co(&scenario, &class, &other, &state, secs(599)), 0.0);
        assert!(co(&scenario, &class, &other, &state, secs(600)) > 0.0);
        assert_eq!(co(&scenario, &class, &base, &state, secs(600)), 0.0);
    }
}

#[test]
fn a_profile_free_advance_continues_the_class_whatever_profile_ran() {
    let scenario = scenario("successor-delayed-onset");
    let class = full_class(&scenario);
    let mut state = cold(&scenario);
    state.advance(Some(&class), secs(700)).unwrap();
    for depth in [40, 41] {
        assert!(
            co(
                &scenario,
                &class,
                &uniform(&scenario, depth),
                &state,
                secs(1)
            ) > 0.0
        );
    }
    state
        .advance_profile(Some(&class), &uniform(&scenario, 41), secs(1))
        .unwrap();
    assert!(co(&scenario, &class, &uniform(&scenario, 41), &state, secs(1)) > 0.0);
    assert_eq!(
        co(
            &scenario,
            &class,
            &uniform(&scenario, 40),
            &state,
            secs(600)
        ),
        0.0
    );
}

#[test_case(0, 300, 600, 0; "fresh prefix inside the warmup")]
#[test_case(0, 700, 600, 100; "fresh prefix past the warmup")]
#[test_case(400, 300, 600, 100; "continued prefix crossing the warmup")]
#[test_case(600, 300, 600, 300; "continued prefix after the warmup")]
#[test_case(700, 300, 600, 300; "continued prefix well after the warmup")]
#[test_case(0, 300, 0, 300; "no warmup")]
#[test_case(0, 0, 600, 0; "empty prefix")]
fn eligible_time_follows_the_uninterrupted_clock(
    prior: u64,
    observed: u64,
    warmup: u64,
    eligible: u64,
) {
    let scenario = scenario("successor-delayed-onset");
    let class = full_class(&scenario);
    let profile = uniform(&scenario, 40);
    let mut state = cold(&scenario);
    state
        .advance_profile(Some(&class), &profile, secs(prior))
        .unwrap();
    let slice = scenario
        .slice_seconds(&class, &profile, &state, secs(observed), secs(warmup))
        .unwrap();
    assert_eq!(slice.intended, class.duration());
    assert_eq!(slice.observed, secs(observed));
    assert_eq!(slice.eligible, secs(eligible));
}

#[test]
fn eligibility_restarts_with_the_class_profile_or_an_interruption() {
    let scenario = scenario("successor-delayed-onset");
    let class = full_class(&scenario);
    let profile = uniform(&scenario, 40);
    let mut state = cold(&scenario);
    state
        .advance_profile(Some(&class), &profile, secs(700))
        .unwrap();
    let eligible = |state: &TruthState, profile: &Profile| {
        scenario
            .slice_seconds(&class, profile, state, secs(300), secs(600))
            .unwrap()
            .eligible
    };
    assert_eq!(eligible(&state, &profile), secs(300));
    assert_eq!(eligible(&state, &uniform(&scenario, 41)), Duration::ZERO);
    state.interrupt();
    assert_eq!(eligible(&state, &profile), Duration::ZERO);
}

#[test]
fn onset_active_time_is_per_core_and_zero_for_unloaded_cores() {
    let scenario = scenario("successor-delayed-onset");
    let night = scenario.night_classes()[0].clone();
    let profile = uniform(&scenario, 40);
    let mut state = cold(&scenario);
    state
        .advance_profile(Some(&night), &profile, secs(400))
        .unwrap();
    let slice = scenario
        .slice_seconds(&night, &profile, &state, secs(300), Duration::ZERO)
        .unwrap();
    assert_eq!(slice.intended, secs(300));
    for info in scenario.topology() {
        let expected = if night.loaded().contains(info.core) {
            secs(100)
        } else {
            Duration::ZERO
        };
        assert_eq!(
            slice.onset_active.get(info.core),
            Some(expected),
            "core {}",
            info.core.get()
        );
    }
}

#[test]
fn a_prefix_may_equal_the_intended_duration_but_never_exceed_it() {
    let scenario = scenario("successor-delayed-onset");
    let class = full_class(&scenario);
    let profile = uniform(&scenario, 40);
    let state = cold(&scenario);
    let intended = class.duration();
    let slice = scenario
        .slice_seconds(&class, &profile, &state, intended, Duration::ZERO)
        .unwrap();
    assert_eq!(slice.intended, intended);
    assert_eq!(slice.observed, intended);
    assert_eq!(
        scenario
            .slice_seconds(
                &class,
                &profile,
                &state,
                intended + Duration::from_nanos(1),
                Duration::ZERO
            )
            .unwrap_err(),
        TruthError::Invalid("observed prefix beyond intended duration")
    );
    let prefix = scenario
        .slice_seconds(&class, &profile, &state, secs(300), Duration::ZERO)
        .unwrap();
    assert_eq!(prefix.intended, intended);
    assert_ne!(prefix.observed, prefix.intended);
}

#[test]
fn cumulative_uninterrupted_exposure_may_exceed_the_intended_duration() {
    let scenario = scenario("successor-delayed-onset");
    let class = full_class(&scenario);
    let profile = uniform(&scenario, 40);
    let mut state = cold(&scenario);
    state
        .advance_profile(Some(&class), &profile, class.duration())
        .unwrap();
    state
        .advance_profile(Some(&class), &profile, class.duration())
        .unwrap();
    assert!(co(&scenario, &class, &profile, &state, class.duration() * 2) > 0.0);
    let slice = scenario
        .slice_seconds(&class, &profile, &state, class.duration(), secs(600))
        .unwrap();
    assert_eq!(slice.eligible, class.duration());
}

#[test_case("successor-delayed-onset")]
#[test_case("successor-thermal-history")]
#[test_case("successor-weak-core-delayed")]
fn checkpoint_splits_of_one_run_match_the_whole_run(id: &str) {
    let scenario = scenario(id);
    let class = full_class(&scenario);
    let profile = uniform(&scenario, 40);
    let core = scenario.topology()[0].core;
    let start = cold(&scenario);
    let whole = co(&scenario, &class, &profile, &start, secs(3600));
    let whole_slice = scenario
        .slice_seconds(&class, &profile, &start, secs(3600), secs(600))
        .unwrap();
    let mut state = cold(&scenario);
    let mut sliced = 0.0;
    let mut eligible = Duration::ZERO;
    let mut active = Duration::ZERO;
    for _ in 0..12 {
        sliced += co(&scenario, &class, &profile, &state, secs(300));
        let slice = scenario
            .slice_seconds(&class, &profile, &state, secs(300), secs(600))
            .unwrap();
        eligible += slice.eligible;
        active += slice.onset_active.get(core).unwrap();
        state
            .advance_profile(Some(&class), &profile, secs(300))
            .unwrap();
    }
    assert_close(whole, sliced);
    assert_eq!(eligible, whole_slice.eligible);
    assert_close_duration(active, whole_slice.onset_active.get(core).unwrap());
}

#[test_case(true, false; "interrupted starts never reach the delay")]
#[test_case(false, true; "uninterrupted short starts accumulate")]
fn many_short_starts_expose_only_when_the_clock_keeps_running(interrupted: bool, exposed: bool) {
    let scenario = scenario("successor-delayed-onset");
    let class = full_class(&scenario);
    let profile = uniform(&scenario, 40);
    let mut state = cold(&scenario);
    let mut total = 0.0;
    for _ in 0..100 {
        total += co(&scenario, &class, &profile, &state, secs(10));
        state
            .advance_profile(Some(&class), &profile, secs(10))
            .unwrap();
        if interrupted {
            state.interrupt();
        }
    }
    assert_eq!(total > 0.0, exposed);
}

#[test]
fn draws_repeat_and_never_precede_the_core_onset() {
    let scenario = scenario("successor-delayed-onset");
    let class = full_class(&scenario);
    let profile = uniform(&scenario, 40);
    let state = cold(&scenario);
    for key in 0..200 {
        let early = scenario
            .draw(&class, &profile, &state, secs(600), DAY, 5, key)
            .unwrap();
        assert_eq!(early, None, "key {key}");
    }
    let mut failures = 0;
    for key in 0..200 {
        let draw = scenario
            .draw(&class, &profile, &state, secs(3600), DAY, 5, key)
            .unwrap();
        assert_eq!(
            draw,
            scenario
                .draw(&class, &profile, &state, secs(3600), DAY, 5, key)
                .unwrap(),
            "key {key}"
        );
        if let Some(failure) = draw {
            failures += 1;
            assert!(failure.elapsed >= secs(600), "key {key}");
            assert!(matches!(failure.cause, FailureCause::Core(_)), "key {key}");
        }
    }
    assert!(failures > 0);
    assert_eq!(
        scenario
            .draw(&class, &profile, &state, Duration::ZERO, DAY, 5, 0)
            .unwrap(),
        None
    );
}

#[test]
fn stock_draws_fail_only_from_background() {
    let scenario = scenario("successor-stock-background");
    let class = full_class(&scenario);
    let profile = uniform(&scenario, 0);
    let mut observed = 0;
    for key in 0..20 {
        let failure = scenario
            .draw(
                &class,
                &profile,
                &cold(&scenario),
                Duration::from_hours(1000),
                DAY,
                3,
                key,
            )
            .unwrap();
        if let Some(failure) = failure {
            observed += 1;
            assert_eq!(failure.cause, FailureCause::Background, "key {key}");
        }
    }
    assert!(observed > 0);
}

#[test]
fn joint_draws_name_the_pair_and_need_both_endpoints_loaded() {
    let scenario = scenario("successor-core-interactions");
    let class = full_class(&scenario);
    let profile = uniform(&scenario, 21);
    let mut pairs = [false; 2];
    for key in 0..60 {
        let failure = scenario
            .draw(
                &class,
                &profile,
                &cold(&scenario),
                Duration::from_hours(5000),
                DAY,
                3,
                key,
            )
            .unwrap()
            .unwrap();
        let FailureCause::Interaction(id) = failure.cause else {
            panic!("key {key} drew {:?}", failure.cause);
        };
        let index = scenario
            .joint_pairs()
            .iter()
            .position(|&(first, second)| InteractionId::new(first, second).unwrap() == id)
            .unwrap();
        pairs[index] = true;
    }
    assert_eq!(pairs, [true, true]);
    let night = scenario.night_classes()[0].clone();
    for key in 0..60 {
        let draw = scenario
            .draw(
                &night,
                &profile,
                &cold(&scenario),
                Duration::from_hours(5000),
                DAY,
                3,
                key,
            )
            .unwrap();
        assert_eq!(draw, None, "key {key}");
    }
}

#[test]
fn core_draws_name_only_loaded_cores() {
    let scenario = scenario("successor-within-ccd-heterogeneous");
    let night = scenario.night_classes()[0].clone();
    let profile = uniform(&scenario, 45);
    let only = scenario.topology()[0].core;
    assert_eq!(night.loaded().len(), 1);
    assert!(night.loaded().contains(only));
    for key in 0..100 {
        let failure = scenario
            .draw(
                &night,
                &profile,
                &cold(&scenario),
                Duration::from_hours(100),
                DAY,
                3,
                key,
            )
            .unwrap()
            .unwrap();
        assert_eq!(failure.cause, FailureCause::Core(only), "key {key}");
    }
}

#[test]
fn the_calendar_day_is_one_based() {
    let scenario = scenario("successor-edge-shallow");
    let class = full_class(&scenario);
    let profile = uniform(&scenario, 40);
    let state = cold(&scenario);
    let invalid = TruthError::Invalid("one-based calendar day");
    assert_eq!(
        scenario
            .evaluate(&class, &profile, &state, secs(1), 0)
            .unwrap_err(),
        invalid
    );
    assert_eq!(
        scenario
            .draw(&class, &profile, &state, secs(1), 0, 0, 0)
            .unwrap_err(),
        invalid
    );
}

#[test]
fn history_profile_and_class_must_cover_the_scenario_topology() {
    let scenario = scenario("successor-edge-shallow");
    let class = full_class(&scenario);
    let profile = uniform(&scenario, 40);
    let half = &scenario.topology()[..8];
    let small = TruthState::cold(half).unwrap();
    assert_eq!(
        scenario
            .evaluate(&class, &profile, &small, secs(1), DAY)
            .unwrap_err(),
        TruthError::Invalid("history topology")
    );
    let partial = Profile::uniform(half, Depth::new(40).unwrap());
    assert_eq!(
        scenario
            .evaluate(&class, &partial, &cold(&scenario), secs(1), DAY)
            .unwrap_err(),
        TruthError::Invalid("profile topology")
    );
    let mut state = small;
    assert_eq!(
        state.advance(Some(&class), secs(60)).unwrap_err(),
        TruthError::Invalid("class topology")
    );
    assert_eq!(state.heat_seconds(scenario.topology()[0].ccd), Some(0.0));
    assert_eq!(
        state.advance_profile(None, &profile, secs(60)).unwrap_err(),
        TruthError::Invalid("profile topology")
    );
}

#[test]
fn a_refused_advance_leaves_history_unchanged() {
    let scenario = scenario("successor-thermal-history");
    let class = full_class(&scenario);
    let mut state = cold(&scenario);
    state.advance(Some(&class), Duration::MAX).unwrap();
    let ccd = scenario.topology()[0].ccd;
    let heat = state.heat_seconds(ccd);
    assert_eq!(
        state.advance(Some(&class), Duration::MAX).unwrap_err(),
        TruthError::Invalid("uninterrupted exposure")
    );
    assert_eq!(state.heat_seconds(ccd), heat);
}

#[test]
fn cold_history_needs_a_nonempty_topology_of_distinct_cores() {
    let scenario = scenario("successor-edge-shallow");
    let first = scenario.topology()[0].clone();
    assert_eq!(
        TruthState::cold(&[]).unwrap_err(),
        TruthError::Invalid("history topology")
    );
    assert_eq!(
        TruthState::cold(&[first.clone(), first.clone()]).unwrap_err(),
        TruthError::Invalid("duplicate core")
    );
    let one = TruthState::cold(std::slice::from_ref(&first)).unwrap();
    assert_eq!(one.heat_seconds(first.ccd), Some(0.0));
}
