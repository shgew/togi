// Steady rates, hazard sources and the model as the predictor.

use std::collections::BTreeMap;
use togi_machine::{CoreId, Regime};
use togi_sim::definition::{CcdDef, JointDef};
use togi_sim::{HazardSource, MachineDefinition, Model, ShapeError, SourceKind, saturating_count};

use test_case::test_case;

use crate::common::{committed_machine, flat_cores, load, machine, model, offsets, r7_workload};

fn base() -> MachineDefinition {
    let mut definition = machine(2, flat_cores(2, -10, -12));
    definition.model.past_limit_rate = 2.0;
    definition.model.growth = 4.0;
    definition.model.near_limit_rate = 0.25;
    definition.model.onset_boost = 100.0;
    definition.core[0].workload =
        BTreeMap::from([(togi_machine::pick_workload(Regime::R1, 0).id(), -8)]);
    definition
}

#[test]
fn a_count_is_exact_up_to_u32_max_and_saturates_beyond() {
    let max = usize::try_from(u32::MAX).expect("a 64-bit usize");
    assert_eq!(saturating_count(0).to_bits(), 0.0_f64.to_bits());
    assert_eq!(saturating_count(7) / saturating_count(2), 3.5);
    assert_eq!(saturating_count(max), f64::from(u32::MAX));
    assert_eq!(saturating_count(max + 1), f64::from(u32::MAX));
    assert_eq!(saturating_count(usize::MAX), f64::from(u32::MAX));
}

fn joint(members: &[(i64, i64)], regimes: &[Regime], rate: Option<f64>, after_s: f64) -> JointDef {
    JointDef {
        members: members.iter().copied().collect(),
        regimes: regimes.to_vec(),
        rate,
        after_s,
        ..JointDef::default()
    }
}

struct Case {
    profile: [i64; 2],
    cores: &'static [i64],
    regime: Regime,
    flat: f64,
    idle: Option<i64>,
    workload: &'static str,
    joint: Option<JointDef>,
    want: f64,
}

fn case(profile: [i64; 2], cores: &'static [i64], want: f64) -> Case {
    Case {
        profile,
        cores,
        regime: Regime::R1,
        flat: 0.0,
        idle: None,
        workload: "",
        joint: None,
        want,
    }
}

#[test_case(case([-12, 0], &[0], 8.0); "past the alone limit")]
#[test_case(case([-10, 0], &[0], 0.25); "at the alone limit")]
#[test_case(case([-12, -1], &[0], 0.25); "an outsider at a nonzero offset switches to the together limit")]
#[test_case(Case { flat: 0.5, ..case([-1, 0], &[], 0.5) }; "flat rate on an unloaded core at a nonzero offset")]
#[test_case(Case { flat: 0.5, ..case([0, 0], &[], 0.0) }; "flat rate on an unloaded core at offset zero")]
#[test_case(Case { flat: 0.5, ..case([0, 0], &[0], 0.25) }; "flat rate does not count on a loaded core at offset zero")]
#[test_case(Case { idle: Some(-8), ..case([-9, 0], &[], 2.0) }; "idle limit")]
#[test_case(Case { workload: "mprime-sse-4k-21k", ..case([-9, 0], &[0], 2.0) }; "workload limit")]
#[test_case(
    Case {
        regime: Regime::R7,
        joint: Some(joint(&[(0, -5), (1, -5)], &[Regime::R7], Some(3.0), 240.0)),
        ..case([-5, -5], &[], 3.0)
    };
    "an active joint counts at its steady rate whatever its delay"
)]
#[test_case(
    Case {
        regime: Regime::R7,
        joint: Some(joint(&[(0, -5), (1, -5)], &[], Some(3.0), 0.0)),
        ..case([-5, -4], &[], 0.0)
    };
    "a joint with one member short of its threshold is inactive"
)]
#[test_case(
    Case {
        joint: Some(joint(&[(0, -5)], &[Regime::R7], Some(3.0), 0.0)),
        ..case([-5, -5], &[], 0.0)
    };
    "a joint outside its regimes is inactive"
)]
#[test_case(
    Case {
        regime: Regime::R7,
        joint: Some(joint(&[(0, -5)], &[], None, 0.0)),
        ..case([-5, -5], &[], 2.0)
    };
    "a joint without a rate takes the model rate"
)]
fn steady_hazard_follows_the_limits_of_the_applied_registers(case: Case) {
    let mut definition = base();
    definition.core[0].flat = case.flat;
    definition.core[0].idle = case.idle;
    definition.joint = case.joint.into_iter().collect();
    let model = model(&definition);
    let load = load(case.regime, case.workload, case.cores, 90.0);
    let got = model
        .hazard(&offsets(&case.profile), &load)
        .expect("a matching profile");
    assert_eq!(got, case.want);
}

