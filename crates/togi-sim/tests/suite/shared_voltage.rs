// The shared-voltage rail: clocks, requests, hazards, failure share and validation.

use std::time::Duration;
use togi_machine::{CoreId, Regime, Signal, TargetOffset};
use togi_sim::definition::{
    CcdDef, DEFAULT_COUNT_V, JointDef, SharedVoltageDef, VoltageCoreDef, VoltageWorkloadDef,
    Weights,
};
use togi_sim::file;
use togi_sim::voltage::core_watts;
use togi_sim::{MachineDefinition, Model, Problem, SourceKind};

use test_case::test_case;

use crate::common::{
    Rig, committed_machine, flat_cores, invalid, load, machine_dir_text, model, offsets,
    r7_workload, spec,
};

const SECONDS: f64 = 120.0;

type RailEdit = fn(&mut SharedVoltageDef);
type WorkloadEdit = fn(&mut VoltageWorkloadDef);

#[expect(
    clippy::cast_possible_truncation,
    reason = "requests are recorded as f32"
)]
fn narrowed(volts: f64) -> f32 {
    volts as f32
}

fn voltage_model() -> Model {
    model(&committed_machine("shared-voltage.json"))
}

fn r7() -> Regime {
    Regime::R7
}

fn core(id: i64) -> CoreId {
    CoreId::new(id).expect("a core")
}

fn close(got: f64, want: f64, tolerance: f64) {
    assert!((got - want).abs() <= tolerance, "{got} vs {want}");
}

#[test]
fn a_core_draws_its_base_watts_at_minus_35_more_when_shallower_and_never_negative() {
    let at = |offset| TargetOffset::new(offset).expect("an offset");
    close(core_watts(13.0, 0.12, at(-35)), 13.0, 0.0);
    close(core_watts(13.0, 0.12, at(0)), 13.0 + 0.12 * 35.0, 0.0);
    close(core_watts(13.0, 0.12, at(-50)), 13.0 - 0.12 * 15.0, 0.0);
    close(core_watts(1.0, 0.5, at(-50)), 0.0, 0.0);
}

#[test]
fn the_rail_follows_the_top_loaded_requester_and_idle_cores_never_set_it() {
    let model = voltage_model();
    let idle_v = model.voltage().expect("a rail").idle_v;
    let regime = r7();
    let workload = r7_workload(0);
    let mut profile = [-35; 16];
    profile[6] = -40;
    let full = model
        .voltage_state(
            &offsets(&profile),
            &load(regime, workload, &[0, 1, 2, 3, 4, 5, 6, 7], SECONDS),
        )
        .expect("shape")
        .expect("a rail");
    close(full.rail, f64::from(full.requests[7]), 1e-7);

    let partial_load = load(regime, workload, &[0, 1, 2, 3, 4, 5, 6], SECONDS);
    let partial = model
        .voltage_state(&offsets(&profile), &partial_load)
        .expect("shape")
        .expect("a rail");
    assert!(partial.rail < full.rail);
    assert_eq!(partial.requests[7], narrowed(idle_v));

    profile[7] = 0;
    let unloaded_at_zero = model
        .voltage_state(&offsets(&profile), &partial_load)
        .expect("shape")
        .expect("a rail");
    assert_eq!(partial, unloaded_at_zero);

    let across = model
        .voltage_state(
            &offsets(&profile),
            &load(regime, workload, &[0, 1, 2, 3, 4, 5, 6, 10], SECONDS),
        )
        .expect("shape")
        .expect("a rail");
    assert!(across.rail > partial.rail);
    close(across.rail, f64::from(across.requests[10]), 1e-7);
}

#[test]
fn clocks_follow_package_power_idle_gain_and_cross_ccd_balance() {
    let definition = committed_machine("shared-voltage.json");
    let model = model(&definition);
    let regime = r7();
    let clocks = |profile: &[i64; 16], workload: &str, cores: &[i64], model: &Model| {
        model
            .voltage_state(&offsets(profile), &load(regime, workload, cores, SECONDS))
            .expect("shape")
            .expect("a rail")
            .clocks
    };
    let mut profile = [-35; 16];
    let avx2 = r7_workload(0);
    let avx512 = r7_workload(1);
    assert_eq!(
        clocks(&profile, avx2, &[0, 1, 2, 3, 4, 5, 6, 7], &model),
        [5240, 0]
    );
    assert_eq!(
        clocks(&profile, avx2, &[0, 1, 2, 3, 4, 5, 6], &model),
        [5270, 0]
    );
    let all: Vec<i64> = (0..16).collect();
    assert_eq!(clocks(&profile, avx512, &all, &model), [4799, 4799]);
    for offset in &mut profile[..8] {
        *offset = 0;
    }
    assert_eq!(clocks(&profile, avx512, &all, &model), [4329, 4889]);

    let mut limited = definition;
    limited
        .shared_voltage
        .as_mut()
        .expect("a rail")
        .thermal_limit_w = 180.0;
    let limited = Model::new(&limited, 1).expect("valid");
    let [first, second] = clocks(&profile, avx512, &all, &limited);
    assert!(first < 4329 && second < 4889, "{first} {second}");
}

/// The rail and core 11's rate and threshold when `cores` run the second R7 workload under a deep profile.
fn core_eleven_rail(cores: &[i64]) -> (f64, f64, f64) {
    let model = voltage_model();
    let workload = r7_workload(1);
    let profile = offsets(&[
        -31, -38, -37, -34, -35, -36, -45, -42, -48, -47, -38, -44, -50, -42, -50, -50,
    ]);
    let state = model
        .voltage_state(&profile, &load(r7(), workload, cores, SECONDS))
        .expect("shape")
        .expect("a rail");
    let threshold = model.voltage().expect("a rail").workload(workload).cores[11].threshold_v;
    (state.rail, state.rates[11], threshold)
}

