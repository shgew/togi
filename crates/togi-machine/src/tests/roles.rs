use super::{core, topology};
use crate::*;
use std::ops::Range;
use std::time::Duration;
use test_case::test_case;

fn cpu(value: u16) -> CpuId {
    CpuId::from(value)
}

fn class(regime: Regime, index: usize) -> WorkloadId {
    WorkloadId::new(regime, index).unwrap()
}

fn r2() -> WorkloadId {
    class(Regime::R2, 1)
}

fn spec_of(regime: Regime, workload: WorkloadId, seed: u64, roles: Vec<TrialRole>) -> TrialSpec {
    TrialSpec {
        id: "t".to_owned(),
        regime,
        workload,
        condition: Condition::Together,
        cores: TrialRole::cores(&roles),
        cpus: TrialRole::cpus(&roles),
        roles,
        duration: Duration::from_secs(120),
        index: 0,
        seed,
        offsets: PerCore::default(),
        controller: crate::ControllerMode::Managed {
            rotation: 0,
            purpose: crate::Purpose::Isolated,
        },
    }
}

fn neighbours(light: i64, index: usize) -> TrialSpec {
    let workload = class(Regime::R8, index);
    let roles = TrialRole::neighbours(workload, core(light), r2(), 7, &topology()).unwrap();
    spec_of(Regime::R8, workload, 7, roles)
}

fn all_threads(serial: u16) -> TrialSpec {
    let workload = class(Regime::R9, 0);
    let roles = TrialRole::all_threads(workload, cpu(serial), &topology()).unwrap();
    spec_of(Regime::R9, workload, 1, roles)
}

#[test_case(0, 0; "first core of the first CCD")]
#[test_case(3, 1; "a middle core")]
#[test_case(7, 2; "last core of the first CCD")]
#[test_case(8, 0; "first core of the second CCD")]
#[test_case(15, 5; "last core of the second CCD with a load-step light class")]
fn r8_runs_the_light_class_on_the_light_core_and_r2_on_its_ccd_neighbours(
    light: i64,
    index: usize,
) {
    let workload = class(Regime::R8, index);
    let roles = TrialRole::neighbours(workload, core(light), r2(), 7, &topology()).unwrap();
    let ccd = if light < 8 { 0..8 } else { 8..16 };
    let cores: Vec<CoreId> = ccd.clone().map(i64::from).map(core).collect();
    assert_eq!(TrialRole::cores(&roles), cores, "light core {light}");
    let first_cpus: Vec<CpuId> = ccd
        .map(|value| cpu(u16::try_from(value).unwrap()))
        .collect();
    assert_eq!(TrialRole::cpus(&roles), first_cpus, "light core {light}");
    let light_roles: Vec<&TrialRole> = roles
        .iter()
        .filter(|role| role.workload == workload.runs())
        .collect();
    assert_eq!(light_roles.len(), 1, "light core {light}");
    assert_eq!(light_roles[0].core, core(light), "light core {light}");
    for role in &roles {
        assert_eq!(role.instance, role.cpu, "{role:?}");
        if role.core == core(light) {
            continue;
        }
        assert_eq!(role.workload, r2(), "{role:?}");
        assert_eq!(role.schedule, PhaseSchedule::Sustained {}, "{role:?}");
    }
}

#[test]
fn r8_sustained_class_runs_an_r1_workload_and_a_step_class_runs_an_r3_workload_with_the_seed() {
    let sustained = class(Regime::R8, 1);
    assert_eq!(sustained.runs(), class(Regime::R1, 1));
    let roles = TrialRole::neighbours(sustained, core(2), r2(), 9, &topology()).unwrap();
    let light = roles.iter().find(|role| role.core == core(2)).unwrap();
    assert_eq!(light.schedule, PhaseSchedule::Sustained {});

    let steps = class(Regime::R8, 4);
    assert_eq!(steps.runs(), class(Regime::R3, 1));
    let roles = TrialRole::neighbours(steps, core(2), r2(), 9, &topology()).unwrap();
    let light = roles.iter().find(|role| role.core == core(2)).unwrap();
    assert_eq!(light.workload, class(Regime::R3, 1));
    assert_eq!(
        light.schedule,
        PhaseSchedule::Steps {
            load: LoadSchedule::Random { seed: 9 }
        }
    );
}

