use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};
use togi_facts::{Decisive, TrialClass};
use togi_machine::{Regime, WorkloadId, pick_workload};
use togi_sim::{Load, MachineDefinition, Model};
use togi_tool_modelcheck::{Checker, Status};
use togi_tool_trialfacts::{Profile, TrialFact};

use super::{blank_machine, cores, crash, fact, padded, refs, repeated};
use crate::error::Error;
use crate::evidence::{Observation, core_def, initial_config, load_of};
use crate::likelihood::Likelihood;
use crate::search::{fit, fit_from};
use crate::voltage::voltage_config;

/// The 16-core profile whose first offsets are `offsets`.
fn profile(offsets: &[i64]) -> Profile {
    super::profile(&padded(offsets))
}

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

#[test_case::test_case(true, false; "with a rail")]
#[test_case::test_case(false, true; "without a rail")]
fn r9_multi_core_rail_loads_are_not_limit_driven(rail: bool, expected: bool) {
    let o = observation(&[-20, -20], Regime::R9, &[0, 1], 60, 1, 0);
    assert_eq!(crate::search::limit_driven(rail, &o), expected);
}

#[test]
fn r8_observations_use_the_r2_limit_slot() {
    let o = observation(&[-20], Regime::R8, &[0], 60, 1, 0);
    assert_eq!(
        crate::search::limit_driven(true, &o),
        crate::search::limit_driven(true, &observation(&[-20], Regime::R2, &[0], 60, 1, 0))
    );
    assert_eq!(
        togi_sim::limit_index(o.regime()),
        togi_sim::limit_index(Regime::R2)
    );
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
fn a_fit_recovers_the_limits_and_hazard_shape_of_the_machine_that_drew_the_trials() {
    let mut known = blank_machine(16);
    known.model.past_limit_rate = 0.008;
    known.model.growth = 3.0;
    for (c, limits) in known.core.iter_mut().enumerate().take(4) {
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
    let (got, _) = fit(&refs(&draw.records)).unwrap();
    for (found, want) in got.core.iter().zip(&known.core).take(4) {
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
}

/// `reps` trials of each class drawn, with seed 12, from `known` in `regimes`: cores 0 to 3 alone and
/// together, from one count inside their [`one_limit_machine`] limit to three past it.
fn one_limit_draw(known: &MachineDefinition, regimes: &[Regime], reps: usize) -> Vec<TrialFact> {
    let source = Model::new(known, 0).unwrap();
    let mut draw = Draw::new(&source, 12);
    for core in 0..4_i64 {
        for &regime in regimes {
            for alone in [true, false] {
                for delta in -1..=3 {
                    let mut offsets = [0_i64; 4];
                    if !alone {
                        offsets[usize::try_from((core + 1) % 4).unwrap()] = -5;
                    }
                    offsets[usize::try_from(core).unwrap()] = -22 - core - delta;
                    draw.add(&offsets, regime, &[core], 60, reps);
                }
            }
        }
    }
    draw.records
}

/// Cores 0 to 3 with one limit, -22 to -25, for alone and together trials in R1 to R6: the structure
/// #427 adopts.
fn one_limit_machine() -> MachineDefinition {
    let mut known = blank_machine(16);
    known.model.past_limit_rate = 0.008;
    known.model.growth = 3.0;
    for (c, limits) in known.core.iter_mut().enumerate().take(4) {
        let limit = -22 - i64::try_from(c).unwrap();
        limits.alone = [limit; 5];
        limits.together = [limit, limit, limit, limit, limit, limit, -50];
    }
    known
}

#[test]
fn a_fit_recovers_one_limit_shared_by_alone_and_together_trials() {
    let known = one_limit_machine();
    let records = one_limit_draw(&known, &[Regime::R1, Regime::R2, Regime::R3], 60);
    let (got, _) = fit(&refs(&records)).unwrap();
    for (found, want) in got.core.iter().zip(&known.core).take(4) {
        assert_eq!(&found.alone[..3], &want.alone[..3], "core {}", want.id);
        assert_eq!(
            &found.together[..3],
            &want.together[..3],
            "core {}",
            want.id
        );
    }
    assert!((got.model.past_limit_rate / known.model.past_limit_rate - 1.0).abs() < 0.25);
    assert!((got.model.growth - known.model.growth).abs() < 0.6);
}

#[test]
fn a_fit_of_seeded_draws_is_bit_identical_on_every_worker_count() {
    let known = one_limit_machine();
    let fit_on = |threads: usize| {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        let (machine, objective) = pool
            .install(|| fit(&refs(&one_limit_draw(&known, &[Regime::R1], 20))))
            .unwrap();
        (
            togi_sim::file::encode(&machine).unwrap(),
            objective.to_bits(),
        )
    };
    let one = fit_on(1);
    assert_eq!(fit_on(4), one);
    assert_eq!(fit_on(1), one);
}

#[test]
fn a_fit_of_a_sample_that_is_not_sixteen_cores_is_refused() {
    let mut small = seed_fact();
    small.profile = super::profile(&[0, -1]);
    let error = fit(&[&small]).unwrap_err();
    assert!(
        matches!(error, Error::NeedsSixteenCores { cores: 2, .. }),
        "{error}"
    );
}

#[test]
fn a_constrained_refit_maximizes_the_resampled_likelihood_inside_the_original_evidence() {
    let template = fact(&[-10, 0], Regime::R1, &[0], 900, Decisive::Pass);
    let sample = repeated(&template, 40, 32);
    let sample = refs(&sample);
    let mut base = blank_machine(16);
    base.model.past_limit_rate = -(0.8_f64).ln() / 900.0;
    base.core[0].alone[0] = -9;
    base.shared_voltage = Some(voltage_config(&sample));
    let p = probability(
        &Model::new(&base, 0).unwrap(),
        &[-10, 0],
        Regime::R1,
        &[0],
        900,
    );
    assert!((0.1..0.5).contains(&p), "the fixture's base prediction {p}");
    let failing = (0..=40_usize)
        .min_by(|&a, &b| {
            let distance = |k: usize| (f64::from(u32::try_from(k).unwrap()) - 40.0 * p).abs();
            distance(a).total_cmp(&distance(b))
        })
        .unwrap();
    let original = repeated(&template, 40, failing);
    let checker = Checker::new(&Model::new(&base, 0).unwrap(), &original).unwrap();
    let before = base.clone();
    let (fitted, _) = fit_from(&sample, Some(&base), Some(&checker)).unwrap();
    let check = checker
        .check("synthetic", "synthetic", &Model::new(&fitted, 0).unwrap())
        .unwrap();
    assert_eq!(check.status, Status::Ok, "{check:?}");
    assert!(check.groups[0].mean_p > p, "{}", check.groups[0].mean_p);
    assert_eq!(
        base, before,
        "the refit changed the all-facts starting machine"
    );
    let (again, _) = fit_from(&sample, Some(&base), Some(&checker)).unwrap();
    assert_eq!(fitted, again);
}

#[test_case::test_case((0.0, 1, true); "0 0 1 true")]
#[test_case::test_case((100.0, 0, true); "100 0 0 true")]
#[test_case::test_case((0.0, 0, false); "0 0 0 false")]
#[test_case::test_case((100.0, 1, false); "100 0 1 false")]
fn an_outcome_the_model_calls_impossible_costs_infinity_and_a_certain_one_costs_nothing(
    case: (f64, u32, bool),
) {
    let (flat, failures, impossible) = case;
    let obs = [observation(&[-10, 0], Regime::R1, &[0], 90, 1, 0)];
    let mut obs = obs.to_vec();

    let mut cfg = blank_machine(16);
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

#[test_case::test_case(9_u32; "9 u32")]
#[test_case::test_case(10; "10")]
fn a_workload_override_needs_ten_trials_and_a_better_likelihood(case: u32) {
    let n = case;
    let id = pick_workload(Regime::R1, 0).id();

    let mut cfg = initial_config(&[&seed_fact()]);
    cfg.model.near_limit_rate = 0.0;
    let obs = [observation(&[-20, 0], Regime::R1, &[0], 60, n, 1)];
    let mut l = Likelihood::new(cfg, &obs, None, None);
    let before = l.score(&[0]);
    l.fit_workloads();
    assert_eq!(
        l.cfg.core[0].workload.contains_key(&id),
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

#[test]
fn the_coupled_shift_moves_workload_limits_and_leaves_unsupported_ones() {
    let id = pick_workload(Regime::R1, 0).id();
    let mut cfg = initial_config(&[&seed_fact()]);
    cfg.model.near_limit_rate = 0.0;
    cfg.model.past_limit_rate = 0.01;
    let unsupported = pick_workload(Regime::R1, 1).id();
    cfg.core[0].workload = [(id, -20), (unsupported, -50)].into();
    let obs = [
        observation(&[-21, 0], Regime::R1, &[0], 60, 100, 0),
        observation(&[-24, 0], Regime::R1, &[0], 60, 100, 99),
    ];
    let mut l = Likelihood::new(cfg, &obs, None, None);
    let before = l.score(&[0, 1]);
    let deep = probability(l.model.as_ref().unwrap(), &[-24, 0], Regime::R1, &[0], 60);
    l.fit_limit_rate_shift(&[0, 1]);
    assert!(
        l.score(&[0, 1]) < before && l.cfg.core[0].workload[&id] < -20,
        "{:?}",
        l.cfg.core[0].workload
    );
    let after = probability(l.model.as_ref().unwrap(), &[-24, 0], Regime::R1, &[0], 60);
    assert!(
        (after - deep).abs() < 1e-12,
        "the past-limit hazard changed: {deep} -> {after}"
    );
    assert_eq!(l.cfg.core[0].workload[&unsupported], -50);
    assert_eq!(l.cfg.core[0].alone[0], -50);
}

#[test]
fn an_initial_machine_seeds_no_joint_whatever_failed_below_zero() {
    let mut records = vec![fact(&[-30; 8], Regime::R7, &[0, 1], 60, crash())];
    records.push(fact(&[-25; 8], Regime::R7, &[0, 1], 60, crash()));
    let cfg = initial_config(&refs(&records));
    assert!(cfg.joint.is_empty() && cfg.ccd.is_none() && cfg.shared_voltage.is_none());
    assert_eq!(cfg.core.len(), 16);
    assert_eq!(cfg.core[3], core_def(3));
}

#[test]
fn a_fit_recovers_a_flat_rate_and_an_idle_limit() {
    let mut known = blank_machine(16);
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
    let (got, _) = fit(&refs(&draw.records)).unwrap();
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

fn idle_machine() -> MachineDefinition {
    let mut cfg = blank_machine(16);
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

#[test_case::test_case((None, None); "None None")]
#[test_case::test_case((Some(0), Some(0)); "Some 0 Some 0")]
#[test_case::test_case((Some(-50), None); "Some 50 None")]
fn an_idle_limit_needs_a_negative_offset_on_an_unloaded_core_to_be_fitted(
    case: (Option<i64>, Option<i64>),
) {
    let (initial, want) = case;
    let obs = [
        observation(&[-1, 0], Regime::R1, &[0], 60, 1, 1),
        observation(&[0, -1], Regime::R1, &[1], 60, 100, 0),
    ];

    let mut cfg = idle_machine();
    cfg.core[1].idle = initial;
    let mut l = Likelihood::new(cfg, &obs, None, None);
    l.fit_idle();
    assert_eq!(l.cfg.core[1].idle, want, "initial {initial:?}");
}