const UNSUPPORTED: [i64; 6] = [8, 9, 11, 12, 14, 15];

#[test_case(10; "core 10")]
#[test_case(13; "core 13")]
fn core_eleven_needs_a_supporting_core_to_keep_its_rail(supporter: i64) {
    let (unsupported, unsupported_rate, threshold) = core_eleven_rail(&UNSUPPORTED);
    let mut cores = UNSUPPORTED.to_vec();
    cores.push(supporter);
    let (supported, supported_rate, _) = core_eleven_rail(&cores);
    assert!(
        unsupported < threshold && supported > threshold,
        "{unsupported} and {supported} around {threshold}"
    );
    assert!(
        unsupported_rate > supported_rate * 10.0,
        "{unsupported_rate} against {supported_rate}"
    );
}

#[test]
fn core_eleven_keeps_its_rail_with_its_whole_ccd_and_fails_by_computation_error() {
    let (rail, _, threshold) = core_eleven_rail(&[8, 9, 10, 11, 12, 13, 14, 15]);
    assert!(rail > threshold, "{rail} <= {threshold}");
    let model = voltage_model();
    let mix = model
        .voltage()
        .expect("a rail")
        .workload(r7_workload(1))
        .cores[11]
        .signals
        .as_ref()
        .expect("core 11 names its signals");
    for x in [0.0, 0.25, 0.5, 0.999] {
        assert_eq!(mix.pick(x), Signal::ComputationError, "{x}");
    }
}

#[test]
fn a_rail_failure_of_core_eleven_takes_the_signal_of_its_own_mix() {
    let id = r7_workload(1);
    let mut definition = rewrite_cores(committed_machine("shared-voltage.json"), |core| {
        core.base_v = 1.2;
        core.threshold_v = 0.01;
        core.count_v = 0.0;
        core.clock_v_per_100mhz = 0.0;
        core.threshold_clock_v_per_100mhz = 0.0;
        core.signals = None;
    });
    // Core 11 alone requires more than the rail gives; the others are far above their requirement.
    let eleven = &mut definition
        .shared_voltage
        .as_mut()
        .expect("a rail")
        .workload
        .get_mut(id)
        .expect("workload")
        .core[11];
    eleven.threshold_v = 5.0;
    eleven.signals = Some(Weights::from([(Signal::ComputationError, 1.0)]));
    definition.model.signals = Weights::from([(Signal::Crash, 1.0)]);
    definition.model.regime_signals = [(Regime::R7, Weights::from([(Signal::Crash, 1.0)]))].into();
    let model = model(&definition);
    for seed in 0..5 {
        let mut rig = Rig::with(
            model.clone(),
            togi_sim::RunSettings {
                seed,
                ..togi_sim::RunSettings::default()
            },
        );
        let trial = spec(
            "0001",
            r7(),
            togi_machine::workload_by_id(id).expect("an R7 workload"),
            &[10, 11],
            Duration::from_secs(120),
        );
        let result = rig.pass(trial);
        let failure = result.failure.expect("the rail fails core 11");
        assert_eq!(
            (failure.signal, failure.core),
            (Signal::ComputationError, core(11)),
            "seed {seed}"
        );
    }
}

pub(crate) fn tuned_rail(mut definition: MachineDefinition) -> MachineDefinition {
    definition.core = flat_cores(16, -50, -50);
    for workload in definition
        .shared_voltage
        .as_mut()
        .expect("a rail")
        .workload
        .values_mut()
    {
        workload.full_mhz = [workload.reference_mhz + 100.0; 2];
        workload.idle_gain_mhz = 0.0;
        workload.package_mhz_per_w = 0.0;
        workload.balance_mhz_per_w = 0.0;
        for core in &mut workload.core {
            core.base_v = 1.2;
            core.threshold_v = 1.0;
            core.clock_v_per_100mhz = 0.03;
            core.threshold_clock_v_per_100mhz = 0.03;
            core.signals = None;
        }
        workload.core[0].threshold_v = 1.2;
    }
    definition
}

#[test]
fn the_rail_replaces_loaded_core_hazards_joints_and_ccd_for_multi_core_r7_only() {
    let mut definition = tuned_rail(committed_machine("shared-voltage.json"));
    definition.ccd = Some(CcdDef {
        log_rate: 10.0,
        ..CcdDef::default()
    });
    definition.joint = vec![JointDef {
        members: [(0, 0)].into(),
        regimes: vec![Regime::R7],
        rate: Some(100.0),
        ..JointDef::default()
    }];
    let rate = definition.shared_voltage.as_ref().expect("a rail").rate;
    let model = model(&definition);
    let regime = r7();
    let workload = r7_workload(0);
    let zero = offsets(&[0; 16]);
    let two = load(regime, workload, &[0, 1], SECONDS);
    let hazard = model.hazard(&zero, &two).expect("shape");
    close(hazard, rate, 1e-12);
    let want = -(-hazard * SECONDS).exp_m1();
    close(
        model.failure_probability(&zero, &two).expect("shape"),
        want,
        1e-12,
    );

    let deep = offsets(&[-10; 16]);
    assert!(model.failure_probability(&deep, &two).expect("shape") > want * 5.0);

    let single = load(regime, workload, &[0], SECONDS);
    assert!(
        model.hazard(&zero, &single).expect("shape") >= 100.0,
        "single-core R7 keeps its joints"
    );
}