#[test]
fn r8_light_positions_make_distinct_role_sets() {
    let workload = class(Regime::R8, 0);
    let sets: Vec<Vec<TrialRole>> = (0..8)
        .map(|light| TrialRole::neighbours(workload, core(light), r2(), 1, &topology()).unwrap())
        .collect();
    for (a, left) in sets.iter().enumerate() {
        for (b, right) in sets.iter().enumerate().skip(a + 1) {
            assert_ne!(left, right, "light cores {a} and {b}");
        }
    }
}

#[test_case(0; "the first CPU")]
#[test_case(5; "a core's first CPU")]
#[test_case(21; "an SMT sibling")]
#[test_case(31; "the last CPU")]
fn r9_runs_every_logical_cpu_as_its_own_instance_with_one_serial_cpu(serial: u16) {
    let spec = all_threads(serial);
    assert_eq!(spec.roles.len(), 32);
    let listed: Vec<CpuId> = topology().into_iter().flat_map(|info| info.cpus).collect();
    assert_eq!(TrialRole::cpus(&spec.roles), listed);
    assert_eq!(
        TrialRole::cores(&spec.roles),
        (0..16).map(core).collect::<Vec<_>>()
    );
    let workload = class(Regime::R9, 0).runs();
    assert_eq!(workload, class(Regime::R1, 0));
    for role in &spec.roles {
        assert_eq!(role.instance, role.cpu, "{role:?}");
        assert_eq!(role.workload, workload, "{role:?}");
        assert_eq!(
            role.schedule,
            PhaseSchedule::Alternate {
                phase_s: 10,
                serial: role.cpu == cpu(serial),
            },
            "{role:?}"
        );
    }
    assert_eq!(spec.check_roles(&topology()), Ok(()));
}

#[test]
fn r9_cpus_follow_the_topology_by_core_then_listed_cpu() {
    let spec = all_threads(0);
    let expected: Vec<CpuId> = (0..16)
        .flat_map(|value| [cpu(value), cpu(value + 16)])
        .collect();
    assert_eq!(TrialRole::cpus(&spec.roles), expected);
    for (role, info) in spec.roles.chunks(2).zip(topology()) {
        assert_eq!(
            role.iter().map(|role| role.core).collect::<Vec<_>>(),
            [info.core, info.core]
        );
    }
}

#[test]
fn the_role_regime_constants_pin_the_documented_durations() {
    assert_eq!(Regime::R8.role_duration(), Some(Duration::from_secs(120)));
    assert_eq!(Regime::R9.role_duration(), Some(Duration::from_secs(120)));
    assert_eq!(Regime::R8.warm_up(), Some(Duration::from_secs(30)));
    assert_eq!(Regime::R9.warm_up(), Some(Duration::from_secs(30)));
    assert_eq!(R9_PHASE, Duration::from_secs(u64::from(R9_PHASE_S)));
    for regime in LEGACY_REGIMES {
        assert_eq!(regime.role_duration(), None, "{regime}");
        assert_eq!(regime.warm_up(), None, "{regime}");
        assert!(!regime.has_roles(), "{regime}");
    }
    for regime in [Regime::R8, Regime::R9] {
        assert!(regime.has_roles(), "{regime}");
    }
}

#[test]
fn the_legacy_regimes_are_the_regimes_without_roles_and_all_regimes_adds_the_role_regimes() {
    assert_eq!(LEGACY_REGIMES, ALL_REGIMES[..7]);
    assert_eq!(ALL_REGIMES[7..], [Regime::R8, Regime::R9]);
}

