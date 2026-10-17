use togi_facts::Decisive;
use togi_machine::{CcdId, CoreId, Regime, pick_workload, workloads};
use togi_sim::{Load, MachineDefinition, Model};
use togi_tool_modelcheck::Checker;
use togi_tool_trialfacts::TrialFact;

use super::{crash, fact, repeated};
use crate::evidence::{aggregate, bootstrap, initial_config, load_of};
use crate::likelihood::Likelihood;
use crate::search::{fit, fit_from};
use crate::voltage::{named_failures, sync_ycruncher, voltage_config};

fn loaded_16() -> Vec<i64> {
    (0..16).collect()
}

fn avx2() -> &'static str {
    pick_workload(Regime::R7, 0).id
}

fn voltage_records() -> Vec<TrialFact> {
    (0..40_i64)
        .map(|i| {
            let mhz = u32::try_from(5200 + (i % 4) * 20).unwrap();
            let mut offsets = [-35_i64; 16];
            let mut record = fact(
                &offsets,
                Regime::R7,
                &[0, 1, 2, 3, 4, 5, 6, 7],
                120,
                Decisive::Pass,
            );
            for core in 0..8_u8 {
                offsets[usize::from(core)] = -20 - i / 4;
                let request = 1.22
                    + f64::from(core) * 0.005
                    + 0.0038 * f64::from(i32::try_from(offsets[usize::from(core)]).unwrap())
                    + 0.03 * f64::from(i32::try_from(mhz).unwrap() - 5240) / 100.0;
                record
                    .voltage_requests_v
                    .set(CoreId::try_from(core).unwrap(), request);
            }
            record.profile = super::profile(&offsets);
            record.ccd_mhz.insert(CcdId::new(0).unwrap(), mhz);
            record
        })
        .collect()
}

#[test]
fn the_request_regression_recovers_the_count_and_clock_slopes_and_the_clock_medians() {
    let records = voltage_records();
    let v = voltage_config(&records);
    let w = &v.workload[avx2()];
    for (core, c) in w.core.iter().enumerate().take(8) {
        assert!((c.count_v - 0.0038).abs() < 0.000_01, "core {core}: {c:?}");
        assert!(
            (c.clock_v_per_100mhz - 0.03).abs() < 0.0001,
            "core {core}: {c:?}"
        );
        assert!(
            (c.threshold_clock_v_per_100mhz - c.clock_v_per_100mhz).abs() < f64::EPSILON,
            "the required-voltage clock slope must equal the measured request slope"
        );
    }
    let second = &v.workload[pick_workload(Regime::R7, 1).id];
    assert_eq!(
        w.core, second.core,
        "an unmeasured workload must inherit the request curve"
    );
    assert_eq!(
        w.full_mhz,
        [5230.0, 5240.0],
        "clock medians, and the unsupported prior of the unmeasured CCD"
    );
    assert_eq!(
        v,
        voltage_config(&records),
        "the request fit is not deterministic"
    );
}

#[test]
fn a_named_core_failure_pins_the_hazard_to_the_named_core() {
    let records = voltage_records();
    let mut cfg = initial_config(&records);
    let mut shared = voltage_config(&records);
    {
        let w = shared.workload.get_mut(avx2()).unwrap();
        for core in &mut w.core {
            core.threshold_v = 1.0;
        }
        w.core[0].threshold_v = 1.2;
    }
    cfg.shared_voltage = Some(shared);
    let mut failing = records[0].clone();
    failing.outcome = Decisive::Failure {
        signal: togi_machine::Signal::ComputationError,
        core: Some(CoreId::new(0).unwrap()),
        stalled: None,
    };
    let score = |failing: &TrialFact| {
        let named = named_failures(std::slice::from_ref(failing));
        let obs = aggregate(std::slice::from_ref(failing));
        Likelihood::new(cfg.clone(), &obs, None, Some(&named)).score(&[0])
    };
    let first = score(&failing);
    let mut other = failing.clone();
    other.outcome = Decisive::Failure {
        signal: togi_machine::Signal::ComputationError,
        core: Some(CoreId::new(1).unwrap()),
        stalled: None,
    };
    let second = score(&other);
    assert!(
        second > first + 5.0,
        "named-core attribution did not constrain the named hazard: core0={first} core1={second}"
    );
    let mut stalled = failing.clone();
    stalled.outcome = Decisive::Failure {
        signal: togi_machine::Signal::Stall,
        core: None,
        stalled: Some(CoreId::new(0).unwrap()),
    };
    assert_eq!(named_failures(&[stalled.clone()])[0].core.get(), 0);
    stalled.outcome = Decisive::Failure {
        signal: togi_machine::Signal::Stall,
        core: None,
        stalled: Some(CoreId::new(15).unwrap()),
    };
    assert!(
        named_failures(&[stalled]).is_empty(),
        "an unloaded core must not become a loaded-core failure attribution"
    );
}

