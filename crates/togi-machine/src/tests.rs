use crate::*;
use flate2::{Compression, write::GzEncoder};
use proptest::prelude::*;
use std::io::{Cursor, Write};
use std::path::Path;

mod load;
mod seams;

fn core(value: i64) -> CoreId {
    CoreId::new(value).unwrap()
}

#[test]
fn clamp_offset_safety() {
    for value in -100..=100 {
        let offset = TargetOffset::clamped(value);
        assert_eq!(offset.get(), value.clamp(-50, 0));
        assert_eq!(TargetOffset::new(value).is_ok(), (-50..=0).contains(&value));
    }
}

#[test]
fn offsets_json_preserves_raw_readbacks() {
    for value in [i64::MIN, -100, -51, -50, 0, 10, i64::MAX] {
        let json = value.to_string();
        assert_eq!(serde_json::to_string(&ObservedOffset(value)).unwrap(), json);
        assert_eq!(
            serde_json::from_str::<ObservedOffset>(&json).unwrap(),
            ObservedOffset(value)
        );
        assert_eq!(
            serde_json::from_str::<TargetOffset>(&json).is_ok(),
            (-50..=0).contains(&value)
        );
    }
    assert_ne!(ObservedOffset(10), TargetOffset::ZERO.observed());
}

#[test]
fn core_order() {
    let cores: Vec<_> = (0..16_u8)
        .rev()
        .map(|value| CoreInfo {
            core: core(i64::from(value)),
            ccd: CcdId(value / 8),
            cpus: Vec::new(),
        })
        .collect();
    assert_eq!(
        order(&cores),
        [0, 8, 1, 9, 2, 10, 3, 11, 4, 12, 5, 13, 6, 14, 7, 15].map(core)
    );
    let uneven = [
        CoreInfo {
            core: core(9),
            ccd: CcdId(3),
            cpus: Vec::new(),
        },
        CoreInfo {
            core: core(2),
            ccd: CcdId(1),
            cpus: Vec::new(),
        },
        CoreInfo {
            core: core(0),
            ccd: CcdId(1),
            cpus: Vec::new(),
        },
    ];
    assert_eq!(order(&uneven), [0, 9, 2].map(core));
    assert!(order(&[]).is_empty());
}

#[test]
fn per_core_copies_are_independent() {
    let mut original = PerCore::<i64>::default();
    original.set(core(3), 1);
    let mut copied = original;
    copied.set(core(3), 2);
    copied.set(core(4), 5);
    assert_eq!(original.get(core(3)), Some(1));
    assert_eq!(original.len(), 1);
    assert_eq!(original.get(core(4)), None);
    assert_eq!(
        copied.iter().collect::<Vec<_>>(),
        [(core(3), 2), (core(4), 5)]
    );
}

#[test]
fn workload_regime_contracts() {
    for (regime, count, threads, bases) in [
        (Regime::R1, 3, 1, vec![Regime::R1]),
        (Regime::R2, 3, 1, vec![Regime::R2]),
        (Regime::R3, 6, 1, vec![Regime::R1, Regime::R2]),
        (Regime::R4, 9, 1, vec![Regime::R1]),
        (Regime::R5, 6, 2, vec![Regime::R1, Regime::R2]),
        (Regime::R6, 3, 1, vec![Regime::R1]),
        (Regime::R7, 3, 1, vec![Regime::R2]),
    ] {
        let catalog = workloads(regime);
        assert_eq!(catalog.len(), count);
        let mut ids = std::collections::BTreeSet::new();
        for (index, workload) in catalog.iter().enumerate() {
            assert!(ids.insert(workload.id));
            assert_eq!(workload.threads, threads);
            assert_eq!(workload_by_id(workload.id), Some(workload));
            let base = workload_by_id(workload.base).unwrap();
            assert_eq!(base.id, workload.base);
            assert_eq!(base.id, base.base);
            assert_eq!(base.backend, workload.backend);
            assert!(
                bases
                    .iter()
                    .any(|base_regime| workloads(*base_regime).contains(base))
            );
            if regime == Regime::R4 {
                assert!([25, 50, 75].contains(&workload.duty_pct.get()));
                assert_eq!(
                    workload.label,
                    format!("{} {}% duty", base.label, workload.duty_pct)
                );
            } else {
                assert_eq!(workload.duty_pct.get(), 0);
            }
            assert_eq!(pick_workload(regime, index + count * 3), workload);
        }
    }
    assert_eq!(workload_by_id("not-a-workload"), None);
}