#[test_case(Regime::R8, 0, Regime::R1, 0)]
#[test_case(Regime::R8, 2, Regime::R1, 2)]
#[test_case(Regime::R8, 3, Regime::R3, 0)]
#[test_case(Regime::R8, 5, Regime::R3, 2)]
#[test_case(Regime::R9, 1, Regime::R1, 1)]
#[test_case(Regime::R7, 1, Regime::R7, 1)]
fn a_catalog_entry_runs_the_workload_it_names(
    regime: Regime,
    index: usize,
    runs: Regime,
    at: usize,
) {
    assert_eq!(class(regime, index).runs(), class(runs, at));
}

#[test_case(Regime::R1, 1, &[0], &[0], &[(0, 0)]; "R1 one thread")]
#[test_case(Regime::R5, 4, &[3], &[3, 19], &[(3, 3), (19, 3)]; "R5 both threads share an instance")]
#[test_case(Regime::R7, 0, &[2, 9], &[2, 9], &[(2, 2), (9, 9)]; "R7 one instance per core")]
fn legacy_roles_list_the_cpus_and_group_by_instance(
    regime: Regime,
    index: usize,
    cores: &[i64],
    cpus: &[u16],
    instances: &[(u16, u16)],
) {
    let cores: Vec<CoreId> = cores.iter().copied().map(core).collect();
    let cpus: Vec<CpuId> = cpus.iter().copied().map(cpu).collect();
    let workload = class(regime, index);
    let roles = TrialRole::legacy(regime, workload, 5, &cores, &cpus, &topology()).unwrap();
    assert_eq!(
        roles
            .iter()
            .map(|role| (role.cpu.get(), role.instance.get()))
            .collect::<Vec<_>>(),
        instances
    );
    assert_eq!(TrialRole::cpus(&roles), cpus);
    assert_eq!(TrialRole::cores(&roles), cores);
    for role in &roles {
        assert_eq!(role.workload, workload, "{role:?}");
        assert_eq!(role.schedule, PhaseSchedule::Sustained {}, "{role:?}");
    }
}

#[test]
fn legacy_r3_and_r4_roles_carry_their_load_schedule() {
    let r3 = class(Regime::R3, 2);
    let roles = TrialRole::legacy(Regime::R3, r3, 41, &[core(1)], &[cpu(1)], &topology()).unwrap();
    assert_eq!(
        roles[0].schedule,
        PhaseSchedule::Steps {
            load: LoadSchedule::Random { seed: 41 }
        }
    );
    let r4 = class(Regime::R4, 1);
    let roles = TrialRole::legacy(Regime::R4, r4, 41, &[core(1)], &[cpu(1)], &topology()).unwrap();
    assert_eq!(
        roles[0].schedule,
        PhaseSchedule::Steps {
            load: LoadSchedule::Duty {
                pct: DutyPct::new(50).unwrap()
            }
        }
    );
}

#[test]
fn legacy_r6_roles_carry_their_burst_turn_in_scheduling_order() {
    let workload = class(Regime::R6, 0);
    let cores = [core(9), core(0), core(1), core(8)];
    let cpus = cores.map(|core| cpu(u16::from(core.get())));
    let roles = TrialRole::legacy(Regime::R6, workload, 1, &cores, &cpus, &topology()).unwrap();
    // `order` interleaves the CCDs: core 0, core 8, core 1, core 9.
    let positions: Vec<(u8, PhaseSchedule)> = roles
        .iter()
        .map(|role| (role.core.get(), role.schedule))
        .collect();
    assert_eq!(
        positions,
        [
            (9, PhaseSchedule::Burst { position: 3 }),
            (0, PhaseSchedule::Burst { position: 0 }),
            (1, PhaseSchedule::Burst { position: 2 }),
            (8, PhaseSchedule::Burst { position: 1 }),
        ]
    );
}

