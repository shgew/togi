// Integrated failure probability, the onset window and its inverse.

use togi_machine::Regime;
use togi_sim::definition::{CoreDef, JointDef};
use togi_sim::model::Onset;

use test_case::test_case;

use crate::common::{core_def, load, machine, model, offsets};

#[test_case((10.0, 0.0, 0.0, 0.0), (0.1, 0.0, 0.0), 1.0; "steady")]
#[test_case((10.0, 0.0, 4.0, 2.0), (0.1, 0.0, 0.0), 1.8; "onset")]
#[test_case((2.0, 0.0, 4.0, 2.0), (0.1, 0.0, 0.0), 0.6; "within onset")]
#[test_case((10.0, 0.0, 0.0, 0.0), (0.1, 0.2, 0.0), 3.0; "unloaded")]
#[test_case((10.0, 3.0, 4.0, 2.0), (0.0, 0.0, 0.1), 0.9; "delayed joint")]
#[test_case((10.0, 12.0, 4.0, 2.0), (0.0, 0.0, 0.1), 0.0; "joint after the end")]
#[test_case((10.0, 6.0, 4.0, 2.0), (0.0, 0.0, 0.1), 0.4; "joint after the onset window")]
#[test_case((10.0, 10.0, 4.0, 2.0), (0.0, 0.0, 0.1), 0.0; "joint starting at the end")]
fn probability_integrates_loaded_idle_and_delayed_joint_exposure(
    (duration, after, onset, boost): (f64, f64, f64, f64),
    (loaded, idle, joint): (f64, f64, f64),
    hazard: f64,
) {
    let limits = |flat| CoreDef {
        flat,
        ..core_def(0, 0, 0)
    };
    let mut definition = machine(
        2,
        vec![
            limits(loaded),
            CoreDef {
                id: 1,
                ..limits(idle)
            },
        ],
    );
    definition.model.past_limit_rate = 0.0;
    definition.model.near_limit_rate = 0.0;
    definition.model.onset_s = onset;
    definition.model.onset_boost = boost;
    definition.joint = vec![JointDef {
        members: [(0, -10), (1, -10)].into(),
        rate: Some(joint),
        after_s: after,
        ..JointDef::default()
    }];
    let model = model(&definition);
    let load = load(Regime::R1, "", &[0], duration);
    let got = model
        .failure_probability(&offsets(&[-10, -10]), &load)
        .expect("shape");
    let want = -(-hazard).exp_m1();
    assert!((got - want).abs() < 1e-14, "{got} vs {want}");
}

#[test]
fn exposure_counts_boosted_seconds_once_more() {
    let onset = Onset {
        window_s: 4.0,
        boost: 2.0,
    };
    assert_eq!(onset.exposure(0.0, 10.0), 10.0 + 2.0 * 4.0);
    assert_eq!(onset.exposure(0.0, 2.0), 2.0 + 2.0 * 2.0);
    assert_eq!(onset.exposure(3.0, 10.0), 7.0 + 2.0 * 1.0);
    assert_eq!(onset.exposure(6.0, 10.0), 4.0, "after the window");
    assert_eq!(onset.exposure(12.0, 10.0), 0.0, "after the end");
    let flat = Onset {
        window_s: 4.0,
        boost: 0.0,
    };
    assert_eq!(flat.exposure(0.0, 10.0), 10.0);
}

