// Integrated failure probability, the onset window and its inverse.
// Go: TestFailureProbability (its negative-onset-boost row is now a validation error, see validation.rs),
// the analytic half of TestModelHazards and TestFailureDrawAfterOnset (the exact draws are replaced by the
// inverse-of-exposure property, which is what makes draws agree with scores).

use togi_machine::Regime;
use togi_sim::definition::{CoreDef, JointDef};
use togi_sim::model::Onset;

use crate::common::{core_def, load, machine, model, offsets};

struct Row {
    name: &'static str,
    duration: f64,
    after: f64,
    onset: f64,
    boost: f64,
    loaded: f64,
    idle: f64,
    joint: f64,
    hazard: f64,
}

fn row(
    name: &'static str,
    (duration, after, onset, boost): (f64, f64, f64, f64),
    (loaded, idle, joint): (f64, f64, f64),
    hazard: f64,
) -> Row {
    Row {
        name,
        duration,
        after,
        onset,
        boost,
        loaded,
        idle,
        joint,
        hazard,
    }
}

#[test]
fn probability_integrates_loaded_idle_and_delayed_joint_exposure() {
    let rows = [
        row("steady", (10.0, 0.0, 0.0, 0.0), (0.1, 0.0, 0.0), 1.0),
        row("onset", (10.0, 0.0, 4.0, 2.0), (0.1, 0.0, 0.0), 1.8),
        row("within onset", (2.0, 0.0, 4.0, 2.0), (0.1, 0.0, 0.0), 0.6),
        row("unloaded", (10.0, 0.0, 0.0, 0.0), (0.1, 0.2, 0.0), 3.0),
        row("delayed joint", (10.0, 3.0, 4.0, 2.0), (0.0, 0.0, 0.1), 0.9),
        row(
            "joint after the end",
            (10.0, 12.0, 4.0, 2.0),
            (0.0, 0.0, 0.1),
            0.0,
        ),
        row(
            "joint after the onset window",
            (10.0, 6.0, 4.0, 2.0),
            (0.0, 0.0, 0.1),
            0.4,
        ),
        row(
            "joint starting at the end",
            (10.0, 10.0, 4.0, 2.0),
            (0.0, 0.0, 0.1),
            0.0,
        ),
    ];
    for row in rows {
        let limits = |flat| CoreDef {
            flat,
            ..core_def(0, 0, 0)
        };
        let mut definition = machine(
            2,
            vec![
                limits(row.loaded),
                CoreDef {
                    id: 1,
                    ..limits(row.idle)
                },
            ],
        );
        definition.model.past_limit_rate = 0.0;
        definition.model.near_limit_rate = 0.0;
        definition.model.onset_s = row.onset;
        definition.model.onset_boost = row.boost;
        definition.joint = vec![JointDef {
            members: [(0, -10), (1, -10)].into(),
            rate: Some(row.joint),
            after_s: row.after,
            ..JointDef::default()
        }];
        let model = model(&definition);
        let regime = Regime::R1;
        let load = load(regime, "", &[0], row.duration);
        let got = model
            .failure_probability(&offsets(&[-10, -10]), &load)
            .expect("shape");
        let want = -(-row.hazard).exp_m1();
        assert!((got - want).abs() < 1e-14, "{}: {got} vs {want}", row.name);
    }
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