#[test_case(Regime::R1, 0, &[0], &[0]; "R1")]
#[test_case(Regime::R2, 1, &[4], &[4]; "R2")]
#[test_case(Regime::R3, 3, &[4], &[4]; "R3")]
#[test_case(Regime::R4, 8, &[15], &[15]; "R4")]
#[test_case(Regime::R5, 0, &[7], &[7, 23]; "R5")]
#[test_case(Regime::R6, 2, &[0, 1, 2, 8], &[0, 1, 2, 8]; "R6")]
#[test_case(Regime::R7, 2, &[0, 1, 8, 9], &[0, 1, 8, 9]; "R7")]
fn a_generated_legacy_spec_passes_its_check(
    regime: Regime,
    index: usize,
    cores: &[i64],
    cpus: &[u16],
) {
    let cores: Vec<CoreId> = cores.iter().copied().map(core).collect();
    let cpus: Vec<CpuId> = cpus.iter().copied().map(cpu).collect();
    let workload = class(regime, index);
    let roles = TrialRole::legacy(regime, workload, 3, &cores, &cpus, &topology()).unwrap();
    let spec = spec_of(regime, workload, 3, roles);
    assert_eq!(spec.check_roles(&topology()), Ok(()));
}

#[test]
fn a_generated_r8_spec_passes_its_check_for_every_light_position_and_class() {
    for light in 0..16 {
        for index in 0..6 {
            let spec = neighbours(light, index);
            assert_eq!(
                spec.check_roles(&topology()),
                Ok(()),
                "light core {light}, class {index}"
            );
        }
    }
}

#[test_case(0..16, 0; "every core")]
#[test_case(0..8, 3; "the first CCD")]
#[test_case(8..16, 9; "the second CCD")]
fn a_generated_r9_spec_passes_its_check_on_the_cores_it_loads(cores: Range<usize>, serial: u16) {
    let workload = class(Regime::R9, 0);
    let loaded: Vec<CoreInfo> = topology()
        .into_iter()
        .filter(|info| cores.contains(&info.core.index()))
        .collect();
    let roles = TrialRole::all_threads(workload, cpu(serial), &loaded).unwrap();
    let spec = spec_of(Regime::R9, workload, 1, roles);
    assert_eq!(spec.check_roles(&topology()), Ok(()));
}

#[test_case(Regime::R8, 0, InvalidRoles::NotLegacy { regime: Regime::R8 }; "R8 has no legacy roles")]
#[test_case(Regime::R9, 0, InvalidRoles::NotLegacy { regime: Regime::R9 }; "R9 has no legacy roles")]
fn legacy_refuses_the_role_regimes(regime: Regime, index: usize, error: InvalidRoles) {
    let workload = class(regime, index);
    assert_eq!(
        TrialRole::legacy(regime, workload, 1, &[core(0)], &[cpu(0)], &topology()),
        Err(error)
    );
}

#[test]
fn legacy_refuses_a_workload_of_another_regime() {
    let r2 = class(Regime::R2, 0);
    assert_eq!(
        TrialRole::legacy(Regime::R1, r2, 1, &[core(0)], &[cpu(0)], &topology()),
        Err(InvalidRoles::WrongCatalog {
            expected: Regime::R1,
            got: r2
        })
    );
}