fn write_gzip(path: &Path, content: &str) {
    let mut writer = GzEncoder::new(std::fs::File::create(path).unwrap(), Compression::default());
    writer.write_all(content.as_bytes()).unwrap();
    writer.finish().unwrap();
}

#[test]
fn read_samples_forms() {
    let two = "{\"elapsed_ms\":1000}\n{\"elapsed_ms\":2000}\n";
    for (plain, gzip, raw_gzip, want) in [
        (Some(two), None, false, vec![1000, 2000]),
        (None, Some(two), false, vec![1000, 2000]),
        (Some(two), Some("torn"), true, vec![1000, 2000]),
        (
            Some("{\"elapsed_ms\":1000}\n{\"elapsed_ms\":20"),
            None,
            false,
            vec![1000],
        ),
        (
            None,
            Some("{\"elapsed_ms\":1000}\n{\"elapsed_ms\":20"),
            false,
            vec![1000],
        ),
        (
            None,
            Some("{\"elapsed_ms\":1000}\nnot json\n{\"elapsed_ms\":2000}\n"),
            false,
            vec![1000, 2000],
        ),
        (None, Some("not gzip"), true, vec![]),
        (None, None, false, vec![]),
    ] {
        let dir = tempfile::tempdir().unwrap();
        if let Some(content) = plain {
            std::fs::write(dir.path().join(togi_trialfiles::SAMPLES), content).unwrap();
        }
        if let Some(content) = gzip {
            let path = dir.path().join(format!("{}.gz", togi_trialfiles::SAMPLES));
            if raw_gzip {
                std::fs::write(path, content).unwrap();
            } else {
                write_gzip(&path, content);
            }
        }
        let samples: Vec<_> = read_samples(dir.path()).collect();
        let expected: Vec<_> = want
            .into_iter()
            .map(|elapsed_ms| TrialConditions {
                elapsed_ms,
                ..TrialConditions::default()
            })
            .collect();
        assert_eq!(samples, expected);
    }
}

#[test]
fn read_samples_stops_early() {
    let dir = tempfile::tempdir().unwrap();
    write_gzip(
        &dir.path().join(format!("{}.gz", togi_trialfiles::SAMPLES)),
        "{\"elapsed_ms\":1}\n{\"elapsed_ms\":2}\n",
    );
    let samples: Vec<_> = read_samples(dir.path()).take(1).collect();
    assert_eq!(
        samples,
        [TrialConditions {
            elapsed_ms: 1,
            ..TrialConditions::default()
        }]
    );
}
fn fixed_config() -> ProptestConfig {
    ProptestConfig {
        rng_seed: proptest::test_runner::RngSeed::Fixed(0x7061_7274_6974_696f),
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

#[test]
fn property_runs_are_reproducible() {
    use proptest::strategy::{Strategy, ValueTree};
    use proptest::test_runner::TestRunner;
    let draw = || {
        let mut runner = TestRunner::new(fixed_config());
        (0..8)
            .map(|_| any::<u64>().new_tree(&mut runner).unwrap().current())
            .collect::<Vec<_>>()
    };
    assert_eq!(draw(), draw());
    assert!(fixed_config().failure_persistence.is_none());
}

#[test]
fn cancellation_clones_and_children_keep_context_ownership() {
    let parent = CancellationToken::new();
    let clone = parent.clone();
    let child = parent.child_token();
    let grandchild = child.child_token();
    child.cancel();
    assert!(child.is_cancelled());
    assert!(grandchild.is_cancelled());
    assert!(!parent.is_cancelled());
    let sibling = parent.child_token();
    clone.cancel();
    parent.cancel();
    assert!(parent.is_cancelled());
    assert!(sibling.is_cancelled());
    assert!(parent.child_token().is_cancelled());
    assert!(child.child_token().is_cancelled());
}

#[test]
fn cancellation_wakes_waiters_without_sleeping_or_polling() {
    let token = CancellationToken::new();
    let (started, ready) = std::sync::mpsc::channel();
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let waiting = token.clone();
            let started = started.clone();
            std::thread::spawn(move || {
                started.send(()).unwrap();
                waiting.wait();
                assert!(waiting.is_cancelled());
            })
        })
        .collect();
    for _ in &handles {
        ready.recv().unwrap();
    }
    token.cancel();
    for handle in handles {
        handle.join().unwrap();
    }
    token.wait();
}