#[test]
fn the_shared_voltage_fit_is_deterministic_bounded_and_leaves_its_input_alone() {
    let records = voltage_records();
    let before = voltage_config(&records);
    let (cfg, loss) = fit(&records).unwrap();
    let (again, again_loss) = fit(&records).unwrap();
    assert_eq!(cfg, again);
    assert_eq!(loss.to_bits(), again_loss.to_bits());
    assert!(cfg.shared_voltage.is_some() && cfg.ccd.is_none() && cfg.joint.is_empty());
    assert_eq!(
        before,
        voltage_config(&records),
        "fitting mutated the input telemetry"
    );
    let obs = aggregate(&records);
    let l = Likelihood::new(cfg.clone(), &obs, None, None);
    assert_eq!(
        l.raw_score(&l.all()).to_bits(),
        loss.to_bits(),
        "the reported likelihood must exclude priors and named-core terms"
    );
    for workload in workloads(Regime::R7) {
        for core in &cfg.shared_voltage.as_ref().unwrap().workload[workload.id].core {
            assert!(
                (0.8..=1.5).contains(&core.threshold_v)
                    && (0.9..=1.5).contains(&core.base_v)
                    && (0.0025..=0.005).contains(&core.count_v)
                    && (core.threshold_clock_v_per_100mhz - core.clock_v_per_100mhz).abs()
                        < f64::EPSILON,
                "fit outside physical bounds: {core:?}"
            );
        }
    }
}

#[test]
fn y_cruncher_takes_each_cores_higher_threshold_of_the_two_avx_workloads_and_nothing_else() {
    let mut v = voltage_config(&voltage_records());
    let ids: Vec<&str> = workloads(Regime::R7).iter().map(|w| w.id).collect();
    let (a, b, y) = (ids[0], ids[1], ids[2]);
    let before = v.workload[y].clone();
    for core in 0..16_u8 {
        let index = usize::from(core);
        v.workload.get_mut(a).unwrap().core[index].threshold_v = 1.0 + 0.01 * f64::from(core);
        v.workload.get_mut(b).unwrap().core[index].threshold_v = 1.08;
    }
    sync_ycruncher(&mut v);
    let after = &v.workload[y];
    for (core, synced) in after.core.iter().enumerate() {
        let want = v.workload[a].core[core]
            .threshold_v
            .max(v.workload[b].core[core].threshold_v);
        assert_eq!(synced.threshold_v.to_bits(), want.to_bits(), "core {core}");
    }
    let mut restored = after.clone();
    for (core, original) in restored.core.iter_mut().zip(&before.core) {
        core.threshold_v = original.threshold_v;
    }
    assert_eq!(restored, before, "syncing changed more than the thresholds");
}

