use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use togi_facts::{Decisive, TrialClass};
use togi_machine::{Regime, WorkloadId, pick_workload};
use togi_sim::definition::{CcdDef, JointDef};
use togi_sim::{Load, MachineDefinition, Model};
use togi_tool_modelcheck::{Checker, Status};
use togi_tool_trialfacts::TrialFact;

use super::{SESSION, blank_machine, cores, crash, fact, profile, repeated};
use crate::evidence::{Observation, aggregate, core_def, initial_config, load_of};
use crate::likelihood::Likelihood;
use crate::search::{fit, fit_from};

fn observation(
    offsets: &[i64],
    regime: Regime,
    loaded: &[i64],
    duration_s: u32,
    n: u32,
    k: u32,
) -> Observation {
    Observation {
        profile: profile(offsets),
        class: TrialClass {
            workload: WorkloadId::new(regime, 0).unwrap(),
            loaded: cores(loaded),
            duration_s,
        },
        n,
        k,
    }
}

fn seed_fact() -> TrialFact {
    fact(&[0, 0], Regime::R1, &[0], 60, Decisive::Pass)
}

fn probability(
    model: &Model,
    offsets: &[i64],
    regime: Regime,
    loaded: &[i64],
    duration_s: u32,
) -> f64 {
    let class = TrialClass {
        workload: WorkloadId::new(regime, 0).unwrap(),
        loaded: cores(loaded),
        duration_s,
    };
    let load: Load<'_> = load_of(&class);
    model
        .failure_probability(profile(offsets).as_slice(), &load)
        .unwrap()
}

/// Draws `reps` outcomes of each trial class from `known`, seeded.
struct Draw<'a> {
    known: &'a Model,
    rng: StdRng,
    records: Vec<TrialFact>,
}

impl<'a> Draw<'a> {
    fn new(known: &'a Model, seed: u64) -> Self {
        Self {
            known,
            rng: StdRng::seed_from_u64(seed),
            records: Vec::new(),
        }
    }

    fn add(
        &mut self,
        offsets: &[i64],
        regime: Regime,
        loaded: &[i64],
        duration_s: u32,
        reps: usize,
    ) {
        let p = probability(self.known, offsets, regime, loaded, duration_s);
        for _ in 0..reps {
            let outcome = if self.rng.random_range(0.0..1.0) < p {
                crash()
            } else {
                Decisive::Pass
            };
            let mut record = fact(offsets, regime, loaded, duration_s, outcome);
            record.seq =
                togi_journal::Seq::new(u64::try_from(self.records.len()).unwrap() + 1).unwrap();
            self.records.push(record);
        }
    }
}

#[test]
fn a_fit_recovers_the_limits_hazard_shape_and_ccd_hazard_of_the_machine_that_drew_the_trials() {
    let mut known = blank_machine(4);
    known.model.past_limit_rate = 0.008;
    known.model.growth = 3.0;
    known.ccd = Some(CcdDef {
        log_rate: 0.006_f64.ln(),
        slope: 0.15,
        effect: [0.0, 0.6],
    });
    for (c, limits) in known.core.iter_mut().enumerate() {
        let c = i64::try_from(c).unwrap();
        limits.alone = [-20 - c; 5];
        limits.together = [-18 - c, -18 - c, -18 - c, -18 - c, -18 - c, -18 - c, -50];
    }
    let source = Model::new(&known, 0).unwrap();
    let mut draw = Draw::new(&source, 12);
    for core in 0..4_i64 {
        for regime in [Regime::R1, Regime::R2] {
            for alone in [true, false] {
                let limit = if alone { -20 - core } else { -18 - core };
                for delta in -1..=3 {
                    let mut offsets = [0_i64; 4];
                    if !alone {
                        offsets[usize::try_from((core + 1) % 4).unwrap()] = -5;
                    }
                    offsets[usize::try_from(core).unwrap()] = limit - delta;
                    draw.add(&offsets, regime, &[core], 60, 60);
                }
            }
        }
    }
    for ccd in 0..2_i64 {
        let loaded = [ccd * 2, ccd * 2 + 1];
        for depth in (15..=40).step_by(5) {
            let mut offsets = [0_i64; 4];
            offsets[usize::try_from(loaded[0]).unwrap()] = -depth;
            offsets[usize::try_from(loaded[1]).unwrap()] = -depth;
            draw.add(&offsets, Regime::R7, &loaded, 60, 150);
        }
    }
    let records = draw.records;
    let (got, _) = fit(&records).unwrap();
    for (found, want) in got.core.iter().zip(&known.core) {
        assert_eq!(&found.alone[..2], &want.alone[..2], "core {}", want.id);
        assert_eq!(
            &found.together[..2],
            &want.together[..2],
            "core {}",
            want.id
        );
    }
    assert!((got.model.past_limit_rate / known.model.past_limit_rate - 1.0).abs() < 0.25);
    assert!((got.model.growth - known.model.growth).abs() < 0.6);
    let fitted = Model::new(&got, 0).unwrap();
    for o in aggregate(&records)
        .iter()
        .filter(|o| o.regime() == Regime::R7)
    {
        let load = load_of(&o.class);
        let want = source
            .failure_probability(o.profile.as_slice(), &load)
            .unwrap();
        let actual = fitted
            .failure_probability(o.profile.as_slice(), &load)
            .unwrap();
        assert!(
            (actual - want).abs() < 0.12,
            "CCD profile {:?}: p={actual} want {want}",
            o.profile.as_slice()
        );
    }
}

