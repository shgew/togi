use togi_facts::Decisive;
use togi_machine::{CcdId, CoreId, Regime, pick_workload, workloads};
use togi_sim::{Load, MachineDefinition, Model};
use togi_tool_modelcheck::Checker;
use togi_tool_trialfacts::TrialFact;

use super::{crash, fact, failure, repeated};
use crate::evidence::{aggregate, initial_config, load_of};
use crate::likelihood::Likelihood;
use crate::voltage::{
    NamedFailure, fit_shared_voltage, named_failures, sync_ycruncher, voltage_config,
};

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
            record.ccd_mhz.insert(CcdId(0), mhz);
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
    cfg.joint.clear();
    cfg.ccd = None;
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
    let (cfg, loss) = fit_shared_voltage(&records).unwrap();
    let (again, again_loss) = fit_shared_voltage(&records).unwrap();
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
    let (cfg, _) = fit_shared_voltage(&records).unwrap();
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
    cfg.joint.clear();
    cfg.ccd = None;
    let mut shared = voltage_config(records);
    for core in &mut shared.workload.get_mut(avx2()).unwrap().core {
        core.threshold_v = threshold;
    }
    sync_ycruncher(&mut shared);
    cfg.shared_voltage = Some(shared);
    cfg
}

#[test]
fn the_constrained_rate_search_keeps_the_voltage_geometry_and_refuses_incompatible_groups() {
    let template = fact(
        &[-25; 16],
        Regime::R7,
        &[0, 1, 2, 3, 4, 5, 6, 7],
        120,
        Decisive::Pass,
    );
    let mut records = repeated(&template, 20, 5);
    let cfg = shared_machine(&records, 1.135);
    let mut expected = cfg.clone();
    let obs = aggregate(&records);
    let named = [];
    let mut l = Likelihood::new(cfg.clone(), &obs, None, Some(&named));
    let checker = Checker::new(&Model::new(&cfg, 0).unwrap(), &records).unwrap();
    assert!(
        !checker.accepts(l.model.as_ref().unwrap()).unwrap(),
        "the fixture must initially violate the unchanged group interval"
    );
    assert!(l.constrain_rate(&records).unwrap());
    assert!(
        checker.accepts(l.model.as_ref().unwrap()).unwrap(),
        "a bounded per-time rate must explain the group's failures"
    );
    let rate = l.cfg.shared_voltage.as_ref().unwrap().rate;
    assert!((1e-5..=0.03).contains(&rate), "{rate}");
    expected.shared_voltage.as_mut().unwrap().rate = rate;
    assert_eq!(
        expected, l.cfg,
        "the rate repair changed the voltage geometry or the background"
    );

    for record in &mut records {
        record.outcome = Decisive::Pass;
    }
    let mut failing = records[..20].to_vec();
    for record in &mut failing {
        record.outcome = failure(togi_machine::Signal::Crash);
        if let togi_tool_trialfacts::Class::Trial(class) = &mut record.class {
            class.duration_s = 300;
        }
    }
    records.extend(failing);
    let cfg = shared_machine(&records, 1.155);
    let obs = aggregate(&records);
    let mut l = Likelihood::new(cfg.clone(), &obs, None, Some(&named));
    assert!(
        !l.constrain_rate(&records).unwrap(),
        "incompatible duration groups must not create a passing anchor"
    );
    assert_eq!(
        cfg, l.cfg,
        "a failed constraint search must restore the best fit"
    );
}