#[test]
fn a_failure_time_is_the_inverse_of_exposure() {
    for window_s in [0.0, 4.0, 100.0] {
        for boost in [0.0, 0.5, 2.0, 100.0] {
            let onset = Onset { window_s, boost };
            for after in [0.0, 1.0, 3.0, 6.0, 150.0] {
                for rate in [1e-4, 0.1, 3.0] {
                    for cumulative in [1e-6, 0.01, 0.5, 1.0, 4.0, 20.0] {
                        let time = onset.failure_time(after, rate, cumulative);
                        assert!(time >= after);
                        let back = rate * onset.exposure(after, time);
                        assert!(
                            (back - cumulative).abs() <= 1e-9 * cumulative.max(1.0),
                            "window {window_s} boost {boost} after {after} rate {rate}: {back} vs {cumulative}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn a_tiny_rate_cannot_fail_inside_a_trial() {
    let onset = Onset {
        window_s: 100.0,
        boost: 2.0,
    };
    let time = onset.failure_time(0.0, 1e-300, 1.0);
    assert!(time > 1e6, "{time}");
}

#[test]
fn a_window_count_whose_boosted_rate_overflows_is_scored_and_drawn_alike() {
    // 1e308 · (1 + 10) overflows before the 1e-309 seconds it is multiplied by, yet the count is 1.1.
    let window = 1e-309;
    let onset = Onset {
        window_s: window,
        boost: 10.0,
    };
    let rate = 1e308;
    let count = onset.failures(0.0, rate, window);
    assert!((count - 1.1).abs() < 1e-9, "{count}");
    assert_eq!(
        onset.failures(0.0, rate, 2.0 * window),
        count + rate * window
    );
    assert_eq!(onset.failures(0.0, rate, 0.0), 0.0);

    // A draw is inside the window exactly when its count is at most the window's.
    for (cumulative, inside) in [(0.5, true), (1.09, true), (1.11, false), (2.0, false)] {
        let time = onset.failure_time(0.0, rate, cumulative);
        assert_eq!(time <= window, inside, "{cumulative}: {time}");
    }
    let half = onset.failure_time(0.0, rate, count / 2.0);
    assert!((half / window - 0.5).abs() < 1e-6, "{half}");
    let after = onset.failure_time(0.0, rate, count + 1.0);
    assert!(
        (after / window - 1.0 - 1.0 / (rate * window)).abs() < 1e-6,
        "{after}"
    );

    // The count exceeds f64::MAX only when it truly does.
    let huge = Onset {
        window_s: 1.0,
        boost: 1e308,
    };
    assert!(huge.failures(0.0, 1e308, 1.0).is_infinite());
    let drawn = huge.failure_time(0.0, 1e308, 1.0);
    assert!((0.0..1e-300).contains(&drawn), "{drawn}");
}

#[test]
fn a_boost_whose_exposure_overflows_still_scores_the_failures_it_draws() {
    let rate = 1e-309;
    let mut definition = machine(
        2,
        vec![
            CoreDef {
                flat: rate,
                ..core_def(0, -10, -10)
            },
            core_def(1, -10, -10),
        ],
    );
    definition.model.past_limit_rate = 0.0;
    definition.model.near_limit_rate = 0.0;
    definition.model.growth = 1.0;
    definition.model.onset_s = 10.0;
    definition.model.onset_boost = 1e308;
    let model = model(&definition);
    let onset = model.params().onset;
    assert!(
        onset.exposure(0.0, 10.0).is_infinite(),
        "a unit rate's boosted exposure does not fit"
    );
    let load = load(Regime::R1, "", &[0], 10.0);
    let profile = offsets(&[-5, 0]);
    let sources = model.sources(&profile, &load).expect("shape");
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].rate, rate);

    // The boosted rate is about 0.1 per second, so the ten seconds hold about one failure.
    let failures = onset.failures(0.0, rate, 10.0);
    assert!((failures - 1.0).abs() < 1e-9, "{failures}");
    let got = model.failure_probability(&profile, &load).expect("shape");
    assert_eq!(got, -(-failures).exp_m1());
    assert!((got - (1.0 - (-1.0f64).exp())).abs() < 1e-9, "{got}");

    // Drawing inverts the same count: half of it is reached halfway, twice of it after the end.
    let half = onset.failure_time(0.0, rate, failures / 2.0);
    assert!((half - 5.0).abs() < 1e-6, "{half}");
    assert!(onset.failure_time(0.0, rate, failures * 2.0) > 10.0);
}

#[test]
fn a_growth_power_that_underflows_cannot_silence_a_representable_rate() {
    let mut definition = machine(
        2,
        vec![
            core_def(0, -10, -10),
            CoreDef {
                idle: Some(-10),
                ..core_def(1, -10, -10)
            },
        ],
    );
    definition.model.past_limit_rate = 1e308;
    definition.model.growth = 1e-163;
    definition.model.near_limit_rate = 0.0;
    let model = model(&definition);
    // Three counts past the limit: 1e308 · (1e-163)² is 1e-18, though (1e-163)² alone is 0.
    let profile = offsets(&[-13, -13]);
    let load = load(Regime::R1, "", &[0], 1e18);
    let sources = model.sources(&profile, &load).expect("shape");
    assert_eq!(
        sources.iter().map(|source| source.kind).collect::<Vec<_>>(),
        [
            togi_sim::SourceKind::Core(crate::common::core(0)),
            togi_sim::SourceKind::Idle(crate::common::core(1)),
        ]
    );
    for source in &sources {
        assert!((source.rate - 1e-18).abs() <= 1e-9 * 1e-18, "{source:?}");
    }
    let got = model.failure_probability(&profile, &load).expect("shape");
    assert!((got - (1.0 - (-2.0f64).exp())).abs() < 1e-9, "{got}");
}

#[test_case(0.0, 0.0; "no time")]
#[test_case(1.0, 1.0; "one second")]
#[test_case(10.0, 1.0; "the whole onset window")]
#[test_case(100.0, 1.0; "past the onset window")]
fn rates_at_the_edge_of_f64_never_turn_a_probability_into_nan(seconds: f64, want: f64) {
    let flat = |id| CoreDef {
        flat: 1e308,
        ..core_def(id, -10, -10)
    };
    let mut definition = machine(2, vec![flat(0), flat(1)]);
    definition.model.past_limit_rate = 0.0;
    definition.model.near_limit_rate = 0.0;
    definition.model.growth = 1.0;
    definition.model.onset_s = 10.0;
    definition.model.onset_boost = 1e308;
    let model = model(&definition);
    let profile = offsets(&[-5, -5]);
    let load = load(Regime::R1, "", &[0, 1], seconds);
    let sources = model.sources(&profile, &load).expect("shape");
    assert!(sources.iter().all(|source| source.rate.is_finite()));
    assert_eq!(model.failure_probability(&profile, &load), Ok(want));
}