#[test]
fn a_fit_recovers_a_flat_rate_and_an_idle_limit() {
    let mut known = blank_machine(2);
    known.model.past_limit_rate = 0.008;
    known.model.growth = 3.0;
    known.core[0].flat = 0.000_15;
    known.core[1].idle = Some(-30);
    let source = Model::new(&known, 0).unwrap();
    let mut draw = Draw::new(&source, 56);
    for (offsets, loaded, duration) in [
        ([0, 0], 0, 600),
        ([-5, 0], 0, 600),
        ([-15, 0], 0, 600),
        ([-25, 0], 0, 600),
        ([-5, -20], 1, 600),
        ([0, -20], 0, 600),
        ([0, -30], 0, 60),
        ([0, -31], 0, 60),
        ([0, -32], 0, 60),
        ([0, -33], 0, 60),
    ] {
        draw.add(&offsets, Regime::R1, &[loaded], duration, 150);
    }
    let (got, _) = fit(&draw.records).unwrap();
    assert_eq!(
        got.core[1].idle,
        Some(-30),
        "past_limit_rate={} growth={}",
        got.model.past_limit_rate,
        got.model.growth
    );
    assert!(
        (got.core[0].flat / known.core[0].flat - 1.0).abs() < 0.4,
        "flat {}",
        got.core[0].flat
    );
}

#[test]
fn a_constrained_refit_maximizes_the_resampled_likelihood_inside_the_original_evidence() {
    let mut base = blank_machine(2);
    base.core[0].flat = -(0.8_f64).ln() / 900.0;
    let template = fact(&[-10, 0], Regime::R1, &[0], 900, Decisive::Pass);
    let original = repeated(&template, 40, 8);
    let sample = repeated(&template, 40, 32);
    let checker = Checker::new(&Model::new(&base, 0).unwrap(), &original).unwrap();
    let before = base.clone();
    let (fitted, _) = fit_from(&sample, Some(&base), Some(&checker)).unwrap();
    let check = checker
        .check("synthetic", "synthetic", &Model::new(&fitted, 0).unwrap())
        .unwrap();
    assert_eq!(check.status, Status::Ok, "{check:?}");
    assert!(check.groups[0].mean_p > 0.25, "{}", check.groups[0].mean_p);
    assert_eq!(
        base, before,
        "the refit changed the all-facts starting machine"
    );
}

#[test]
fn a_constrained_refit_refits_the_ccd_hazard_and_keeps_the_r7_structure() {
    let mut base = blank_machine(2);
    base.ccd = Some(CcdDef {
        log_rate: (-(0.8_f64).ln() / 60.0).ln(),
        ..CcdDef::default()
    });
    base.joint = vec![JointDef {
        members: [(1, -40)].into(),
        regimes: vec![Regime::R7],
        rate: Some(0.001),
        ..JointDef::default()
    }];
    let before = base.clone();
    let template = fact(&[-20, -20], Regime::R7, &[0], 60, Decisive::Pass);
    let original = repeated(&template, 40, 8);
    let sample = repeated(&template, 40, 24);
    let checker = Checker::new(&Model::new(&base, 0).unwrap(), &original).unwrap();
    let (first, loss) = fit_from(&sample, Some(&base), Some(&checker)).unwrap();
    let check = checker
        .check("synthetic", "synthetic", &Model::new(&first, 0).unwrap())
        .unwrap();
    assert_eq!(check.status, Status::Ok, "{check:?}");
    assert!(check.groups[0].mean_p > 0.25, "{}", check.groups[0].mean_p);
    assert_ne!(first.ccd, before.ccd, "the CCD hazard did not refit");
    assert_eq!(
        first.joint, before.joint,
        "the refit changed the frozen joints"
    );
    for (found, want) in first.core.iter().zip(&before.core) {
        assert_eq!(found.together[6], want.together[6]);
        assert!(
            found.workload.is_empty(),
            "core {}: {:?}",
            found.id,
            found.workload
        );
    }
    assert_eq!(base, before, "the refit changed its all-facts seed");
    let (second, second_loss) = fit_from(&sample, Some(&base), Some(&checker)).unwrap();
    assert_eq!((first, loss.to_bits()), (second, second_loss.to_bits()));
}