fn telemetry(loaded: &[i64], offsets: &[i64; 16], mhz: &[(u8, u32)]) -> TrialFact {
    let mut record = fact(offsets, Regime::R7, loaded, 120, Decisive::Pass);
    for &(ccd, clock) in mhz {
        record.ccd_mhz.insert(CcdId(ccd), clock);
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
    let (cfg, _) = fit_shared_voltage(&records).unwrap();
    assert!(
        (-30..0).contains(&cfg.core[0].alone[0]),
        "{}",
        cfg.core[0].alone[0]
    );
}

#[test]
fn a_rate_that_is_too_low_for_a_group_is_raised_into_its_interval() {
    let template = fact(
        &[-25; 16],
        Regime::R7,
        &[0, 1, 2, 3, 4, 5, 6, 7],
        120,
        Decisive::Pass,
    );
    let records = repeated(&template, 20, 8);
    let cfg = shared_machine(&records, 1.135);
    let obs = aggregate(&records);
    let named = [];
    let mut l = Likelihood::new(cfg.clone(), &obs, None, Some(&named));
    let checker = Checker::new(&Model::new(&cfg, 0).unwrap(), &records).unwrap();
    assert!(!checker.accepts(l.model.as_ref().unwrap()).unwrap());
    assert!(l.constrain_rate(&records).unwrap());
    assert!(checker.accepts(l.model.as_ref().unwrap()).unwrap());
    assert!(
        l.cfg.shared_voltage.as_ref().unwrap().rate > cfg.shared_voltage.as_ref().unwrap().rate
    );
}

#[test]
fn a_rate_bisection_midpoint_below_the_feasible_interval_is_raised_into_it() {
    let template = fact(
        &[-25; 16],
        Regime::R7,
        &[0, 1, 2, 3, 4, 5, 6, 7],
        120,
        Decisive::Pass,
    );
    let records = repeated(&template, 20, 8);
    let cfg = shared_machine(&records, 1.135);
    let obs = aggregate(&records);
    let named = [];
    let checker = Checker::new(&Model::new(&cfg, 0).unwrap(), &records).unwrap();
    let accepts = |rate: f64| {
        let mut def = cfg.clone();
        def.shared_voltage.as_mut().unwrap().rate = rate;
        checker.accepts(&Model::new(&def, 0).unwrap()).unwrap()
    };
    let mut repaired = Likelihood::new(cfg.clone(), &obs, None, Some(&named));
    assert!(repaired.constrain_rate(&records).unwrap());
    let (mut infeasible, mut lowest) = (
        cfg.shared_voltage.as_ref().unwrap().rate,
        repaired.cfg.shared_voltage.as_ref().unwrap().rate,
    );
    assert!(!accepts(infeasible) && accepts(lowest));
    for _ in 0..60 {
        let mid = f64::midpoint(infeasible, lowest);
        if accepts(mid) {
            lowest = mid;
        } else {
            infeasible = mid;
        }
    }
    let ceiling = 1.5 * lowest;
    assert!(
        !accepts(f64::midpoint(1e-5, ceiling)),
        "the first midpoint must lie below the feasible interval"
    );
    let mut l = Likelihood::new(cfg.clone(), &obs, None, Some(&named));
    assert!(
        l.constrain_time_rate(&records, |v| &mut v.rate, 1e-5, ceiling)
            .unwrap(),
        "the search must raise a too-low midpoint until a group interval admits it"
    );
    let rate = l.cfg.shared_voltage.as_ref().unwrap().rate;
    assert!(
        (lowest..=ceiling).contains(&rate),
        "{lowest} <= {rate} <= {ceiling}"
    );
    assert!(checker.accepts(l.model.as_ref().unwrap()).unwrap());
}

#[test]
fn the_shared_steps_minimize_the_objective_the_anchor_sweep_converges_on() {
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
    let measured = |def: &MachineDefinition| {
        let l = Likelihood::new(def.clone(), &obs, None, Some(&named));
        l.score(&l.all())
    };
    let flat_step = |named: Option<&[NamedFailure]>| {
        let mut l = Likelihood::new(cfg.clone(), &obs, None, named);
        l.fit_flat();
        l.cfg
    };
    let merged = flat_step(Some(&named));
    let raw_only = flat_step(None);
    let unloaded_flat = |def: &MachineDefinition| def.core[8].flat;
    let (start, after_merged, after_raw) = (measured(&cfg), measured(&merged), measured(&raw_only));
    assert!(
        after_merged <= start,
        "a shared step raised the measured objective: {start} -> {after_merged}"
    );
    assert!(
        unloaded_flat(&raw_only) > unloaded_flat(&merged),
        "the raw likelihood alone must buy the failures with unloaded-core hazard the named cores refute"
    );
    assert!(
        after_raw > start && after_raw > after_merged,
        "the raw-only step must worsen the measured objective: {start} -> {after_raw} vs {after_merged}"
    );
}