#[test]
fn the_ccd_threshold_shift_moves_every_core_from_its_original_snapshot_inside_the_physical_range() {
    let records = voltage_records();
    let mut cfg = shared_machine(&records, 1.15);
    let v = cfg.shared_voltage.as_mut().unwrap();
    let spread = [0.85, 0.95, 1.05, 1.15, 1.25, 1.35, 1.45, 1.10];
    for (core, threshold) in v
        .workload
        .get_mut(avx2())
        .unwrap()
        .core
        .iter_mut()
        .zip(spread)
    {
        core.threshold_v = threshold;
    }
    sync_ycruncher(v);
    let obs = aggregate(&records);
    let named = named_failures(&records);
    let mut l = Likelihood::new(cfg, &obs, None, Some(&named));
    l.fit_voltage();
    let fitted = l.cfg.shared_voltage.as_ref().unwrap();
    for workload in workloads(Regime::R7) {
        for core in &fitted.workload[workload.id].core {
            assert!(
                (0.8..=1.5).contains(&core.threshold_v),
                "a threshold left the physical range: {core:?}"
            );
        }
    }
}

#[test]
fn a_failure_at_stock_offsets_is_explained_by_a_bounded_background_rate() {
    let mut records = Vec::new();
    for i in 0..149 {
        let offset = if i >= 29 { -30 } else { 0 };
        let outcome = if i == 0 { crash() } else { Decisive::Pass };
        let mut record = fact(&[offset; 16], Regime::R7, &loaded_16(), 600, outcome);
        record.class = togi_tool_trialfacts::Class::Trial(togi_facts::TrialClass {
            workload: togi_machine::WorkloadId::new(Regime::R7, 1).unwrap(),
            loaded: super::cores(&loaded_16()),
            duration_s: 600,
        });
        records.push(record);
    }
    let (cfg, _) = fit(&records).unwrap();
    let background = cfg.shared_voltage.as_ref().unwrap().background_rate;
    assert!(background > 0.0 && background <= 0.001, "{background}");
    let model = Model::new(&cfg, 0).unwrap();
    let togi_tool_trialfacts::Class::Trial(class) = records[0].class else {
        unreachable!("the fixture is a trial");
    };
    let load: Load<'_> = load_of(&class);
    let stock = model.hazard(records[0].profile.as_slice(), &load).unwrap();
    assert!(
        stock >= background,
        "stock hazard {stock} omitted fitted background {background}"
    );
}

fn shared_machine(records: &[TrialFact], threshold: f64) -> togi_sim::MachineDefinition {
    let mut cfg = initial_config(records);
    let mut shared = voltage_config(records);
    for core in &mut shared.workload.get_mut(avx2()).unwrap().core {
        core.threshold_v = threshold;
    }
    sync_ycruncher(&mut shared);
    cfg.shared_voltage = Some(shared);
    cfg
}

fn telemetry(loaded: &[i64], offsets: &[i64; 16], mhz: &[(u8, u32)]) -> TrialFact {
    let mut record = fact(offsets, Regime::R7, loaded, 120, Decisive::Pass);
    for &(ccd, clock) in mhz {
        record.ccd_mhz.insert(CcdId::try_from(ccd).unwrap(), clock);
    }
    record
}