#[test]
fn an_outcome_the_model_calls_impossible_costs_infinity_and_a_certain_one_costs_nothing() {
    let obs = [observation(&[-10, 0], Regime::R1, &[0], 90, 1, 0)];
    let mut obs = obs.to_vec();
    for (flat, failures, impossible) in [
        (0.0, 1, true),
        (100.0, 0, true),
        (0.0, 0, false),
        (100.0, 1, false),
    ] {
        let mut cfg = blank_machine(2);
        cfg.core[0].flat = flat;
        obs[0].k = failures;
        let l = Likelihood::new(cfg, &obs, None, None);
        let loss = l.value_of(l.model.as_ref().unwrap(), 0);
        if impossible {
            assert!(
                loss.is_infinite() && loss > 0.0,
                "flat={flat} failures={failures}: {loss}"
            );
        } else {
            assert!(
                loss.abs() < f64::EPSILON,
                "flat={flat} failures={failures}: {loss}"
            );
        }
    }
}

#[test]
fn the_joint_search_separates_a_clean_boundary_from_a_failing_one() {
    let mut cfg = initial_config(&[seed_fact()]);
    cfg.ccd = None;
    cfg.model.near_limit_rate = 0.0;
    cfg.joint = vec![JointDef {
        members: [(0, -10), (1, -10)].into(),
        regimes: vec![Regime::R7],
        rate: Some(0.01),
        ..JointDef::default()
    }];
    let obs = [
        observation(&[-20, -20], Regime::R7, &[0, 1], 60, 100, 0),
        observation(&[-30, -30], Regime::R7, &[0, 1], 60, 100, 50),
    ];
    let mut l = Likelihood::new(cfg, &obs, None, None);
    let before = l.score(&[0, 1]);
    l.fit_joints();
    l.fit_joints();
    let after = l.score(&[0, 1]);
    assert!(after < before - 1.0, "{before} -> {after}");
    let model = l.model.as_ref().unwrap();
    assert!(probability(model, &[-20, -20], Regime::R7, &[0, 1], 60).abs() < f64::EPSILON);
    let failing = probability(model, &[-30, -30], Regime::R7, &[0, 1], 60);
    assert!((failing - 0.5).abs() < 0.01, "{failing}");
}

#[test]
fn a_ninth_joint_on_a_ccd_is_never_added_but_an_eighth_is() {
    let template = fact(&[-30, 0], Regime::R7, &[0, 1], 60, Decisive::Pass);
    let mut records = repeated(&template, 10, 0);
    records.extend(repeated(&template, 90, 90));
    for count in [7_i64, 8] {
        let mut cfg = initial_config(&records);
        cfg.model.near_limit_rate = 0.0;
        cfg.joint = (0..count)
            .map(|j| JointDef {
                members: [(0, -20 - j)].into(),
                regimes: vec![Regime::R7],
                rate: Some(0.001),
                ..JointDef::default()
            })
            .collect();
        let want = cfg.clone();
        let obs = aggregate(&records);
        let mut l = Likelihood::new(cfg, &obs, None, None);
        let before = l.score(&[0]);
        l.add_joint(&records, 0);
        if count == 8 {
            assert_eq!(l.cfg, want, "a ninth joint was admitted");
        } else {
            assert_eq!(l.cfg.joint.len(), 8);
            assert!(l.score(&[0]) < before - 0.5);
            assert_eq!(l.cfg.joint[7].members, [(0, -30)].into());
        }
    }
}