#[test]
fn legacy_refuses_cores_and_cpus_the_topology_does_not_carry() {
    let workload = class(Regime::R5, 0);
    let build = |cores: &[CoreId], cpus: &[CpuId]| {
        TrialRole::legacy(Regime::R5, workload, 1, cores, cpus, &topology())
    };
    assert_eq!(build(&[], &[cpu(0)]), Err(InvalidRoles::NoCores));
    assert_eq!(
        build(&[core(0)], &[cpu(0), cpu(0)]),
        Err(InvalidRoles::DuplicateCpu(cpu(0)))
    );
    assert_eq!(
        build(&[core(0)], &[cpu(1)]),
        Err(InvalidRoles::ForeignCpu(cpu(1)))
    );
    assert!(matches!(
        build(&[core(0), core(1)], &[cpu(0)]),
        Err(InvalidRoles::LoadShape { .. })
    ));
    assert!(matches!(
        build(&[core(0)], &[]),
        Err(InvalidRoles::LoadShape { .. })
    ));
    let r7 = class(Regime::R7, 0);
    assert_eq!(
        TrialRole::legacy(
            Regime::R7,
            r7,
            1,
            &[core(0), core(0)],
            &[cpu(0), cpu(0)],
            &topology()
        ),
        Err(InvalidRoles::DuplicateCore(core(0)))
    );
    assert_eq!(
        TrialRole::legacy(
            Regime::R7,
            r7,
            1,
            &[core(0)],
            &[cpu(0), cpu(1)],
            &topology()
        ),
        Err(InvalidRoles::LoadShape {
            regime: Regime::R7,
            requested: 1,
            cpus: 2
        })
    );
    let sparse = [CoreInfo {
        core: core(0),
        ccd: CcdId::new(0).unwrap(),
        cpus: Vec::new(),
    }];
    assert_eq!(
        TrialRole::legacy(
            Regime::R1,
            class(Regime::R1, 0),
            1,
            &[core(5)],
            &[cpu(5)],
            &sparse
        ),
        Err(InvalidRoles::UnknownCore(core(5)))
    );
}

#[test]
fn r8_refuses_a_class_a_heavy_workload_or_a_light_core_that_is_not_one() {
    let top = topology();
    let r8 = class(Regime::R8, 0);
    assert_eq!(
        TrialRole::neighbours(class(Regime::R1, 0), core(0), r2(), 1, &top),
        Err(InvalidRoles::WrongCatalog {
            expected: Regime::R8,
            got: class(Regime::R1, 0)
        })
    );
    assert_eq!(
        TrialRole::neighbours(r8, core(0), class(Regime::R1, 0), 1, &top),
        Err(InvalidRoles::WrongCatalog {
            expected: Regime::R2,
            got: class(Regime::R1, 0)
        })
    );
    let alone: Vec<CoreInfo> = top
        .iter()
        .filter(|info| info.core == core(0))
        .cloned()
        .collect();
    assert_eq!(
        TrialRole::neighbours(r8, core(0), r2(), 1, &alone),
        Err(InvalidRoles::NoNeighbours(core(0)))
    );
    let rest: Vec<CoreInfo> = top
        .iter()
        .filter(|info| info.core != core(0))
        .cloned()
        .collect();
    assert_eq!(
        TrialRole::neighbours(r8, core(0), r2(), 1, &rest),
        Err(InvalidRoles::UnknownCore(core(0)))
    );
    let mut bare = topology();
    bare[1].cpus.clear();
    assert_eq!(
        TrialRole::neighbours(r8, core(0), r2(), 1, &bare),
        Err(InvalidRoles::CoreWithoutCpu(core(1)))
    );
}

#[test]
fn r9_refuses_a_class_a_serial_cpu_or_a_topology_that_is_not_one() {
    let top = topology();
    let r9 = class(Regime::R9, 0);
    assert_eq!(
        TrialRole::all_threads(class(Regime::R8, 0), cpu(0), &top),
        Err(InvalidRoles::WrongCatalog {
            expected: Regime::R9,
            got: class(Regime::R8, 0)
        })
    );
    assert_eq!(
        TrialRole::all_threads(r9, cpu(32), &top),
        Err(InvalidRoles::UnknownCpu(cpu(32)))
    );
    assert_eq!(
        TrialRole::all_threads(r9, cpu(0), &[]),
        Err(InvalidRoles::NoCores)
    );
    let mut bare = topology();
    bare[3].cpus.clear();
    assert_eq!(
        TrialRole::all_threads(r9, cpu(0), &bare),
        Err(InvalidRoles::CoreWithoutCpu(core(3)))
    );
    let mut shared = topology();
    shared[1].cpus = vec![cpu(0)];
    assert_eq!(
        TrialRole::all_threads(r9, cpu(0), &shared),
        Err(InvalidRoles::DuplicateCpu(cpu(0)))
    );
}