#[test]
fn clocks_are_fitted_from_full_and_partial_ccd_loads_and_from_all_core_pressure() {
    let ccd0: Vec<i64> = (0..8).collect();
    let ccd1: Vec<i64> = (8..16).collect();
    let seven: Vec<i64> = (8..15).collect();
    let mut records = vec![
        telemetry(&ccd0, &[-30; 16], &[(0, 5250)]),
        telemetry(&ccd1, &[-30; 16], &[(1, 5100)]),
        telemetry(&ccd1, &[-30; 16], &[(1, 5110)]),
        telemetry(&ccd1, &[-30; 16], &[(1, 5120)]),
        telemetry(&seven, &[-30; 16], &[(1, 5130)]),
        // A loaded core at stock offset is power-limited and must not lower the full-CCD clock.
        telemetry(&ccd1, &[0; 16], &[(1, 4500)]),
    ];
    let mut lopsided = [-35_i64; 16];
    for offset in &mut lopsided[8..] {
        *offset = -20;
    }
    records.push(telemetry(&loaded_16(), &[-35; 16], &[(0, 5200), (1, 5000)]));
    records.push(telemetry(&loaded_16(), &lopsided, &[(0, 5200), (1, 5000)]));
    records.push(fact(&[-10; 16], Regime::R1, &[0], 60, Decisive::Pass));
    let v = voltage_config(&records);
    let w = &v.workload[avx2()];
    assert_eq!(w.full_mhz, [5250.0, 5110.0]);
    assert!((w.idle_gain_mhz - 20.0).abs() < 1e-9, "{}", w.idle_gain_mhz);
    // Both CCDs at 104 W: 208 W against the 192 W limit is 16 W of pressure; the lopsided trial has 30.4 W.
    let first: f64 = (5250.0 + 5110.0 - (5200.0 + 5000.0)) / (2.0 * 16.0);
    let second: f64 = (5250.0 + 5110.0 - (5200.0 + 5000.0)) / (2.0 * (104.0 + 118.4 - 192.0));
    let package = f64::midpoint(first, second);
    // The lopsided trial: 104 W on CCD0 and 8 * (13 + 0.12 * 15) = 118.4 W on CCD1.
    let balance: f64 = (5250.0 - 5110.0 - (5200.0 - 5000.0)) / (104.0 - 118.4);
    assert!(
        (w.package_mhz_per_w - package).abs() < 1e-9,
        "{} vs {package}",
        w.package_mhz_per_w
    );
    assert!(
        (w.balance_mhz_per_w - balance.clamp(0.0, 30.0)).abs() < 1e-9,
        "{} vs {balance}",
        w.balance_mhz_per_w
    );
}

#[test]
fn single_core_evidence_still_fits_its_limits_beside_the_shared_voltage_model() {
    let mut records = voltage_records();
    let at = |depth: i64, failing: usize| {
        let mut offsets = [0_i64; 16];
        offsets[0] = depth;
        let template = fact(&offsets, Regime::R1, &[0], 60, Decisive::Pass);
        repeated(&template, 40, failing)
    };
    records.extend(at(-10, 0));
    records.extend(at(-30, 40));
    let (cfg, _) = fit(&records).unwrap();
    assert!(
        (-30..0).contains(&cfg.core[0].alone[0]),
        "{}",
        cfg.core[0].alone[0]
    );
}

#[test]
fn constrained_voltage_candidates_stay_inside_the_original_groups_and_an_unconstrained_fit_does_not()
 {
    let template = fact(
        &[-25; 16],
        Regime::R7,
        &[0, 1, 2, 3, 4, 5, 6, 7],
        120,
        Decisive::Pass,
    );
    let togi_tool_trialfacts::Class::Trial(class) = template.class else {
        unreachable!("the fixture is a trial");
    };
    let load: Load<'_> = load_of(&class);
    let mut base = shared_machine(&repeated(&template, 1, 0), 1.135);
    let p0 = Model::new(&base, 0)
        .unwrap()
        .failure_probability(template.profile.as_slice(), &load)
        .unwrap();
    assert!(
        p0 > 0.0 && p0 < 1.0,
        "the fixture's starting prediction {p0}"
    );
    // The hazard is linear in the rate, so this scales the group's failure probability to one half.
    base.shared_voltage.as_mut().unwrap().rate *= std::f64::consts::LN_2 / -(-p0).ln_1p();
    let base_model = Model::new(&base, 0).unwrap();
    let half = base_model
        .failure_probability(template.profile.as_slice(), &load)
        .unwrap();
    assert!((half - 0.5).abs() < 0.05, "{half}");
    let original = repeated(&template, 40, 20);
    let sample = repeated(&template, 40, 40);
    let checker = Checker::new(&base_model, &original).unwrap();
    assert!(checker.accepts(&base_model).unwrap());
    let (guarded, _) = fit_from(&sample, Some(&base), Some(&checker)).unwrap();
    let guarded_model = Model::new(&guarded, 0).unwrap();
    assert!(
        checker.accepts(&guarded_model).unwrap(),
        "a voltage candidate outside the original group's interval was kept"
    );
    assert!(
        guarded_model
            .failure_probability(template.profile.as_slice(), &load)
            .unwrap()
            > half,
        "the all-failing resample did not move the voltage model"
    );
    let (free, _) = fit_from(&sample, Some(&base), None).unwrap();
    assert!(
        !checker.accepts(&Model::new(&free, 0).unwrap()).unwrap(),
        "the unconstrained fit stays inside the interval, so the guard binds nothing"
    );
}