#[test]
fn the_rail_drops_unloaded_flat_and_idle_hazards_but_other_loads_keep_theirs() {
    let mut definition = committed_machine("shared-voltage.json");
    definition
        .shared_voltage
        .as_mut()
        .expect("a rail")
        .background_rate = 0.001;
    for (id, flat, idle) in [(2, 0.02, None), (12, 0.01, Some(-20))] {
        let entry = definition
            .core
            .iter_mut()
            .find(|core| core.id == id)
            .expect("a core");
        entry.flat = flat;
        entry.idle = idle;
    }
    let model = model(&definition);
    let regime = r7();
    let workload = r7_workload(0);
    let shallow = offsets(&[-35; 16]);
    let mut moved = [-35; 16];
    moved[2] = 0;
    moved[12] = -45;
    moved[13] = -45;
    let moved = offsets(&moved);

    let two = load(regime, workload, &[0, 1], SECONDS);
    let seen = |profile| {
        let sources = model.sources(profile, &two).expect("shape");
        let pairs: Vec<_> = sources.iter().map(|s| (s.kind, s.rate)).collect();
        (
            pairs,
            model.hazard(profile, &two).expect("shape"),
            model.failure_probability(profile, &two).expect("shape"),
            model
                .r7_failure_share(profile, &two, core(0))
                .expect("shape"),
        )
    };
    let (pairs, ..) = seen(&shallow);
    let kinds: Vec<_> = pairs.iter().map(|(kind, _)| *kind).collect();
    assert_eq!(
        kinds,
        [
            SourceKind::Voltage(core(0)),
            SourceKind::Voltage(core(1)),
            SourceKind::Background,
        ],
        "a multi-core R7 load on the rail has the rail's and the background's sources only"
    );
    assert_eq!(
        seen(&shallow),
        seen(&moved),
        "unloaded offsets change nothing of a multi-core R7 load"
    );

    let kinds_of = |profile, load: &_| -> Vec<_> {
        model
            .sources(profile, load)
            .expect("shape")
            .iter()
            .map(|s| (s.kind, s.rate))
            .collect()
    };
    let single = load(regime, workload, &[0], SECONDS);
    let before = kinds_of(&shallow, &single);
    assert!(
        before
            .iter()
            .any(|(kind, rate)| *kind == SourceKind::Idle(core(12)) && *rate >= 0.01),
        "a single-core R7 load keeps the unloaded cores' mechanisms: {before:?}"
    );
    assert_ne!(before, kinds_of(&moved, &single));
    let other = load(Regime::R2, workload, &[0, 1], SECONDS);
    let before = kinds_of(&shallow, &other);
    assert!(
        before
            .iter()
            .any(|(kind, _)| *kind == SourceKind::Idle(core(12))),
        "an R2 load keeps them too: {before:?}"
    );
    assert_ne!(before, kinds_of(&moved, &other));
}

fn tuned(delta: f64, coefficient: f64) -> Model {
    let mut definition = committed_machine("shared-voltage.json");
    definition.core = flat_cores(16, -50, -50);
    let rail = definition.shared_voltage.as_mut().expect("a rail");
    let w = rail.workload.get_mut(r7_workload(0)).expect("workload");
    w.idle_gain_mhz = 0.0;
    w.package_mhz_per_w = 0.0;
    w.balance_mhz_per_w = 0.0;
    w.full_mhz = [w.reference_mhz + delta; 2];
    for core in &mut w.core {
        core.base_v = 1.2;
        core.threshold_v = 1.074;
        core.clock_v_per_100mhz = 0.03;
        core.threshold_clock_v_per_100mhz = coefficient;
    }
    model(&definition)
}

#[test_case(-100.0; "down")]
#[test_case(100.0; "up")]
fn equal_request_and_requirement_slopes_cancel_a_common_clock_shift(delta: f64) {
    let profile = offsets(&[-35; 16]);
    let two = load(r7(), r7_workload(0), &[0, 1], SECONDS);
    let reference = tuned(0.0, 0.03).hazard(&profile, &two).expect("shape");
    let model = tuned(delta, 0.03);
    close(
        model.hazard(&profile, &two).expect("shape"),
        reference,
        1e-12,
    );
    let want = -(-reference * SECONDS).exp_m1();
    close(
        model.failure_probability(&profile, &two).expect("shape"),
        want,
        1e-12,
    );
}

#[test]
fn a_steeper_requirement_slope_raises_the_hazard_of_a_faster_clock() {
    let profile = offsets(&[-35; 16]);
    let two = load(r7(), r7_workload(0), &[0, 1], SECONDS);
    let reference = tuned(0.0, 0.03).hazard(&profile, &two).expect("shape");
    let steeper = tuned(100.0, 0.04).hazard(&profile, &two).expect("shape");
    assert!(steeper > reference, "{steeper} <= {reference}");
}

#[test_case(&[0, 1]; "two cores")]
#[test_case(&[8, 9, 10, 11, 12, 13, 14, 15]; "a whole ccd")]
fn an_omitted_requirement_coefficient_keeps_the_clock_independent_threshold(cores: &[i64]) {
    let model = voltage_model();
    let regime = r7();
    let workload = r7_workload(0);
    let voltage = model.voltage().expect("a rail");
    let state = model
        .voltage_state(
            &offsets(&[-35; 16]),
            &load(regime, workload, cores, SECONDS),
        )
        .expect("shape")
        .expect("a rail");
    for &id in cores {
        let parameters = &voltage.workload(workload).cores[usize::try_from(id).expect("index")];
        assert_eq!(parameters.threshold_clock_v_per_100mhz, 0.0);
        let x = (parameters.threshold_v - state.rail) / voltage.margin_v;
        let want = voltage.rate * (x.max(0.0) + (-x.abs()).exp().ln_1p()) / std::f64::consts::LN_2;
        assert_eq!(state.rates[usize::try_from(id).expect("index")], want);
    }
}

#[test]
fn a_core_count_of_zero_selects_the_default_volts_per_count() {
    let voltage_model = voltage_model();
    let voltage = voltage_model.voltage().expect("a rail");
    assert!(
        voltage
            .workloads
            .iter()
            .flat_map(|workload| &workload.cores)
            .all(|core| core.count_v == DEFAULT_COUNT_V)
    );
}

