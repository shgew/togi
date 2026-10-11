// The shared-voltage rail: clocks, requests, hazards, failure share and validation.
// Go: TestSharedVoltageTopRequester, TestSharedVoltageClockShape, TestSharedVoltageCore11NeedsSupport (the
// computation-error mix is asserted on the mix, not on a draw), TestSharedVoltageDrawsAndScores (analytic
// part), TestSharedVoltageValidation, TestSharedVoltageThresholdClock, TestSharedVoltageDefaultThresholdClock,
// TestR7FailureShare (its allocation count: visiting sources allocates nothing by construction),
// TestSharedVoltageThresholdClockValidation, TestSharedVoltageThresholdClockDecodeAndOwnership,
// TestSharedVoltageBackgroundHazard, TestSharedVoltageBackgroundFailureShare,
// TestSharedVoltageBackgroundValidation.
// The sample, draw and replay-bypass tests wait for the runtime.

use togi_machine::{CoreId, Regime};
use togi_sim::definition::{
    CcdDef, DEFAULT_COUNT_V, JointDef, SharedVoltageDef, VoltageWorkloadDef, Weights,
};
use togi_sim::file;
use togi_sim::{MachineDefinition, Model, Problem, RegimeId, SignalKind, SourceKind};

use crate::common::{
    committed_machine, flat_cores, invalid, load, machine_dir_text, model, offsets, r7_workload,
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
            &load(&regime, workload, &[0, 1, 2, 3, 4, 5, 6, 7], SECONDS),
        )
        .expect("shape")
        .expect("a rail");
    close(full.rail, f64::from(full.requests[7]), 1e-7);

    let partial_load = load(&regime, workload, &[0, 1, 2, 3, 4, 5, 6], SECONDS);
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
            &load(&regime, workload, &[0, 1, 2, 3, 4, 5, 6, 10], SECONDS),
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
            .voltage_state(&offsets(profile), &load(&regime, workload, cores, SECONDS))
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

#[test]
fn core_eleven_needs_a_supporting_core_to_keep_its_rail() {
    let model = voltage_model();
    let regime = r7();
    let workload = r7_workload(1);
    let profile = offsets(&[
        -31, -38, -37, -34, -35, -36, -45, -42, -48, -47, -38, -44, -50, -42, -50, -50,
    ]);
    let state = |cores: &[i64]| {
        model
            .voltage_state(&profile, &load(&regime, workload, cores, SECONDS))
            .expect("shape")
            .expect("a rail")
    };
    let unsupported_cores = [8, 9, 11, 12, 14, 15];
    let unsupported = state(&unsupported_cores);
    let threshold = model.voltage().expect("a rail").workload(workload).cores[11].threshold_v;
    assert!(state(&[8, 9, 10, 11, 12, 13, 14, 15]).rail > threshold);
    for supporter in [10, 13] {
        let mut cores = unsupported_cores.to_vec();
        cores.push(supporter);
        let supported = state(&cores);
        assert!(unsupported.rail < threshold && supported.rail > threshold);
        assert!(unsupported.rates[11] > supported.rates[11] * 10.0);
    }
    let mix = model.voltage().expect("a rail").workload(workload).cores[11]
        .signals
        .as_ref()
        .expect("core 11 names its signals");
    for x in [0.0, 0.25, 0.5, 0.999] {
        assert_eq!(mix.pick(x), SignalKind::ComputationError);
    }
}

fn tuned_rail(mut definition: MachineDefinition) -> MachineDefinition {
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
        regimes: vec![RegimeId::R7],
        rate: Some(100.0),
        ..JointDef::default()
    }];
    let rate = definition.shared_voltage.as_ref().expect("a rail").rate;
    let model = model(&definition);
    let regime = r7();
    let workload = r7_workload(0);
    let zero = offsets(&[0; 16]);
    let two = load(&regime, workload, &[0, 1], SECONDS);
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

    let single = load(&regime, workload, &[0], SECONDS);
    assert!(
        model.hazard(&zero, &single).expect("shape") >= 100.0,
        "single-core R7 keeps its joints"
    );
}