#[test]
fn r6_and_r7_use_the_together_limit_even_when_alone() {
    let definition = base();
    let model = model(&definition);
    let profile = offsets(&[-11, 0]);
    let hazard = |regime: Regime| {
        let load = load(regime, "", &[0], 90.0);
        model.hazard(&profile, &load).expect("shape")
    };
    assert_eq!(hazard(Regime::R5), 2.0, "alone limit -10");
    assert_eq!(hazard(Regime::R6), 0.25, "together limit -12");
    assert_eq!(hazard(Regime::R7), 0.25, "together limit -12");
}

#[test]
fn a_limit_of_one_fails_at_offset_zero() {
    let mut definition = base();
    definition.core[0].alone = [1; 5];
    let model = model(&definition);
    let regime = Regime::R1;
    let load = load(regime, "", &[0], 90.0);
    assert_eq!(model.hazard(&offsets(&[0, 0]), &load).expect("shape"), 2.0);
}

#[test]
fn near_limit_rate_applies_at_the_limit_and_nothing_below_it() {
    let mut definition = base();
    definition.model.near_limit_rate = 0.0;
    let model = model(&definition);
    let regime = Regime::R1;
    let load = load(regime, "", &[0], 90.0);
    assert_eq!(model.hazard(&offsets(&[-10, 0]), &load), Ok(0.0));
    assert_eq!(model.hazard(&offsets(&[-11, 0]), &load), Ok(2.0));
    assert_eq!(model.hazard(&offsets(&[-12, 0]), &load), Ok(8.0));
}

#[test]
fn profiles_and_loads_that_do_not_fit_the_machine_are_refused() {
    let model = model(&base());
    let regime = Regime::R1;
    let ok = load(regime, "", &[0], 90.0);
    let expect = ShapeError::Profile {
        expected: 2,
        found: 3,
    };
    assert_eq!(model.hazard(&offsets(&[0, 0, 0]), &ok), Err(expect));
    assert_eq!(
        model.failure_probability(&offsets(&[0, 0, 0]), &ok),
        Err(expect)
    );
    assert_eq!(
        model.sources(&offsets(&[0, 0, 0]), &ok).unwrap_err(),
        expect
    );
    let outside = load(regime, "", &[2], 90.0);
    assert_eq!(
        model.hazard(&offsets(&[0, 0]), &outside),
        Err(ShapeError::LoadedCore { core: 2, cores: 2 })
    );
}

fn kinds(sources: &[HazardSource]) -> Vec<SourceKind> {
    sources.iter().map(|source| source.kind).collect()
}

fn core(id: i64) -> CoreId {
    CoreId::new(id).expect("a core")
}

#[test]
fn sources_come_in_one_fixed_tie_order() {
    let mut definition = machine(4, flat_cores(4, -10, -10));
    for core in &mut definition.core {
        core.flat = 0.01;
    }
    definition.ccd = Some(CcdDef {
        log_rate: -5.0,
        ..CcdDef::default()
    });
    definition.joint = vec![
        joint(&[(0, -20)], &[], Some(0.1), 0.0),
        joint(&[(1, -20)], &[], Some(0.2), 5.0),
    ];
    let model = model(&definition);
    let regime = Regime::R1;
    let profile = offsets(&[-20, -20, -20, -20]);
    let loaded = load(regime, "", &[2, 0], 90.0);
    let sources = model.sources(&profile, &loaded).expect("shape");
    assert_eq!(
        kinds(&sources),
        [
            SourceKind::Core(core(0)),
            SourceKind::Idle(core(1)),
            SourceKind::Core(core(2)),
            SourceKind::Idle(core(3)),
            SourceKind::Joint(0),
            SourceKind::Joint(1),
        ]
    );
    assert_eq!(sources[5].after_s, 5.0);
    assert_eq!(sources[5].rate, 0.2);

    let r7 = Regime::R7;
    let loaded = load(r7, "", &[0, 2], 90.0);
    let sources = model.sources(&profile, &loaded).expect("shape");
    let order = kinds(&sources);
    let position = |kind| order.iter().position(|&known| known == kind);
    assert!(position(SourceKind::Idle(core(3))) < position(SourceKind::Ccd(1)));
    assert!(position(SourceKind::Ccd(1)) < position(SourceKind::Joint(0)));
    assert!(position(SourceKind::Joint(0)) < position(SourceKind::Joint(1)));
    assert!(
        position(SourceKind::Ccd(0)).is_none(),
        "the joint on core 0 explains CCD 0"
    );
}