fn rewrite_cores(
    mut definition: MachineDefinition,
    edit: impl Fn(&mut VoltageCoreDef),
) -> MachineDefinition {
    definition.core = flat_cores(16, -50, -50);
    let rail = definition.shared_voltage.as_mut().expect("a rail");
    for workload in rail.workload.values_mut() {
        workload.core.iter_mut().for_each(&edit);
    }
    definition
}

#[test]
fn the_rail_is_the_highest_loaded_request_even_when_every_request_is_negative() {
    let definition = rewrite_cores(committed_machine("shared-voltage.json"), |core| {
        core.base_v = 0.1;
        core.threshold_v = 0.1;
        core.count_v = 0.0;
        core.clock_v_per_100mhz = 0.0;
        core.threshold_clock_v_per_100mhz = 0.0;
    });
    let model = model(&definition);
    let voltage = model.voltage().expect("a rail");
    let workload = r7_workload(0);
    let profile = offsets(&[-50; 16]);
    let state = |cores: &[i64]| {
        model
            .voltage_state(&profile, &load(r7(), workload, cores, SECONDS))
            .expect("shape")
            .expect("a rail")
    };

    let request = 0.1 + DEFAULT_COUNT_V * -50.0;
    assert!(request < 0.0, "{request}");
    let two = state(&[0, 1]);
    close(two.rail, request, 1e-12);
    assert_eq!(two.requests[0], narrowed(request));
    let x = (0.1 - request) / voltage.margin_v;
    let want = voltage.rate * (x + (-x).exp().ln_1p()) / std::f64::consts::LN_2;
    for core in [0, 1] {
        close(two.rates[core], want, 1e-9 * want);
    }

    let none = state(&[]);
    assert_eq!(none.rail, 0.0, "no loaded request, no rail");
    assert_eq!(none.rates, [0.0; 16]);
}

#[test]
fn a_margin_whose_normalized_deficit_overflows_keeps_its_representable_hazard() {
    let tiny = 1e-310;
    let mut definition = rewrite_cores(committed_machine("shared-voltage.json"), |core| {
        core.base_v = 1.2;
        core.threshold_v = 1.2;
        core.count_v = 0.0;
        core.clock_v_per_100mhz = 0.0;
        core.threshold_clock_v_per_100mhz = 0.0;
    });
    let rail = definition.shared_voltage.as_mut().expect("a rail");
    rail.margin_v = tiny;
    rail.rate = tiny;
    let model = model(&definition);
    let workload = r7_workload(0);
    let profile = offsets(&[-35; 16]);
    // Every loaded core asks 1.2 - 35 · 0.0036 = 1.074 V of a 1.2 V requirement: a deficit of 0.126 V,
    // `rate · deficit / margin_v` of which is the 0.126 failures per second the equal tiny `rate` and
    // `margin_v` leave, over ln 2, though the deficit over the margin alone is infinite.
    let per_core = 0.126 / std::f64::consts::LN_2;
    let one_second = load(r7(), workload, &[0, 1], 1.0);
    let state = model
        .voltage_state(&profile, &one_second)
        .expect("shape")
        .expect("a rail");
    for core in [0, 1] {
        close(state.rates[core], per_core, 1e-9);
    }
    let hazard = model.hazard(&profile, &one_second).expect("shape");
    close(hazard, 2.0 * per_core, 1e-9);
    close(
        model
            .failure_probability(&profile, &one_second)
            .expect("shape"),
        -(-hazard).exp_m1(),
        1e-12,
    );
    for id in [0, 1] {
        let share = model
            .r7_failure_share(&profile, &one_second, core(id))
            .expect("shape");
        close(share, 0.5, 1e-9);
    }
}

#[test]
fn the_share_of_two_rates_whose_sum_overflows_is_still_their_ratio() {
    let mut definition = rewrite_cores(committed_machine("shared-voltage.json"), |core| {
        core.base_v = 1.2;
        core.threshold_v = 1.2;
        core.count_v = 0.0;
        core.clock_v_per_100mhz = 0.0;
        core.threshold_clock_v_per_100mhz = 0.0;
    });
    let rail = definition.shared_voltage.as_mut().expect("a rail");
    rail.rate = 1e307;
    rail.margin_v = 1.0;
    rail.background_rate = 1.7e308;
    let model = model(&definition);
    let profile = offsets(&[-35; 16]);
    let two = load(r7(), r7_workload(0), &[0, 1], 1.0);
    let state = model
        .voltage_state(&profile, &two)
        .expect("shape")
        .expect("a rail");
    let hazard = model.hazard(&profile, &two).expect("shape");
    assert!(hazard.is_infinite(), "the steady hazard does not fit");
    let ratio = state.rates[0] / 1.7e308;
    for id in [0, 1] {
        let share = model
            .r7_failure_share(&profile, &two, core(id))
            .expect("shape");
        close(share, ratio / (1.0 + 2.0 * ratio), 1e-9);
    }
    assert_eq!(model.failure_probability(&profile, &two), Ok(1.0));
}

#[test_case(0.0; "a quiet idle core")]
#[test_case(0.01; "a failing idle core")]
fn failure_share_is_a_loaded_cores_rail_rate_over_the_total(idle_rate: f64) {
    let profile = offsets(&[-35; 16]);
    let three = load(r7(), r7_workload(0), &[0, 1, 7], SECONDS);
    let mut definition = committed_machine("shared-voltage.json");
    definition
        .core
        .iter_mut()
        .find(|core| core.id == 8)
        .expect("core 8")
        .flat = idle_rate;
    let model = model(&definition);
    let state = model
        .voltage_state(&profile, &three)
        .expect("shape")
        .expect("a rail");
    let total = model.hazard(&profile, &three).expect("shape");
    for id in [0, 1, 7] {
        let share = model
            .r7_failure_share(&profile, &three, core(id))
            .expect("shape");
        close(
            share,
            state.rates[usize::try_from(id).expect("index")] / total,
            1e-12,
        );
    }
    assert_eq!(
        model.r7_failure_share(&profile, &three, core(8)),
        Ok(0.0),
        "unloaded"
    );
}