#[test]
fn cancellation_timeout_checks_do_not_need_a_runtime() {
    let token = CancellationToken::default();
    assert!(!token.wait_timeout(std::time::Duration::ZERO));
    token.cancel();
    assert!(token.wait_timeout(std::time::Duration::ZERO));
}

#[test]
fn vocabulary_serde_uses_wire_strings() {
    assert_eq!(serde_json::to_string(&Regime::R3).unwrap(), "\"R3\"");
    assert_eq!(
        serde_json::from_str::<Signal>("\"unexpected_exit\"").unwrap(),
        Signal::UnexpectedExit
    );
    for regime in REGIMES {
        let json = serde_json::to_string(&regime).unwrap();
        assert_eq!(serde_json::from_str::<Regime>(&json).unwrap(), regime);
        assert_eq!(json, format!("\"{regime}\""));
        assert_eq!(
            regime.instance_per_core(),
            matches!(regime, Regime::R6 | Regime::R7)
        );
    }
    for name in ["alone", "together", "parked"] {
        let json = format!("\"{name}\"");
        assert_eq!(
            serde_json::from_str::<Condition>(&json).unwrap().as_str(),
            name
        );
    }
    for name in ["mprime", "ycruncher"] {
        let json = format!("\"{name}\"");
        assert_eq!(
            serde_json::from_str::<Backend>(&json).unwrap().as_str(),
            name
        );
    }
    assert_eq!(
        serde_json::from_str::<ResetKind>("\"sync_flood\"").unwrap(),
        ResetKind::SyncFlood
    );
    assert_ne!(ResetKind::Unknown, ResetKind::Watchdog);
}

#[test]
fn unknown_vocabulary_strings_are_rejected() {
    assert!(serde_json::from_str::<Regime>("\"R8\"").is_err());
    assert!(serde_json::from_str::<Regime>("\"\"").is_err());
    assert!(serde_json::from_str::<Regime>("null").is_err());
    assert!(serde_json::from_str::<Signal>("\"future\"").is_err());
    assert!(serde_json::from_str::<Backend>("\"future\"").is_err());
}

#[test]
fn bank_attribution_scope() {
    for (name, local) in [
        ("load_store", true),
        ("instruction_fetch", true),
        ("l2_cache", true),
        ("decode_unit", true),
        ("execution_unit", true),
        ("floating_point", true),
        ("l3_cache", false),
        ("unified_memory_controller", false),
        ("data_fabric", false),
        ("other", false),
        ("unknown", false),
    ] {
        let bank: BankType = serde_json::from_str(&format!("\"{name}\"")).unwrap();
        assert_eq!(bank.core_local(), local);
        assert_eq!(bank.as_str(), name);
        assert_eq!(serde_json::to_string(&bank).unwrap(), format!("\"{name}\""));
    }
}

#[test]
fn per_core_serializes_as_core_keyed_object() {
    let readings = [(core(15), 1_i64), (core(1), 2), (core(10), 3)]
        .into_iter()
        .collect::<PerCore<_>>();
    let json = serde_json::to_string(&readings).unwrap();
    assert_eq!(
        serde_json::from_str::<PerCore<i64>>(&json).unwrap(),
        readings
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&json).unwrap(),
        serde_json::json!({"1": 2, "10": 3, "15": 1})
    );
    assert_eq!(
        serde_json::to_string(&PerCore::<i64>::default()).unwrap(),
        "{}"
    );
}