#[test]
fn a_source_names_its_random_domain() {
    use togi_sim::random::Domain;
    let source = |kind| HazardSource {
        kind,
        rate: 1.0,
        after_s: 0.0,
    };
    assert_eq!(source(SourceKind::Core(core(1))).domain(), Domain::Core);
    assert_eq!(source(SourceKind::Idle(core(1))).domain(), Domain::Idle);
    assert_eq!(
        source(SourceKind::Voltage(core(1))).domain(),
        Domain::Voltage
    );
    assert_eq!(source(SourceKind::Ccd(1)).domain(), Domain::Ccd(1));
    assert_eq!(source(SourceKind::Joint(7)).domain(), Domain::Joint(7));
    assert_eq!(source(SourceKind::Background).domain(), Domain::Background);
}

#[cfg(target_pointer_width = "64")]
#[test_case(u64::from(u32::MAX) - 1; "below the old key limit")]
#[test_case(u64::from(u32::MAX); "at the old key limit")]
#[test_case(u64::from(u32::MAX) + 1; "above the old key limit")]
#[test_case(u64::MAX; "largest platform index")]
#[expect(clippy::unwrap_used, reason = "a 64-bit platform represents every u64 index")]
fn joint_domains_preserve_the_whole_index(index: u64) {
    let source = HazardSource {
        kind: SourceKind::Joint(usize::try_from(index).unwrap()),
        rate: 1.0,
        after_s: 0.0,
    };
    assert_eq!(source.domain(), togi_sim::random::Domain::Joint(index));
}

#[test]
fn sources_sum_to_the_steady_hazard_and_scale_to_the_probability() {
    let mut definition = machine(4, flat_cores(4, -30, -25));
    definition.core[1].flat = 1e-5;
    definition.core[2].idle = Some(-20);
    definition.core[3].workload = BTreeMap::from([("mprime-sse-4k-21k".parse().unwrap(), -12)]);
    definition.model.onset_boost = 3.0;
    definition.model.onset_s = 30.0;
    definition.ccd = Some(CcdDef {
        log_rate: -7.0,
        slope: 0.05,
        effect: [0.5, -0.5],
    });
    definition.joint = vec![
        joint(&[(0, -20), (1, -22)], &[Regime::R7], Some(0.01), 20.0),
        joint(&[(2, -15), (3, -15)], &[], Some(0.002), 0.0),
    ];
    let model = model(&definition);
    let onset = model.params().onset;
    let profiles = [
        [0, 0, 0, 0],
        [-10, 0, 0, 0],
        [-26, -26, -10, -10],
        [-31, -23, -21, -16],
        [-40, -40, -40, -40],
        [0, 0, -17, -30],
    ];
    let regimes = togi_machine::REGIMES;
    for profile in profiles {
        let profile = offsets(&profile);
        for regime in regimes {
            for cores in [&[0][..], &[2], &[0, 1], &[1, 3], &[0, 1, 2, 3]] {
                for workload in ["", "mprime-sse-4k-21k"] {
                    let case = format!("{profile:?} {regime:?} cores {cores:?} {workload:?}");
                    let load = load(regime, workload, cores, 90.0);
                    let sources = model.sources(&profile, &load).expect("shape");
                    let rate: f64 = sources.iter().map(|source| source.rate).sum();
                    assert_eq!(model.hazard(&profile, &load), Ok(rate), "{case}");
                    let failures: f64 = sources
                        .iter()
                        .map(|source| onset.failures(source.after_s, source.rate, 90.0))
                        .sum();
                    let exposure: f64 = sources
                        .iter()
                        .map(|source| source.rate * onset.exposure(source.after_s, 90.0))
                        .sum();
                    assert!(
                        (failures - exposure).abs() <= 1e-12 * exposure.max(1.0),
                        "{case}: {failures} vs {exposure}"
                    );
                    let probability = model.failure_probability(&profile, &load).expect("shape");
                    assert_eq!(probability, -(-failures).exp_m1(), "{case}");
                    assert!((0.0..=1.0).contains(&probability), "{case}: {probability}");
                }
            }
        }
    }
}