#[test_case(Regime::R7, &[0]; "a single r7 core")]
#[test_case(Regime::R2, &[0, 1]; "an r2 pair")]
fn failure_share_is_zero_outside_a_multi_core_r7_load(regime: Regime, cores: &[i64]) {
    let model = voltage_model();
    let profile = offsets(&[-35; 16]);
    let other = load(regime, r7_workload(0), cores, SECONDS);
    assert_eq!(model.r7_failure_share(&profile, &other, core(0)), Ok(0.0));
}

#[test]
fn failure_share_is_zero_without_a_rail_hazard() {
    let profile = offsets(&[-35; 16]);
    let three = load(r7(), r7_workload(0), &[0, 1, 7], SECONDS);
    let legacy = crate::common::model(&crate::common::machine(16, vec![]));
    assert_eq!(legacy.r7_failure_share(&profile, &three, core(0)), Ok(0.0));

    let mut dead = committed_machine("shared-voltage.json");
    for workload in dead
        .shared_voltage
        .as_mut()
        .expect("a rail")
        .workload
        .values_mut()
    {
        for core in &mut workload.core {
            core.base_v = 100.0;
        }
    }
    let dead = crate::common::model(&dead);
    assert_eq!(dead.hazard(&profile, &three), Ok(0.0));
    assert_eq!(dead.r7_failure_share(&profile, &three, core(0)), Ok(0.0));
}

fn background_machine(rate: f64) -> MachineDefinition {
    let mut definition = committed_machine("shared-voltage.json");
    definition.core = flat_cores(16, -50, -50);
    definition
        .shared_voltage
        .as_mut()
        .expect("a rail")
        .background_rate = rate;
    for workload in definition
        .shared_voltage
        .as_mut()
        .expect("a rail")
        .workload
        .values_mut()
    {
        for core in &mut workload.core {
            core.base_v = 100.0;
        }
    }
    definition
}

#[test]
fn the_background_rate_ignores_profile_workload_and_load_and_takes_the_onset_boost() {
    let mut definition = background_machine(0.001);
    definition.model.onset_boost = 2.0;
    definition.model.onset_s = 10.0;
    let model = model(&definition);
    let regime = r7();
    for offset in [0, -35, -50] {
        let profile = offsets(&[offset; 16]);
        for workload in togi_machine::workloads(Regime::R7) {
            for cores in [&[0, 1][..], &[0, 1, 2, 3, 4, 5, 6, 7], &[0, 1, 8, 9]] {
                let case = format!("offset {offset}, {}, cores {cores:?}", workload.id);
                let two = load(regime, workload.id, cores, 120.0);
                assert_eq!(model.hazard(&profile, &two), Ok(0.001), "{case}");
                for duration in [5.0, 120.0] {
                    let timed = load(regime, workload.id, cores, duration);
                    let exposure = duration + 2.0 * f64::min(duration, 10.0);
                    assert_eq!(
                        model.failure_probability(&profile, &timed),
                        Ok(-(-0.001 * exposure).exp_m1()),
                        "{case}, {duration} s"
                    );
                }
                assert_eq!(
                    model.r7_failure_share(&profile, &two, core(cores[0])),
                    Ok(0.0),
                    "background names no core: {case}"
                );
            }
        }
    }
    let silent = crate::common::model(&background_machine(0.0));
    let zero = offsets(&[0; 16]);
    assert_eq!(
        silent.hazard(&zero, &load(regime, r7_workload(0), &[0, 1], 120.0)),
        Ok(0.0)
    );
}

#[test_case(Regime::R7, r7_workload(0), &[0]; "a single r7 core")]
#[test_case(Regime::R2, "", &[0, 1]; "an r2 pair")]
fn the_background_rate_stays_out_of_other_loads(regime: Regime, workload: &str, cores: &[i64]) {
    let model = model(&background_machine(0.001));
    let outside = load(regime, workload, cores, 120.0);
    assert_eq!(model.hazard(&offsets(&[0; 16]), &outside), Ok(0.0));
}

#[test]
fn the_background_is_part_of_the_total_and_never_of_a_numerator() {
    let mut definition = committed_machine("shared-voltage.json");
    definition
        .shared_voltage
        .as_mut()
        .expect("a rail")
        .background_rate = 0.01;
    let model = model(&definition);
    let regime = r7();
    let profile = offsets(&[-35; 16]);
    let two = load(regime, r7_workload(0), &[0, 1], SECONDS);
    let state = model
        .voltage_state(&profile, &two)
        .expect("shape")
        .expect("a rail");
    let shared: f64 = state.rates.iter().sum();
    for id in [0, 1] {
        let share = model
            .r7_failure_share(&profile, &two, core(id))
            .expect("shape");
        close(
            share,
            state.rates[usize::try_from(id).expect("index")] / (shared + 0.01),
            1e-12,
        );
    }
}

#[test]
fn r7_requests_exist_for_multi_core_r7_and_telemetry_for_any_trial_on_a_rail() {
    let model = voltage_model();
    let regime = r7();
    let profile = offsets(&[-35; 16]);
    let idle_v = narrowed(model.voltage().expect("a rail").idle_v);
    let two = load(regime, r7_workload(0), &[0, 1], SECONDS);
    let requests = model
        .r7_requests(&profile, &two)
        .expect("shape")
        .expect("shared");
    assert!(requests[0] > idle_v && requests[1] > idle_v);
    for request in &requests[2..] {
        assert_eq!(*request, idle_v);
    }
    assert_eq!(
        model.r7_requests(&profile, &load(regime, "", &[0], SECONDS)),
        Ok(None)
    );
    let other = load(Regime::R2, "mprime-sse-4k-21k", &[0, 1], SECONDS);
    assert_eq!(model.r7_requests(&profile, &other), Ok(None));
    let telemetry = model
        .voltage_state(&profile, &other)
        .expect("shape")
        .expect("a rail");
    let first = load(Regime::R2, r7_workload(0), &[0, 1], SECONDS);
    assert_eq!(
        Some(telemetry),
        model.voltage_state(&profile, &first).expect("shape"),
        "other regimes borrow the first R7 workload's parameters"
    );
    let legacy = crate::common::model(&crate::common::machine(16, vec![]));
    assert_eq!(legacy.voltage_state(&profile, &two), Ok(None));
    assert_eq!(legacy.r7_requests(&profile, &two), Ok(None));
}