#[test]
fn unloaded_core_hazards_survive_the_rail_and_sources_keep_their_order() {
    let mut definition = committed_machine("shared-voltage.json");
    definition
        .shared_voltage
        .as_mut()
        .expect("a rail")
        .background_rate = 0.001;
    definition
        .core
        .iter_mut()
        .find(|core| core.id == 12)
        .expect("core 12")
        .flat = 0.01;
    let model = model(&definition);
    let regime = r7();
    let sources = model
        .sources(
            &offsets(&[-35; 16]),
            &load(&regime, r7_workload(0), &[0, 1], SECONDS),
        )
        .expect("shape");
    let kinds: Vec<_> = sources.iter().map(|source| source.kind).collect();
    assert_eq!(
        kinds,
        [
            SourceKind::Idle(core(12)),
            SourceKind::Voltage(core(0)),
            SourceKind::Voltage(core(1)),
            SourceKind::Background,
        ]
    );
    assert_eq!(sources[0].rate, 0.01);
}

#[test]
fn equal_request_and_requirement_slopes_cancel_a_common_clock_shift() {
    let regime = r7();
    let workload = r7_workload(0);
    let profile = offsets(&[-35; 16]);
    let two = load(&regime, workload, &[0, 1], SECONDS);
    let tune = |delta: f64, coefficient: f64| {
        let mut definition = committed_machine("shared-voltage.json");
        definition.core = flat_cores(16, -50, -50);
        let rail = definition.shared_voltage.as_mut().expect("a rail");
        let w = rail.workload.get_mut(workload).expect("workload");
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
    };
    let reference = tune(0.0, 0.03).hazard(&profile, &two).expect("shape");
    for delta in [-100.0, 100.0] {
        let model = tune(delta, 0.03);
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
    let steeper = tune(100.0, 0.04).hazard(&profile, &two).expect("shape");
    assert!(steeper > reference, "{steeper} <= {reference}");
}

#[test]
fn an_omitted_requirement_coefficient_keeps_the_clock_independent_threshold() {
    let model = voltage_model();
    let regime = r7();
    let workload = r7_workload(0);
    let voltage = model.voltage().expect("a rail");
    for cores in [&[0, 1][..], &[8, 9, 10, 11, 12, 13, 14, 15]] {
        let state = model
            .voltage_state(
                &offsets(&[-35; 16]),
                &load(&regime, workload, cores, SECONDS),
            )
            .expect("shape")
            .expect("a rail");
        for &id in cores {
            let parameters = &voltage.workload(workload).cores[usize::try_from(id).expect("index")];
            assert_eq!(parameters.threshold_clock_v_per_100mhz, 0.0);
            let x = (parameters.threshold_v - state.rail) / voltage.margin_v;
            let want =
                voltage.rate * (x.max(0.0) + (-x.abs()).exp().ln_1p()) / std::f64::consts::LN_2;
            assert_eq!(state.rates[usize::try_from(id).expect("index")], want);
        }
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

#[test]
fn failure_share_is_a_loaded_cores_rail_rate_over_the_total() {
    let regime = r7();
    let workload = r7_workload(0);
    let profile = offsets(&[-35; 16]);
    let three = load(&regime, workload, &[0, 1, 7], SECONDS);
    for idle_rate in [0.0, 0.01] {
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
    let model = voltage_model();
    for other in [
        load(&regime, workload, &[0], SECONDS),
        load(&Regime::R2, workload, &[0, 1], SECONDS),
    ] {
        assert_eq!(model.r7_failure_share(&profile, &other, core(0)), Ok(0.0));
    }
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
        for workload in togi_machine::workloads(&Regime::R7) {
            for cores in [&[0, 1][..], &[0, 1, 2, 3, 4, 5, 6, 7], &[0, 1, 8, 9]] {
                let two = load(&regime, workload.id, cores, 120.0);
                assert_eq!(
                    model.hazard(&profile, &two),
                    Ok(0.001),
                    "{offset} {cores:?}"
                );
                for duration in [5.0, 120.0] {
                    let timed = load(&regime, workload.id, cores, duration);
                    let exposure = duration + 2.0 * f64::min(duration, 10.0);
                    assert_eq!(
                        model.failure_probability(&profile, &timed),
                        Ok(-(-0.001 * exposure).exp_m1())
                    );
                }
                assert_eq!(
                    model.r7_failure_share(&profile, &two, core(cores[0])),
                    Ok(0.0),
                    "background names no core"
                );
            }
        }
    }
    let zero = offsets(&[0; 16]);
    for outside in [
        load(&regime, r7_workload(0), &[0], 120.0),
        load(&Regime::R2, "", &[0, 1], 120.0),
    ] {
        assert_eq!(model.hazard(&zero, &outside), Ok(0.0));
    }
    let silent = crate::common::model(&background_machine(0.0));
    assert_eq!(
        silent.hazard(&zero, &load(&regime, r7_workload(0), &[0, 1], 120.0)),
        Ok(0.0)
    );
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
    let two = load(&regime, r7_workload(0), &[0, 1], SECONDS);
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
    let two = load(&regime, r7_workload(0), &[0, 1], SECONDS);
    let requests = model
        .r7_requests(&profile, &two)
        .expect("shape")
        .expect("shared");
    assert!(requests[0] > idle_v && requests[1] > idle_v);
    for request in &requests[2..] {
        assert_eq!(*request, idle_v);
    }
    assert_eq!(
        model.r7_requests(&profile, &load(&regime, "", &[0], SECONDS)),
        Ok(None)
    );
    let other = load(&Regime::R2, "mprime-sse-4k-21k", &[0, 1], SECONDS);
    assert_eq!(model.r7_requests(&profile, &other), Ok(None));
    let telemetry = model
        .voltage_state(&profile, &other)
        .expect("shape")
        .expect("a rail");
    let first = load(&Regime::R2, r7_workload(0), &[0, 1], SECONDS);
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

#[test]
fn rail_numbers_are_finite_and_in_range() {
    let rail: [(&str, Problem, RailEdit); 5] = [
        ("margin_v", Problem::NotFinite, |r| r.margin_v = f64::NAN),
        ("idle_v", Problem::NotPositive, |r| r.idle_v = 0.0),
        ("rate", Problem::NotPositive, |r| r.rate = -1.0),
        ("power_limit_w", Problem::NotFinite, |r| {
            r.power_limit_w = f64::INFINITY;
        }),
        ("thermal_limit_w", Problem::NotPositive, |r| {
            r.thermal_limit_w = 0.0;
        }),
    ];
    for (field, problem, edit) in rail {
        let error = invalid(&edit_rail(edit));
        assert_eq!(
            (error.path, error.problem),
            (format!("shared_voltage.{field}"), problem)
        );
    }

    let workload: [(&str, Problem, WorkloadEdit); 9] = [
        ("core[0].count_v", Problem::NotPositive, |w| {
            w.core[0].count_v = -1.0;
        }),
        ("core[0].base_v", Problem::NotPositive, |w| {
            w.core[0].base_v = 0.0;
        }),
        ("core[0].threshold_v", Problem::NotPositive, |w| {
            w.core[0].threshold_v = -1.0;
        }),
        ("core[0].clock_v_per_100mhz", Problem::Negative, |w| {
            w.core[0].clock_v_per_100mhz = -0.1;
        }),
        ("core[0].signals.crash", Problem::NotFinite, |w| {
            w.core[0].signals = Some(Weights::from([(SignalKind::Crash, f64::INFINITY)]));
        }),
        ("core[0].signals", Problem::ZeroTotal, |w| {
            w.core[0].signals = Some(Weights::default());
        }),
        ("reference_mhz", Problem::NotPositive, |w| {
            w.reference_mhz = 0.0;
        }),
        ("full_mhz[1]", Problem::NotPositive, |w| w.full_mhz[1] = 0.0),
        ("package_mhz_per_w", Problem::Negative, |w| {
            w.package_mhz_per_w = -1.0;
        }),
    ];
    let id = r7_workload(0);
    for (field, problem, edit) in workload {
        let error = invalid(&edit_rail(|r| {
            edit(r.workload.get_mut(id).expect("workload"));
        }));
        assert_eq!(
            (error.path, error.problem),
            (format!("shared_voltage.workload.{id}.{field}"), problem)
        );
    }
}

#[test]
fn requirement_coefficients_are_finite_and_nonnegative_without_an_upper_bound() {
    let workload = r7_workload(0);
    let set = |coefficient: f64| {
        edit_rail(|rail| {
            rail.workload.get_mut(workload).expect("workload").core[0]
                .threshold_clock_v_per_100mhz = coefficient;
        })
    };
    for coefficient in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(Model::new(&set(coefficient), 1).is_err(), "{coefficient}");
    }
    for coefficient in [0.0, 0.027, 2.0] {
        assert!(Model::new(&set(coefficient), 1).is_ok(), "{coefficient}");
    }
}

#[test]
fn the_background_rate_is_finite_and_nonnegative() {
    for rate in [-1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let definition = edit_rail(|rail| rail.background_rate = rate);
        assert!(Model::new(&definition, 1).is_err(), "{rate}");
    }
    for rate in [0.0, 0.001, 2.0] {
        let definition = edit_rail(|rail| rail.background_rate = rate);
        assert!(Model::new(&definition, 1).is_ok(), "{rate}");
    }
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
