use togi_facts::Decisive;
use togi_machine::{CcdId, CoreId, Regime, pick_workload, workloads};
use togi_sim::{Load, Model};
use togi_tool_modelcheck::Checker;
use togi_tool_trialfacts::TrialFact;

use super::{crash, fact, failure, repeated};
use crate::evidence::{aggregate, initial_config, load_of};
use crate::likelihood::Likelihood;
use crate::voltage::{fit_shared_voltage, named_failures, sync_ycruncher, voltage_config};

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