#[test]
fn per_core_rejects_unsupported_cores() {
    for id in [-1, 16, i64::MAX] {
        assert!(CoreId::new(id).is_err());
    }
    for json in ["{\"16\":1}", "{\"-1\":1}", "{\"x\":1}", "null"] {
        assert!(serde_json::from_str::<PerCore<i64>>(json).is_err());
    }
    assert!(serde_json::from_str::<CoreId>("16").is_err());
    assert_eq!(serde_json::from_str::<CoreId>("15").unwrap(), core(15));
}

#[test]
fn context_difference_after_journal_round_trip() {
    let host = BiosContext {
        bios_version: "F3".to_owned(),
        board: "X870E".to_owned(),
        cpu_model: "AMD Ryzen 9 9950X".to_owned(),
        microcode: "0xb404032".to_owned(),
        boost_limit_mhz: 5700,
    };
    let recorded: BiosContext =
        serde_json::from_str(&serde_json::to_string(&host).unwrap()).unwrap();
    assert_eq!(recorded, host);
    assert_eq!(recorded.first_difference(&host), None);
    let changed = BiosContext {
        microcode: "0xb404035".to_owned(),
        ..host.clone()
    };
    let mismatch = recorded.first_difference(&changed).unwrap();
    assert_eq!(mismatch.field, ContextField::Microcode);
    assert_eq!(mismatch.field.to_string(), "microcode");
    assert_eq!(
        mismatch.to_string(),
        "microcode is 0xb404035; the session recorded 0xb404032"
    );
    let changed = BiosContext {
        boost_limit_mhz: 5800,
        ..host.clone()
    };
    assert_eq!(
        recorded.first_difference(&changed).unwrap().to_string(),
        "boost_limit_mhz is 5800; the session recorded 5700"
    );
    let both = BiosContext {
        board: "other".to_owned(),
        boost_limit_mhz: 1,
        ..host
    };
    assert_eq!(
        recorded.first_difference(&both).unwrap().field,
        ContextField::Board
    );
}