fn edit_rail(edit: impl FnOnce(&mut SharedVoltageDef)) -> MachineDefinition {
    let mut definition = committed_machine("shared-voltage.json");
    edit(definition.shared_voltage.as_mut().expect("a rail"));
    definition
}

#[test_case(1e100; "past f32")]
#[test_case(f64::MAX; "the largest f64")]
fn idle_voltage_must_stay_finite_at_recording_precision(idle_v: f64) {
    let definition = edit_rail(|rail| rail.idle_v = idle_v);
    assert!(
        Model::new(&definition, 1).is_err(),
        "idle_v {idle_v} overflows recorded f32 voltage requests"
    );
    let error = invalid(&definition);
    assert_eq!(
        (error.path.as_str(), error.problem),
        ("shared_voltage.idle_v", Problem::NotFinite)
    );
}

#[test_case(
    |rail| {
        for workload in rail.workload.values_mut() {
            workload.core[0].base_v = 1e100;
        }
    };
    "a base that overflows f32"
)]
#[test_case(
    |rail| {
        for workload in rail.workload.values_mut() {
            workload.core[0].count_v = 1e38;
        }
    };
    "a count slope that sends the deepest request below f32"
)]
fn a_loaded_request_must_stay_finite_at_recording_precision(edit: RailEdit) {
    let error = invalid(&edit_rail(edit));
    assert_eq!(error.problem, Problem::Overflow, "{error}");
    assert!(
        error.path.starts_with("shared_voltage.workload.") && error.path.ends_with(".core[0]"),
        "{error}"
    );
}

#[test]
fn the_rail_needs_sixteen_cores_the_three_r7_workloads_and_sixteen_core_entries() {
    let mut eight = committed_machine("shared-voltage.json");
    eight.core.clear();
    eight.cores = 8;
    let error = invalid(&eight);
    assert_eq!(
        (error.path.as_str(), error.problem),
        ("shared_voltage", Problem::NeedsSixteenCores)
    );

    let missing = edit_rail(|rail| {
        rail.workload.remove(r7_workload(0));
    });
    let error = invalid(&missing);
    assert_eq!(
        (error.path.as_str(), error.problem),
        ("shared_voltage.workload", Problem::Workloads)
    );
    let extra = edit_rail(|rail| {
        let copy = rail.workload[r7_workload(0)].clone();
        rail.workload.insert("mprime-sse-4k-21k".to_owned(), copy);
    });
    assert_eq!(invalid(&extra).problem, Problem::Workloads);

    let short = edit_rail(|rail| {
        rail.workload
            .get_mut(r7_workload(0))
            .expect("workload")
            .core
            .pop();
    });
    let error = invalid(&short);
    assert_eq!(
        error.path,
        format!("shared_voltage.workload.{}.core", r7_workload(0))
    );
    assert_eq!(
        error.problem,
        Problem::Length {
            expected: 16,
            found: 15
        }
    );
}

#[test_case("margin_v", Problem::NotFinite, |r| r.margin_v = f64::NAN; "nan margin")]
#[test_case("idle_v", Problem::NotPositive, |r| r.idle_v = 0.0; "zero idle voltage")]
#[test_case("rate", Problem::NotPositive, |r| r.rate = -1.0; "negative rate")]
#[test_case("power_limit_w", Problem::NotFinite, |r| r.power_limit_w = f64::INFINITY; "infinite power limit")]
#[test_case("thermal_limit_w", Problem::NotPositive, |r| r.thermal_limit_w = 0.0; "zero thermal limit")]
fn rail_numbers_are_finite_and_in_range(field: &str, problem: Problem, edit: RailEdit) {
    let error = invalid(&edit_rail(edit));
    assert_eq!(
        (error.path, error.problem),
        (format!("shared_voltage.{field}"), problem)
    );
}

#[test_case("core[0].count_v", Problem::NotPositive, |w| w.core[0].count_v = -1.0; "negative count slope")]
#[test_case("core[0].base_v", Problem::NotPositive, |w| w.core[0].base_v = 0.0; "zero base")]
#[test_case("core[0].threshold_v", Problem::NotPositive, |w| w.core[0].threshold_v = -1.0; "negative threshold")]
#[test_case("core[0].clock_v_per_100mhz", Problem::Negative, |w| w.core[0].clock_v_per_100mhz = -0.1; "negative clock slope")]
#[test_case(
    "core[0].signals.crash",
    Problem::NotFinite,
    |w| w.core[0].signals = Some(Weights::from([(Signal::Crash, f64::INFINITY)]));
    "infinite signal weight"
)]
#[test_case("core[0].signals", Problem::ZeroTotal, |w| w.core[0].signals = Some(Weights::default()); "no signal weight")]
#[test_case("reference_mhz", Problem::NotPositive, |w| w.reference_mhz = 0.0; "zero reference clock")]
#[test_case("full_mhz[1]", Problem::NotPositive, |w| w.full_mhz[1] = 0.0; "zero full clock")]
#[test_case("package_mhz_per_w", Problem::Negative, |w| w.package_mhz_per_w = -1.0; "negative package slope")]
fn workload_numbers_are_finite_and_in_range(field: &str, problem: Problem, edit: WorkloadEdit) {
    let id = r7_workload(0);
    let error = invalid(&edit_rail(|r| {
        edit(r.workload.get_mut(id).expect("workload"));
    }));
    assert_eq!(
        (error.path, error.problem),
        (format!("shared_voltage.workload.{id}.{field}"), problem)
    );
}