fn broken(mut spec: TrialSpec, change: impl FnOnce(&mut TrialSpec)) -> Result<(), InvalidRoles> {
    change(&mut spec);
    spec.check_roles(&topology())
}

#[test]
fn a_spec_without_roles_is_refused() {
    let mut spec = neighbours(0, 0);
    spec.roles.clear();
    assert_eq!(spec.check_roles(&topology()), Err(InvalidRoles::NoRoles));
}

#[test]
fn an_r9_spec_is_refused_unless_every_cpu_runs_with_a_valid_phase() {
    assert_eq!(
        broken(all_threads(0), |spec| {
            spec.roles.pop();
            spec.cpus.pop();
        }),
        Err(InvalidRoles::RolesDiffer),
        "a CPU without its role"
    );
    assert_eq!(
        broken(all_threads(0), |spec| {
            for role in &mut spec.roles {
                role.schedule = PhaseSchedule::Alternate {
                    phase_s: 0,
                    serial: role.cpu == cpu(0),
                };
            }
        }),
        Err(InvalidRoles::RolesDiffer),
        "a phase of no seconds"
    );
    assert_eq!(
        broken(all_threads(0), |spec| {
            for role in &mut spec.roles {
                role.schedule = PhaseSchedule::Alternate {
                    phase_s: 10,
                    serial: false,
                };
            }
        }),
        Err(InvalidRoles::NoAnchor),
        "no serial CPU"
    );
    assert_eq!(
        broken(all_threads(0), |spec| {
            if let PhaseSchedule::Alternate { serial, .. } = &mut spec.roles[3].schedule {
                *serial = true;
            }
        }),
        Err(InvalidRoles::RolesDiffer),
        "two serial CPUs"
    );
    assert_eq!(
        broken(all_threads(0), |spec| spec.cpus.reverse()),
        Err(InvalidRoles::LoadDiffers),
        "CPUs listed in another order"
    );
}

#[test]
fn an_r8_spec_is_refused_unless_its_roles_are_the_ones_its_light_core_derives() {
    assert_eq!(
        broken(neighbours(2, 0), |spec| spec.roles[5].core = core(0)),
        Err(InvalidRoles::RolesDiffer),
        "a core carrying two roles"
    );
    assert_eq!(
        broken(neighbours(2, 0), |spec| {
            spec.roles.remove(0);
            spec.cores.remove(0);
            spec.cpus.remove(0);
        }),
        Err(InvalidRoles::RolesDiffer),
        "a neighbour missing"
    );
    assert_eq!(
        broken(neighbours(2, 0), |spec| spec.roles[0].instance = cpu(1)),
        Err(InvalidRoles::RolesDiffer),
        "an instance that is another core's"
    );
    assert_eq!(
        broken(neighbours(2, 0), |spec| {
            spec.cores.pop();
        }),
        Err(InvalidRoles::LoadDiffers),
        "cores that are not the loaded ones"
    );
    assert_eq!(
        broken(neighbours(2, 0), |spec| {
            for role in &mut spec.roles {
                role.workload = class(Regime::R1, 0);
            }
        }),
        Err(InvalidRoles::WrongCatalog {
            expected: Regime::R2,
            got: class(Regime::R1, 0)
        }),
        "no heavy neighbour"
    );
    assert_eq!(
        broken(neighbours(2, 4), |spec| {
            spec.roles[2].schedule = PhaseSchedule::Steps {
                load: LoadSchedule::Random { seed: 8 },
            };
        }),
        Err(InvalidRoles::RolesDiffer),
        "a seed other than the trial's"
    );
}

#[test]
fn a_legacy_spec_is_refused_when_its_roles_leave_the_derivation() {
    let workload = class(Regime::R6, 0);
    let cores = [core(0), core(1)];
    let cpus = [cpu(0), cpu(1)];
    let roles = TrialRole::legacy(Regime::R6, workload, 1, &cores, &cpus, &topology()).unwrap();
    let spec = spec_of(Regime::R6, workload, 1, roles);
    assert_eq!(spec.check_roles(&topology()), Ok(()));
    assert_eq!(
        broken(spec.clone(), |spec| {
            spec.roles[1].schedule = PhaseSchedule::Burst { position: 2 };
        }),
        Err(InvalidRoles::RolesDiffer),
        "a burst position past the cores"
    );
    assert_eq!(
        broken(spec, |spec| spec.roles[0].schedule =
            PhaseSchedule::Sustained {}),
        Err(InvalidRoles::RolesDiffer),
        "a burst role that never bursts"
    );
}