#[test]
fn topology_and_pstore_round_trip() {
    let info = CoreInfo {
        core: core(3),
        ccd: CcdId(1),
        cpus: vec![CpuId(3), CpuId(19)],
    };
    let json = serde_json::to_string(&info).unwrap();
    assert_eq!(json, r#"{"core":3,"ccd":1,"cpus":[3,19]}"#);
    assert_eq!(serde_json::from_str::<CoreInfo>(&json).unwrap(), info);
    assert!(serde_json::from_str::<CoreInfo>(r#"{"core":16,"ccd":0,"cpus":[]}"#).is_err());
    let record = PstoreRecord {
        path: "dmesg-ramoops-0".to_owned(),
        lines: vec!["panic".to_owned()],
    };
    let json = serde_json::to_string(&record).unwrap();
    assert_eq!(serde_json::from_str::<PstoreRecord>(&json).unwrap(), record);
}

#[test]
fn trial_conditions_round_trip_and_omit_empty_sensors() {
    let minimal = TrialConditions {
        elapsed_ms: 1000,
        ..TrialConditions::default()
    };
    assert_eq!(
        serde_json::to_string(&minimal).unwrap(),
        r#"{"elapsed_ms":1000}"#
    );
    assert_eq!(
        serde_json::from_str::<TrialConditions>(r#"{"elapsed_ms":1000,"future":true}"#).unwrap(),
        minimal
    );
    let full = TrialConditions {
        elapsed_ms: 5000,
        tctl_c: Some(0),
        tccd_c: Some([("Tccd1".to_owned(), 60)].into()),
        core_mhz: [(core(0), 5500)].into_iter().collect(),
        worker_cpu_ms: [(core(0), 0), (core(7), 4000)].into_iter().collect(),
        package_power_w: Some(142.5),
        pm_table: Some(PmTable {
            power_w: [1.5; 16],
            voltage_request_v: [1.25; 16],
            temperature_c: [60.0; 16],
            c0_pct: [100.0; 16],
            cc1_pct: [0.0; 16],
            cc6_pct: [0.0; 16],
        }),
    };
    let json = serde_json::to_string(&full).unwrap();
    assert_eq!(
        serde_json::from_str::<TrialConditions>(&json).unwrap(),
        full
    );
    assert!(
        serde_json::from_str::<TrialConditions>(r#"{"elapsed_ms":1,"pm_table":{"power_w":[1]}}"#)
            .is_err()
    );
}

#[test]
fn read_samples_skips_lines_that_are_not_samples() {
    let samples: Vec<_> = ReadSamples::from_reader(Cursor::new(
        b"{\"elapsed_ms\":1}\n\xff\xfe\n[]\n{\"elapsed_ms\":2}\n{\"elapsed_ms\":3}",
    ))
    .map(|sample| sample.elapsed_ms)
    .collect();
    assert_eq!(samples, [1, 2]);
}

#[test]
fn offsets_are_safe_by_construction() {
    assert!(serde_json::from_str::<TargetOffset>("-51").is_err());
    assert!(serde_json::from_str::<TargetOffset>("1").is_err());
    assert_eq!(
        serde_json::from_str::<TargetOffset>("-50").unwrap().get(),
        -50
    );
    assert_eq!(TargetOffset::ZERO.observed(), ObservedOffset(0));
    assert_eq!(TargetOffset::new(-51), Err(InvalidTarget(-51)));
}

proptest! {
    #![proptest_config(fixed_config())]
    #[test]
    fn clamp_offset_property(value in any::<i64>()) {
        let target = TargetOffset::clamped(value);
        prop_assert_eq!(target.get(), value.clamp(-50, 0));
        prop_assert_eq!(TargetOffset::new(value).is_ok(), (-50..=0).contains(&value));
        prop_assert_eq!(serde_json::from_str::<ObservedOffset>(&value.to_string()).unwrap().0, value);
    }

    #[test]
    fn per_core_round_trip_property(values in prop::collection::vec(any::<i64>(), 16), mask in any::<u16>()) {
        let mut readings = PerCore::default();
        for (index, value) in values.iter().enumerate() {
            if mask & (1 << index) != 0 {
                readings.set(CoreId::new(i64::try_from(index).unwrap()).unwrap(), *value);
            }
        }
        prop_assert_eq!(readings.len(), mask.count_ones() as usize);
        let json = serde_json::to_string(&readings).unwrap();
        prop_assert_eq!(serde_json::from_str::<PerCore<i64>>(&json).unwrap(), readings);
    }
}

#[test]
fn core_set_iterates_ascending_and_deduplicates() {
    let mut set: CoreSet = [15, 3, 3, 0, 9].into_iter().map(core).collect();
    assert_eq!(set.len(), 4);
    assert_eq!(set.iter().collect::<Vec<_>>(), [0, 3, 9, 15].map(core));
    assert!(set.contains(core(9)) && !set.contains(core(8)));
    assert!(!set.insert(core(9)) && set.insert(core(8)));
    assert!(set.remove(core(0)) && !set.remove(core(0)));
    set.extend([core(1)]);
    assert_eq!(set.into_iter().next(), Some(core(1)));
    assert!(CoreSet::default().is_empty());
    assert_eq!(CoreSet::default().iter().len(), 0);
}

#[test]
fn core_set_serializes_as_a_sorted_array() {
    let set: CoreSet = [10, 1, 15].into_iter().map(core).collect();
    let json = serde_json::to_string(&set).unwrap();
    assert_eq!(json, "[1,10,15]");
    assert_eq!(serde_json::from_str::<CoreSet>(&json).unwrap(), set);
    assert_eq!(
        serde_json::from_str::<CoreSet>("[3,1]").unwrap(),
        [1, 3].into_iter().map(core).collect()
    );
    assert_eq!(serde_json::to_string(&CoreSet::default()).unwrap(), "[]");
}

#[test]
fn core_set_rejects_duplicates_and_unsupported_cores() {
    for json in ["[1,1]", "[16]", "[-1]", "[\"1\"]", "null", "{}"] {
        assert!(serde_json::from_str::<CoreSet>(json).is_err(), "{json}");
    }
}