#[test]
fn rail_fields_are_checked_in_document_order() {
    let id = r7_workload(0);
    let error = invalid(&edit_rail(|rail| {
        rail.thermal_limit_w = 0.0;
        rail.workload.get_mut(id).expect("workload").core.pop();
    }));
    assert_eq!(
        (error.path.as_str(), error.problem),
        ("shared_voltage.thermal_limit_w", Problem::NotPositive),
        "the rail's numbers come before its workloads"
    );
    let error = invalid(&edit_rail(|rail| {
        let workload = rail.workload.get_mut(id).expect("workload");
        workload.core.pop();
        workload.balance_mhz_per_w = -1.0;
    }));
    assert_eq!(
        (error.path, error.problem),
        (
            format!("shared_voltage.workload.{id}.balance_mhz_per_w"),
            Problem::Negative
        ),
        "a workload's numbers come before its core table"
    );
    let error = invalid(&edit_rail(|rail| {
        let workload = rail.workload.get_mut(id).expect("workload");
        workload.core.pop();
        workload.core[0].base_v = 0.0;
    }));
    assert_eq!(
        error.path,
        format!("shared_voltage.workload.{id}.core"),
        "the table's length comes before its entries"
    );
}

#[test_case(
    |rail| {
        rail.rate = 1e308;
        rail.margin_v = 1e-3;
    };
    "a rate that overflows over a small margin"
)]
#[test_case(
    |rail| {
        for workload in rail.workload.values_mut() {
            workload.core[0].threshold_clock_v_per_100mhz = 1e308;
        }
    };
    "a requirement slope that overflows the threshold"
)]
fn a_rail_whose_hazard_cannot_be_represented_is_refused(edit: RailEdit) {
    let error = invalid(&edit_rail(edit));
    assert_eq!(error.problem, Problem::Overflow, "{error}");
    assert!(
        error.path.starts_with("shared_voltage.workload.") && error.path.ends_with(".core[0]"),
        "{error}"
    );
}

#[test_case(0, 0.0; "at offset zero")]
#[test_case(-35, 0.126; "at offset -35")]
#[test_case(-50, 0.18; "at the floor")]
fn a_rail_whose_deficit_stays_small_is_accepted_however_large_its_rate(offset: i64, deficit: f64) {
    // Requests and requirements share a 1.2 V base and a default 0.0036 V per count, so a core asks at
    // least 1.02 V of a 1.2 V requirement: the deficit is at most 0.18 V and every source rate is below
    // 1.27e308, though the sum of the two voltages (2.58 V) over a 1 V margin would overflow.
    let mut definition = rewrite_cores(committed_machine("shared-voltage.json"), |core| {
        core.base_v = 1.2;
        core.threshold_v = 1.2;
        core.count_v = 0.0;
        core.clock_v_per_100mhz = 0.0;
        core.threshold_clock_v_per_100mhz = 0.0;
    });
    let rail = definition.shared_voltage.as_mut().expect("a rail");
    rail.rate = 1e308;
    rail.margin_v = 1.0;
    let model = model(&definition);
    let two = load(r7(), r7_workload(0), &[0, 1], 1.0);
    let softplus =
        |deficit: f64| 1e308 * (deficit + (-deficit).exp().ln_1p()) / std::f64::consts::LN_2;
    let profile = offsets(&[offset; 16]);
    let sources = model.sources(&profile, &two).expect("shape");
    assert_eq!(sources.len(), 2);
    for source in &sources {
        assert!(source.rate.is_finite(), "{source:?}");
        close(source.rate / softplus(deficit), 1.0, 1e-9);
    }
    for id in [0, 1] {
        let share = model
            .r7_failure_share(&profile, &two, core(id))
            .expect("shape");
        close(share, 0.5, 1e-9);
    }
    assert_eq!(model.failure_probability(&profile, &two), Ok(1.0));
    file::encode(&definition).expect("a machine that is valid encodes");
}

#[test]
fn a_rail_whose_common_clock_slopes_hide_its_deficit_at_the_clock_extremes_is_refused() {
    // Both clock slopes are 1e16 V per 100 MHz, so at the extreme clocks the request and the requirement are
    // about 1e17 to 1e23 V and 1.02 V against 1.2 V rounds away. At the 5000 MHz reference the deficit is
    // 0.18 V over a 1e-310 V margin: a rate of 1.8e309 per second, which no f64 holds.
    let mut definition = rewrite_cores(committed_machine("shared-voltage.json"), |core| {
        core.base_v = 1.2;
        core.threshold_v = 1.2;
        core.count_v = 0.0;
        core.clock_v_per_100mhz = 1e16;
        core.threshold_clock_v_per_100mhz = 1e16;
    });
    let rail = definition.shared_voltage.as_mut().expect("a rail");
    rail.rate = 1.0;
    rail.margin_v = 1e-310;
    for workload in rail.workload.values_mut() {
        workload.reference_mhz = 5000.0;
        workload.full_mhz = [5000.0; 2];
        workload.idle_gain_mhz = 0.0;
        workload.package_mhz_per_w = 0.0;
        workload.balance_mhz_per_w = 0.0;
    }
    let error = invalid(&definition);
    assert_eq!(error.problem, Problem::Overflow, "{error}");
    assert!(
        error.path.starts_with("shared_voltage.workload.") && error.path.ends_with(".core[0]"),
        "{error}"
    );
    // With a margin in range the same slopes are harmless: the rates stay finite at the reference clock.
    definition.shared_voltage.as_mut().expect("a rail").margin_v = 1.0;
    let model = model(&definition);
    let two = load(r7(), r7_workload(0), &[0, 1], 1.0);
    let sources = model.sources(&offsets(&[-50; 16]), &two).expect("shape");
    assert_eq!(sources.len(), 2);
    assert!(sources.iter().all(|source| source.rate.is_finite()));
}