#[test]
fn sources_with_no_rate_are_left_out_so_a_quiet_machine_has_none() {
    let model = model(&machine(2, flat_cores(2, -10, -10)));
    let regime = Regime::R7;
    let load = load(regime, "", &[0], 90.0);
    assert_eq!(model.sources(&offsets(&[-5, 0]), &load), Ok(vec![]));
    assert_eq!(
        model.failure_probability(&offsets(&[-5, 0]), &load),
        Ok(0.0)
    );
}

#[test]
fn the_model_is_the_predictor_with_no_limits_required() {
    let model = Model::new(&MachineDefinition::default(), 11).expect("seeded limits");
    let regime = Regime::R1;
    let load = load(regime, "", &[3], 90.0);
    let profile = offsets(&[0; 16]);
    let deep = offsets(&[-50; 16]);
    let safe = model.failure_probability(&profile, &load).expect("shape");
    let risky = model.failure_probability(&deep, &load).expect("shape");
    assert!(risky > safe);
}

#[test]
fn a_model_owns_its_data_and_clones_share_it() {
    let mut definition = base();
    definition.core[0].idle = Some(-9);
    definition.joint = vec![joint(&[(0, -20)], &[], Some(0.5), 0.0)];
    definition.ccd = Some(CcdDef::default());
    let owned = model(&definition);
    let regime = Regime::R1;
    // Short enough not to saturate: a probability of 1 would be the same for any configuration.
    let load = load(regime, "", &[1], 0.001);
    let profile = offsets(&[-10, -10]);
    let sources = owned.sources(&profile, &load).expect("shape");
    let hazard = owned.hazard(&profile, &load).expect("shape");
    let before = owned.failure_probability(&profile, &load).expect("shape");
    assert_eq!(
        hazard,
        0.25 + 2.0,
        "the loaded core's near-limit and core 0's idle rate"
    );
    assert!((0.1..0.9).contains(&before), "{before}");

    definition.core[0].idle = Some(-50);
    definition.core[0].flat = 0.5;
    definition.joint[0].members.insert(0, -50);
    definition.joint[0].rate = Some(9.0);
    definition.ccd = None;
    definition.model.past_limit_rate = 0.5;

    let rebuilt = model(&definition);
    assert_ne!(
        rebuilt.sources(&profile, &load).expect("shape"),
        sources,
        "the mutation changes a model built from the definition"
    );
    assert_ne!(rebuilt.hazard(&profile, &load), Ok(hazard));
    assert_ne!(rebuilt.failure_probability(&profile, &load), Ok(before));

    let clone = owned.clone();
    drop(owned);
    assert_eq!(clone.sources(&profile, &load), Ok(sources));
    assert_eq!(clone.hazard(&profile, &load), Ok(hazard));
    assert_eq!(clone.failure_probability(&profile, &load), Ok(before));
}

#[test]
fn a_model_is_cheap_to_share_between_threads() {
    fn shareable<T: Send + Sync + Clone>() {}
    shareable::<Model>();
}

#[test_case(Regime::R7, &[0]; "r7 on one core")]
#[test_case(Regime::R2, &[0, 1]; "r2 on two cores")]
#[test_case(Regime::R1, &[0]; "r1 on one core")]
fn shared_voltage_changes_nothing_for_trials_that_do_not_load_two_cores_in_r7(
    regime: Regime,
    cores: &[i64],
) {
    let plain = committed_machine("shared-voltage.json");
    let mut legacy = plain.clone();
    legacy.shared_voltage = None;
    let with = Model::new(&plain, 3).expect("valid");
    let without = Model::new(&legacy, 3).expect("valid");
    let profile = offsets(&[-35; 16]);
    let load = load(regime, r7_workload(0), cores, 120.0);
    assert_eq!(
        with.sources(&profile, &load).expect("shape"),
        without.sources(&profile, &load).expect("shape")
    );
}