fn set_background(def: &mut MachineDefinition, rate: f64) {
    def.shared_voltage.as_mut().unwrap().background_rate = rate;
}

fn raise_core_threshold(def: &mut MachineDefinition, volts: f64) {
    let v = def.shared_voltage.as_mut().unwrap();
    v.workload.get_mut(avx2()).unwrap().core[0].threshold_v += volts;
    sync_ycruncher(v);
}

#[test]
fn every_candidate_of_every_step_is_scored_on_the_augmented_objective() {
    let template = fact(
        &[-30; 16],
        Regime::R7,
        &[0, 1, 2, 3, 4, 5, 6, 7],
        120,
        Decisive::Pass,
    );
    let mut records = repeated(&template, 20, 8);
    for record in &mut records[..8] {
        record.outcome = Decisive::Failure {
            signal: togi_machine::Signal::ComputationError,
            core: Some(CoreId::new(0).unwrap()),
            stalled: None,
        };
    }
    let cfg = shared_machine(&records, 1.135);
    let obs = aggregate(&records);
    let named = named_failures(&records);
    assert_eq!(named.len(), 8);
    let with = |change: &dyn Fn(&mut MachineDefinition)| {
        let mut def: MachineDefinition = cfg.clone();
        change(&mut def);
        Likelihood::new(def, &obs, None, Some(&named))
    };
    let l = with(&|_| {});
    let all = l.all();

    let backgrounds = [0.0, 5e-4];
    let scanned = l.scan(&backgrounds, |def, &rate| set_background(def, rate), &all);
    for (candidate, rate) in scanned.iter().zip(&backgrounds) {
        let own = with(&|def| set_background(def, *rate));
        assert_eq!(
            candidate.loss.to_bits(),
            own.score(&all).to_bits(),
            "a scanned candidate was not scored on the augmented objective of its own machine"
        );
    }
    let objective_step = scanned[1].loss - scanned[0].loss;
    let raw_step = with(&|def| set_background(def, backgrounds[1])).raw_score(&all)
        - with(&|def| set_background(def, backgrounds[0])).raw_score(&all);
    assert!(
        objective_step - raw_step > 1e-6,
        "the background rate, which no prior touches, did not move the named-core term: {objective_step} vs {raw_step}"
    );

    let shifts = [0.0, 0.01];
    let scanned = l.scan(
        &shifts,
        |def, &shift| raise_core_threshold(def, shift),
        &all,
    );
    for (candidate, shift) in scanned.iter().zip(&shifts) {
        let own = with(&|def| raise_core_threshold(def, *shift));
        assert_eq!(
            candidate.loss.to_bits(),
            own.score(&all).to_bits(),
            "a scanned threshold candidate was not scored on its own augmented objective"
        );
    }
    let objective_step = scanned[1].loss - scanned[0].loss;
    let raw_step = with(&|def| raise_core_threshold(def, shifts[1])).raw_score(&all)
        - with(&|def| raise_core_threshold(def, shifts[0])).raw_score(&all);
    assert!(
        (objective_step - raw_step).abs() > 1e-6,
        "a loaded threshold did not move the prior and named-core terms: {objective_step} vs {raw_step}"
    );
}