#[test]
fn a_rail_whose_terms_round_before_they_cancel_is_refused() {
    // Bases of 1e18 V and a reference of 1e20 MHz cancel at the extreme clocks, but at the actual 32768 MHz
    // clock binary64 rounds the request to 128 V and the requirement to 256 V. Over a 1e-310 V margin that
    // is no f64 rate, so the refusal bounds the rounding by the terms before they cancel.
    let mut definition = rewrite_cores(committed_machine("shared-voltage.json"), |core| {
        core.base_v = 1e18;
        core.threshold_v = 1e18 - 128.0;
        core.count_v = 0.0;
        core.clock_v_per_100mhz = 1.0;
        core.threshold_clock_v_per_100mhz = 1.0 - f64::EPSILON / 2.0;
    });
    let rail = definition.shared_voltage.as_mut().expect("a rail");
    rail.rate = 1.0;
    rail.margin_v = 1e-310;
    for workload in rail.workload.values_mut() {
        workload.reference_mhz = 1e20 + 16384.0;
        workload.full_mhz = [32768.0; 2];
        workload.idle_gain_mhz = 0.0;
        workload.package_mhz_per_w = 0.0;
        workload.balance_mhz_per_w = 0.0;
    }
    let error = invalid(&definition);
    assert_eq!(error.problem, Problem::Overflow, "{error}");
    assert!(
        error.path.starts_with("shared_voltage.workload.") && error.path.ends_with(".core[0]"),
        "{error}"
    );
}

#[test]
fn a_rail_rate_that_rounds_past_f64_stays_finite() {
    // Subnormal voltages make the refusal's rounding allowance vanish, yet at the 4294967294 MHz clock
    // and offset -50 the deficit is one subnormal over a one-subnormal margin at rate f64::MAX: a hazard
    // past f64. The rate is held at f64::MAX, so the sources stay finite and the share is a number.
    let s = f64::from_bits(1);
    let mut definition = rewrite_cores(committed_machine("shared-voltage.json"), |core| {
        core.base_v = 472_446_453.0 * s;
        core.threshold_v = s;
        core.count_v = s;
        core.clock_v_per_100mhz = 50.0 * s;
        core.threshold_clock_v_per_100mhz = 61.0 * s;
    });
    let rail = definition.shared_voltage.as_mut().expect("a rail");
    rail.rate = f64::MAX;
    rail.margin_v = s;
    for workload in rail.workload.values_mut() {
        workload.reference_mhz = 1.0;
        workload.full_mhz = [4_294_967_294.0; 2];
        workload.idle_gain_mhz = 0.0;
        workload.package_mhz_per_w = 0.0;
        workload.balance_mhz_per_w = 0.0;
    }
    let model =
        togi_sim::Model::new(&definition, 1).expect("the refusal cannot bound this rounding");
    let profile = offsets(&[-50; 16]);
    let two = load(r7(), r7_workload(0), &[0, 1], 1.0);
    let sources = model.sources(&profile, &two).expect("shape");
    assert!(!sources.is_empty());
    assert!(
        sources.iter().all(|source| source.rate.is_finite()),
        "{sources:?}"
    );
    let share = model
        .r7_failure_share(&profile, &two, core(0))
        .expect("shape");
    assert!(share.is_finite(), "{share}");
}

#[test_case(-1.0, false; "negative")]
#[test_case(f64::NAN, false; "nan")]
#[test_case(f64::INFINITY, false; "infinite")]
#[test_case(f64::NEG_INFINITY, false; "negative infinite")]
#[test_case(0.0, true; "zero")]
#[test_case(0.027, true; "small")]
#[test_case(2.0, true; "large")]
fn requirement_coefficients_are_finite_and_nonnegative_without_an_upper_bound(
    coefficient: f64,
    valid: bool,
) {
    let workload = r7_workload(0);
    let definition = edit_rail(|rail| {
        rail.workload.get_mut(workload).expect("workload").core[0].threshold_clock_v_per_100mhz =
            coefficient;
    });
    assert_eq!(Model::new(&definition, 1).is_ok(), valid);
}

#[test_case(-1.0, false; "negative")]
#[test_case(f64::NAN, false; "nan")]
#[test_case(f64::INFINITY, false; "infinite")]
#[test_case(f64::NEG_INFINITY, false; "negative infinite")]
#[test_case(0.0, true; "zero")]
#[test_case(0.001, true; "small")]
#[test_case(2.0, true; "large")]
fn the_background_rate_is_finite_and_nonnegative(rate: f64, valid: bool) {
    let definition = edit_rail(|rail| rail.background_rate = rate);
    assert_eq!(Model::new(&definition, 1).is_ok(), valid);
}

#[test]
fn background_and_requirement_coefficients_decode_from_a_file() {
    let text = machine_dir_text("shared-voltage.json")
        .replace(
            r#""threshold_clock_v_per_100mhz": 0"#,
            r#""threshold_clock_v_per_100mhz": 0.027"#,
        )
        .replace(r#""background_rate": 0"#, r#""background_rate": 0.001"#);
    let definition = file::decode(&text).expect("decodes");
    let rail = definition.shared_voltage.as_ref().expect("a rail");
    assert_eq!(rail.background_rate, 0.001);
    for workload in rail.workload.values() {
        for core in &workload.core {
            assert_eq!(core.threshold_clock_v_per_100mhz, 0.027);
        }
    }
    let model = Model::new(&definition, 1).expect("valid");
    assert_eq!(model.voltage().expect("a rail").background_rate, 0.001);
}