#[test]
fn a_workload_override_needs_ten_trials_and_a_better_likelihood() {
    let id = pick_workload(Regime::R1, 0).id;
    for n in [9_u32, 10] {
        let mut cfg = initial_config(&[seed_fact()]);
        cfg.model.near_limit_rate = 0.0;
        let obs = [observation(&[-20, 0], Regime::R1, &[0], 60, n, 1)];
        let mut l = Likelihood::new(cfg, &obs, None, None);
        let before = l.score(&[0]);
        l.fit_workloads();
        assert_eq!(
            l.cfg.core[0].workload.contains_key(id),
            n >= 10,
            "{n} trials"
        );
        if n >= 10 {
            assert!(
                l.score(&[0]) < before,
                "the override did not explain the failure"
            );
        }
    }
}

#[test]
fn the_coupled_shift_moves_workload_limits_and_leaves_unsupported_ones() {
    let id = pick_workload(Regime::R1, 0).id;
    let mut cfg = initial_config(&[seed_fact()]);
    cfg.model.near_limit_rate = 0.0;
    cfg.model.past_limit_rate = 0.01;
    cfg.core[0].workload = [(id.to_owned(), -20), ("unsupported".to_owned(), -50)].into();
    let obs = [
        observation(&[-21, 0], Regime::R1, &[0], 60, 100, 0),
        observation(&[-24, 0], Regime::R1, &[0], 60, 100, 99),
    ];
    let mut l = Likelihood::new(cfg, &obs, None, None);
    let before = l.score(&[0, 1]);
    let deep = probability(l.model.as_ref().unwrap(), &[-24, 0], Regime::R1, &[0], 60);
    l.fit_limit_rate_shift(&[0, 1]);
    assert!(
        l.score(&[0, 1]) < before && l.cfg.core[0].workload[id] < -20,
        "{:?}",
        l.cfg.core[0].workload
    );
    let after = probability(l.model.as_ref().unwrap(), &[-24, 0], Regime::R1, &[0], 60);
    assert!(
        (after - deep).abs() < 1e-12,
        "the past-limit hazard changed: {deep} -> {after}"
    );
    assert_eq!(l.cfg.core[0].workload["unsupported"], -50);
    assert_eq!(l.cfg.core[0].alone[0], -50);
}

fn idle_machine() -> MachineDefinition {
    let mut cfg = blank_machine(2);
    cfg.model.past_limit_rate = 0.015;
    cfg.model.growth = 2.0;
    cfg.core[0].flat = 0.001;
    cfg
}

#[test]
fn the_idle_limit_keeps_the_total_likelihood_from_worsening() {
    let obs = [
        observation(&[-1, -1], Regime::R1, &[0], 60, 1, 1),
        observation(&[-1, 0], Regime::R1, &[0], 60, 100, 0),
    ];
    let mut l = Likelihood::new(idle_machine(), &obs, None, None);
    let all = l.all();
    let before = l.score(&all);
    l.fit_idle();
    assert_eq!(l.cfg.core[1].idle, Some(0));
    assert!(l.score(&all) <= before + 1e-9);
}

#[test]
fn an_idle_limit_needs_a_negative_offset_on_an_unloaded_core_to_be_fitted() {
    let obs = [
        observation(&[-1, 0], Regime::R1, &[0], 60, 1, 1),
        observation(&[0, -1], Regime::R1, &[1], 60, 100, 0),
    ];
    for (initial, want) in [(None, None), (Some(0), Some(0)), (Some(-50), None)] {
        let mut cfg = idle_machine();
        cfg.core[1].idle = initial;
        let mut l = Likelihood::new(cfg, &obs, None, None);
        l.fit_idle();
        assert_eq!(l.cfg.core[1].idle, want, "initial {initial:?}");
    }
}

#[test]
fn an_initial_machine_has_one_joint_per_ccd_whose_every_core_failed_below_zero() {
    let mut records = vec![fact(&[-30, -20, 0, 0], Regime::R7, &[0, 1], 60, crash())];
    records.push(fact(&[-25, -35, 0, 0], Regime::R7, &[0, 1], 60, crash()));
    let cfg = initial_config(&records);
    assert_eq!(cfg.joint.len(), 1);
    assert_eq!(cfg.joint[0].members, [(0, -25), (1, -20)].into());
    assert_eq!(cfg.core.len(), 4);
    assert_eq!(cfg.core[3], core_def(3));
    let _ = SESSION;
}