#[test]
fn roles_round_trip_through_json_with_their_schedule() {
    for spec in [neighbours(1, 4), all_threads(20)] {
        let json = serde_json::to_string(&spec.roles).unwrap();
        assert_eq!(
            serde_json::from_str::<Vec<TrialRole>>(&json).unwrap(),
            spec.roles
        );
    }
    let burst = TrialRole {
        core: core(3),
        cpu: cpu(3),
        instance: cpu(3),
        workload: class(Regime::R6, 1),
        schedule: PhaseSchedule::Burst { position: 4 },
    };
    assert_eq!(
        serde_json::to_value(burst).unwrap(),
        serde_json::json!({
            "core": 3,
            "cpu": 3,
            "instance": 3,
            "workload": "ycruncher-bkt-sftv4-idle",
            "schedule": {"kind": "burst", "position": 4}
        })
    );
}

#[test_case(PhaseSchedule::Sustained {}, r#"{"kind":"sustained"}"#; "sustained")]
#[test_case(
    PhaseSchedule::Steps { load: LoadSchedule::Random { seed: 7 } },
    r#"{"kind":"steps","load":{"kind":"random","seed":7}}"#;
    "random steps"
)]
#[test_case(
    PhaseSchedule::Steps { load: LoadSchedule::Duty { pct: DutyPct::new(25).unwrap() } },
    r#"{"kind":"steps","load":{"kind":"duty","pct":25}}"#;
    "duty steps"
)]
#[test_case(PhaseSchedule::Burst { position: 2 }, r#"{"kind":"burst","position":2}"#; "burst")]
#[test_case(
    PhaseSchedule::Alternate { phase_s: 10, serial: true },
    r#"{"kind":"alternate","phase_s":10,"serial":true}"#;
    "alternate"
)]
fn a_phase_schedule_names_itself_on_the_wire(schedule: PhaseSchedule, json: &str) {
    assert_eq!(serde_json::to_string(&schedule).unwrap(), json);
    assert_eq!(
        serde_json::from_str::<PhaseSchedule>(json).unwrap(),
        schedule
    );
}

#[test_case(r#"{"kind":"burst"}"#; "a missing field")]
#[test_case(r#"{"kind":"sustained","extra":1}"#; "an unknown field")]
#[test_case(r#"{"kind":"unknown"}"#; "an unknown kind")]
#[test_case(r#"["burst",2]"#; "a positional sequence")]
#[test_case(r#"{"kind":"alternate","phase_s":-1,"serial":false}"#; "a negative phase")]
fn a_malformed_phase_schedule_is_refused(json: &str) {
    assert!(serde_json::from_str::<PhaseSchedule>(json).is_err());
}

#[test_case(r#"{"core":0,"cpu":0,"instance":0,"workload":"mprime-sse-4k-21k","schedule":{"kind":"sustained"},"extra":1}"#; "an unknown field")]
#[test_case(r#"{"core":0,"cpu":0,"instance":0,"workload":"nonsense","schedule":{"kind":"sustained"}}"#; "an unknown workload")]
#[test_case(r#"{"core":16,"cpu":0,"instance":0,"workload":"mprime-sse-4k-21k","schedule":{"kind":"sustained"}}"#; "a core past the table")]
#[test_case(r#"[0,0,0,"mprime-sse-4k-21k",{"kind":"sustained"}]"#; "a positional sequence")]
fn a_malformed_role_is_refused(json: &str) {
    assert!(serde_json::from_str::<TrialRole>(json).is_err());
}