#[test]
fn the_default_fit_is_one_shared_voltage_model_without_joints_or_residual() {
    let mut records = voltage_records();
    let mut offsets = [0_i64; 16];
    offsets[0] = -30;
    offsets[9] = -30;
    let template = fact(&offsets, Regime::R1, &[0], 60, Decisive::Pass);
    records.extend(repeated(&template, 40, 20));
    let (cfg, loss) = fit(&records).unwrap();
    assert!(cfg.joint.is_empty() && cfg.ccd.is_none());
    let voltage = cfg.shared_voltage.as_ref().unwrap();
    let ids: Vec<&str> = workloads(Regime::R7).iter().map(|w| w.id).collect();
    for core in 0..16 {
        let threshold = |id: &str| voltage.workload[id].core[core].threshold_v;
        assert!(
            (threshold(ids[2]) - threshold(ids[0]).max(threshold(ids[1]))).abs() < f64::EPSILON,
            "core {core}: y-cruncher's threshold is not the higher of the mprime thresholds"
        );
    }
    let obs = aggregate(&records);
    let named = named_failures(&records);
    let l = Likelihood::new(cfg, &obs, None, Some(&named));
    let all = l.all();
    assert_eq!(l.raw_score(&all).to_bits(), loss.to_bits());
    assert!(
        l.score(&all) > loss,
        "the reported loss holds the prior or the named-core terms"
    );
}

#[test]
fn a_bootstrap_draw_is_fitted_from_the_telemetry_its_trials_carry() {
    let records = voltage_records();
    let sample = bootstrap(&records, 263);
    assert!(
        sample.iter().all(|fact| records.contains(fact)),
        "a draw lost its trials' telemetry"
    );
    let (cfg, _) = fit(&sample).unwrap();
    assert_eq!(
        cfg.shared_voltage.as_ref().unwrap().workload[avx2()].full_mhz,
        voltage_config(&sample).workload[avx2()].full_mhz,
        "the draw's fit does not start from its own clocks"
    );
}

#[test]
fn a_singleton_r7_load_keeps_its_per_core_limit_beside_the_rail() {
    let mut records = voltage_records();
    let at = |depth: i64, failing: usize| {
        let mut offsets = [0_i64; 16];
        offsets[0] = depth;
        let template = fact(&offsets, Regime::R7, &[0], 60, Decisive::Pass);
        repeated(&template, 40, failing)
    };
    records.extend(at(-10, 0));
    records.extend(at(-30, 40));
    let (cfg, _) = fit(&records).unwrap();
    let limit = cfg.core[0].together[6];
    assert!((-29..=-10).contains(&limit), "{limit}");
    let model = Model::new(&cfg, 0).unwrap();
    let singleton = |depth: i64| {
        let mut offsets = [0_i64; 16];
        offsets[0] = depth;
        let template = fact(&offsets, Regime::R7, &[0], 60, Decisive::Pass);
        let togi_tool_trialfacts::Class::Trial(class) = template.class else {
            unreachable!("the fixture is a trial");
        };
        model
            .failure_probability(template.profile.as_slice(), &load_of(&class))
            .unwrap()
    };
    assert!(singleton(-30) > 0.5 && singleton(-10) < 0.5);
}

#[test]
fn flat_and_idle_fits_never_use_a_multi_core_r7_load() {
    let template = fact(
        &[-30; 16],
        Regime::R7,
        &[0, 1, 2, 3, 4, 5, 6, 7],
        120,
        Decisive::Pass,
    );
    let records = repeated(&template, 20, 8);
    let cfg = shared_machine(&records, 1.135);
    let obs = aggregate(&records);
    let named = named_failures(&records);
    let mut l = Likelihood::new(cfg.clone(), &obs, None, Some(&named));
    l.fit_flat();
    l.fit_idle();
    assert!(
        l.cfg
            .core
            .iter()
            .all(|core| core.flat == 0.0 && core.idle.is_none()),
        "a multi-core R7 failure bought an unloaded-core flat rate or idle limit"
    );
    assert_eq!(l.cfg, cfg);
}
