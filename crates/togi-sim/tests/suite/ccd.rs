// The CCD residual hazard.
// Go: TestCCDLoadedHazardAndDraws (analytic part; the empirical draw share waits for the runtime),
// TestCCDValidation (validation.rs), TestCCDResidualDefersToExistingJoint.

use togi_machine::Regime;
use togi_sim::definition::{CcdDef, JointDef};
use togi_sim::{MachineDefinition, RegimeId, SourceKind};

use crate::common::{flat_cores, load, machine, model, offsets};

fn four_cores() -> MachineDefinition {
    let mut definition = machine(4, flat_cores(4, -50, -50));
    definition.ccd = Some(CcdDef {
        log_rate: 0.002f64.ln(),
        slope: 0.1,
        effect: [0.0, 1.0],
    });
    definition
}

#[test]
fn only_loaded_ccds_in_r7_contribute_by_their_mean_depth() {
    let model = model(&four_cores());
    let r7 = Regime::R7;
    let profile = offsets(&[-25, -25, -40, -40]);
    let first_ccd = load(&r7, "", &[0, 1], 90.0);
    let want = -(-0.002f64 * 90.0).exp_m1();
    let near = |got: f64, want: f64| assert!((got - want).abs() < 1e-12, "{got} vs {want}");
    near(
        model
            .failure_probability(&profile, &first_ccd)
            .expect("shape"),
        want,
    );
    near(model.hazard(&profile, &first_ccd).expect("shape"), 0.002);
    near(
        model
            .failure_probability(&offsets(&[-25, -25, 0, 0]), &first_ccd)
            .expect("shape"),
        want,
    );
    assert!(
        model
            .failure_probability(&offsets(&[-30, -30, 0, 0]), &first_ccd)
            .expect("shape")
            > want,
        "a deeper CCD is riskier"
    );
    let both = load(&r7, "", &[0, 2], 90.0);
    assert!(model.failure_probability(&profile, &both).expect("shape") > want);
    let r1 = Regime::R1;
    let outside = load(&r1, "", &[0, 1], 90.0);
    assert_eq!(model.failure_probability(&profile, &outside), Ok(0.0));
}

#[test]
fn the_effect_and_the_slope_shape_each_ccd() {
    let model = model(&four_cores());
    let r7 = Regime::R7;
    let rate_at = |cores: &[i64], profile: [i64; 4]| {
        model
            .hazard(&offsets(&profile), &load(&r7, "", cores, 90.0))
            .expect("shape")
    };
    let flat = rate_at(&[0], [-25, -25, -25, -25]);
    let other = rate_at(&[2], [-25, -25, -25, -25]);
    assert!((other / flat - 1f64.exp()).abs() < 1e-12, "effect[1] is 1");
    let deeper = rate_at(&[0], [-35, -35, 0, 0]);
    assert!(
        (deeper / flat - 1f64.exp()).abs() < 1e-12,
        "slope 0.1 over 10 counts"
    );
}

#[test]
fn an_active_joint_on_a_ccd_replaces_its_residual_even_before_the_joint_starts() {
    let mut definition = four_cores();
    definition.ccd = Some(CcdDef {
        log_rate: 0.002f64.ln(),
        ..CcdDef::default()
    });
    definition.joint = vec![JointDef {
        members: [(0, -20), (1, -20)].into(),
        regimes: vec![RegimeId::R7],
        rate: Some(0.01),
        after_s: 1000.0,
        ..JointDef::default()
    }];
    let model = model(&definition);
    let r7 = Regime::R7;
    let all = load(&r7, "", &[0, 1, 2, 3], 60.0);
    let profile = offsets(&[-25, -25, -25, -25]);
    let kinds: Vec<_> = model
        .sources(&profile, &all)
        .expect("shape")
        .iter()
        .map(|source| source.kind)
        .collect();
    assert_eq!(kinds, [SourceKind::Ccd(1), SourceKind::Joint(0)]);
    let hazard = model.hazard(&profile, &all).expect("shape");
    assert!((hazard - 0.012).abs() < 1e-12, "{hazard}");

    let inactive = offsets(&[-19, -25, -25, -25]);
    let kinds: Vec<_> = model
        .sources(&inactive, &all)
        .expect("shape")
        .iter()
        .map(|source| source.kind)
        .collect();
    assert_eq!(kinds, [SourceKind::Ccd(0), SourceKind::Ccd(1)]);
}
