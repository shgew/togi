use crate::*;
use flate2::{Compression, write::GzEncoder};
use proptest::prelude::*;
use serde::Serialize;
use std::io::{Cursor, Write};
use std::path::Path;

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
fn regime_scope() {
    for value in REGIMES {
        assert!(value.valid());
        assert_eq!(
            value.instance_per_core(),
            matches!(value, Regime::R6 | Regime::R7)
        );
        assert_eq!(Regime::from(value.as_str()), value);
    }
    for value in ["R8", ""] {
        let regime = Regime::from(value);
        assert!(!regime.valid());
        assert!(!regime.instance_per_core());
    }
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
        ("future-bank", false),
        ("", false),
    ] {
        let bank = BankType::from(name);
        assert_eq!(bank.core_local(), local);
        assert_eq!(bank.as_str(), name);
        assert_eq!(serde_json::to_string(&bank).unwrap(), format!("\"{name}\""));
    }
}

#[test]
fn vocabulary_wire_strings() {
    for name in [
        "computation_error",
        "unexpected_exit",
        "stall",
        "corrected_mce",
        "uncorrected_mce",
        "crash",
        "",
        "error",
    ] {
        assert_eq!(Signal::from(name).to_string(), name);
    }
    for name in [
        "watchdog",
        "sync_flood",
        "cpu_shutdown",
        "power_button",
        "thermal_trip",
        "power_loss",
        "unknown",
        "",
    ] {
        assert_eq!(ResetKind::from(name).to_string(), name);
    }
    for name in ["alone", "together", "parked", ""] {
        assert_eq!(Condition::from(name).to_string(), name);
    }
    for name in ["mprime", "ycruncher", "future", ""] {
        assert_eq!(Backend::from(name).to_string(), name);
    }
    assert_ne!(ResetKind::Empty, ResetKind::UnknownReason);
}

#[test]
fn core_order() {
    let cores: Vec<_> = (0..16)
        .rev()
        .map(|value| CoreInfo {
            core: core(value),
            ccd: CcdId(value / 8),
            cpus: None,
        })
        .collect();
    assert_eq!(
        order(&cores),
        [0, 8, 1, 9, 2, 10, 3, 11, 4, 12, 5, 13, 6, 14, 7, 15].map(core)
    );
    let uneven = [
        CoreInfo {
            core: core(9),
            ccd: CcdId(2),
            cpus: None,
        },
        CoreInfo {
            core: core(2),
            ccd: CcdId(-1),
            cpus: None,
        },
        CoreInfo {
            core: core(0),
            ccd: CcdId(-1),
            cpus: None,
        },
    ];
    assert_eq!(order(&uneven), [0, 9, 2].map(core));
    assert!(order(&[]).is_empty());
}

#[derive(Serialize)]
struct SampleTimes {
    elapsed_ms: i64,
    #[serde(skip_serializing_if = "PerCore::is_empty")]
    worker_cpu_ms: PerCore<i64>,
}

#[test]
fn per_core_encodes_as_map_does() {
    for (entries, expected) in [
        (vec![], ""),
        (vec![(0, 0)], ",\"worker_cpu_ms\":{\"0\":0}"),
        (vec![(7, 5)], ",\"worker_cpu_ms\":{\"7\":5}"),
        (
            vec![(2, 2000), (7, 2000)],
            ",\"worker_cpu_ms\":{\"2\":2000,\"7\":2000}",
        ),
        (
            vec![(15, 1), (1, 2), (10, 3), (0, 4), (9, 5), (2, 6)],
            ",\"worker_cpu_ms\":{\"0\":4,\"1\":2,\"10\":3,\"15\":1,\"2\":6,\"9\":5}",
        ),
        (
            (0..16).map(|value| (value, 1)).collect(),
            ",\"worker_cpu_ms\":{\"0\":1,\"1\":1,\"10\":1,\"11\":1,\"12\":1,\"13\":1,\"14\":1,\"15\":1,\"2\":1,\"3\":1,\"4\":1,\"5\":1,\"6\":1,\"7\":1,\"8\":1,\"9\":1}",
        ),
    ] {
        let readings =
            PerCore::from_readings(entries.into_iter().map(|(id, value)| (core(id), value)));
        let sample = SampleTimes {
            elapsed_ms: 1000,
            worker_cpu_ms: readings,
        };
        let json = serde_json::to_string(&sample).unwrap();
        assert_eq!(json, format!("{{\"elapsed_ms\":1000{expected}}}"));
        let decoded: TrialConditions = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.worker_cpu_ms, readings);
    }
}

#[test]
fn per_core_rejects_unsupported_cores() {
    for id in [-1, 16, i64::MAX] {
        assert!(CoreId::new(id).is_err());
    }
    for json in ["{\"16\":1}", "{\"-1\":1}", "{\"x\":1}"] {
        assert!(serde_json::from_str::<PerCore<i64>>(json).is_err());
    }
    let readings: PerCore<i64> = serde_json::from_str("{\"+01\":7,\"1\":8,\"2\":null}").unwrap();
    assert_eq!(readings.get(core(1)), Some(8));
    assert_eq!(readings.get(core(2)), Some(0));
    assert!(
        serde_json::from_str::<PerCore<i64>>("null")
            .unwrap()
            .is_empty()
    );
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
        copied.all().collect::<Vec<_>>(),
        [(core(3), 2), (core(4), 5)]
    );
}

#[test]
fn compare_context_after_journal_round_trip() {
    let host = BiosContext {
        bios_version: HostText::new(b"F3\xff\xfe".to_vec()),
        board: HostText::new(b"X870E \xc3".to_vec()),
        cpu_model: "AMD Ryzen 9 9950X".into(),
        microcode: "0xb404032".into(),
        boost_limit_mhz: 5700,
    };
    let encoded = serde_json::to_string(&host).unwrap();
    assert_eq!(
        encoded,
        "{\"bios_version\":\"F3\\ufffd\\ufffd\",\"board\":\"X870E \\ufffd\",\"cpu_model\":\"AMD Ryzen 9 9950X\",\"microcode\":\"0xb404032\",\"boost_limit_mhz\":5700}"
    );
    let recorded = serde_json::from_str(&encoded).unwrap();
    assert_eq!(
        compare_context(&recorded, &host),
        ("matches the session".to_owned(), true)
    );
    let mut changed = host.clone();
    changed.microcode = "0xb404035".into();
    assert_eq!(
        compare_context(&recorded, &changed),
        (
            "microcode is 0xb404035; the session recorded 0xb404032".to_owned(),
            false
        )
    );
    changed = host.clone();
    changed.bios_version = HostText::new(b"F3\xff".to_vec());
    assert!(!compare_context(&recorded, &changed).1);
    changed = host;
    changed.boost_limit_mhz = 5800;
    assert_eq!(
        compare_context(&recorded, &changed).0,
        "boost_limit_mhz is 5800; the session recorded 5700"
    );
}

#[test]
fn host_text_preserves_invalid_bytes_and_json_escapes() {
    let text = HostText::new(b"\xff\xfe\xe1\x80<>&\n\"\\\x01".to_vec());
    assert_eq!(
        text.normalized(),
        "\u{fffd}\u{fffd}\u{fffd}\u{fffd}<>&\n\"\\\u{1}"
    );
    assert_eq!(
        text.json(false),
        "\"\\ufffd\\ufffd\\ufffd\\ufffd<>&\\n\\\"\\\\\\u0001\""
    );
    assert_eq!(
        HostText::from("\u{fffd}\u{2028}\u{2029}\r\t\u{8}\u{c}").json(true),
        "\"\u{fffd}\\u2028\\u2029\\r\\t\\b\\f\""
    );
    assert_eq!(
        HostText::from("<>&").json(true),
        "\"\\u003c\\u003e\\u0026\""
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
        let catalog = workloads(&regime);
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
                    .any(|base_regime| workloads(base_regime).contains(base))
            );
            if regime == Regime::R4 {
                assert!([25, 50, 75].contains(&workload.duty_pct));
                assert_eq!(
                    workload.label,
                    format!("{} {}% duty", base.label, workload.duty_pct)
                );
            } else {
                assert_eq!(workload.duty_pct, 0);
            }
            assert_eq!(pick_workload(&regime, index + count * 3), workload);
        }
    }
    assert_eq!(workload_by_id("not-a-workload"), None);
    assert!(workloads(&Regime::from("R8")).is_empty());
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

#[test]
fn conditions_decode_go_nulls_duplicates_and_float_width() {
    let sample: TrialConditions = serde_json::from_str(r#"{"ELAPSED_MS":10,"elapsed_ms":null,"tctl_c":0,"tccd_c":{"0":null},"tccd_c":{"1":60},"core_mhz":{"0":0},"worker_cpu_ms":null,"package_power_w":1.25,"pm_table":{"power_w":[1.0000000596046448,null],"c0_pct":[25]},"pm_table":{"temperature_c":[60]},"unknown":true}"#).unwrap();
    assert_eq!(sample.elapsed_ms, 10);
    assert_eq!(sample.tctl_c, Some(0));
    assert_eq!(
        sample.tccd_c.unwrap().into_iter().collect::<Vec<_>>(),
        [("0".to_owned(), 0), ("1".to_owned(), 60)]
    );
    assert_eq!(sample.core_mhz.get(core(0)), Some(0));
    assert!(sample.worker_cpu_ms.is_empty());
    assert_eq!(sample.package_power_w, Some(1.25));
    let table = sample.pm_table.unwrap();
    assert_eq!(table.power_w[0].to_bits(), 1_f32.to_bits() + 1);
    assert_eq!(table.power_w[1], 0.0);
    assert_eq!(table.c0_pct[0], 25.0);
    assert_eq!(table.temperature_c[0], 60.0);
    let excess = format!("{{\"power_w\":[{},\"ignored\"]}}", ["1"; 16].join(","));
    let table: PmTable = serde_json::from_str(&excess).unwrap();
    assert_eq!(table.power_w, [1.0; 16]);
    assert!(serde_json::from_str::<PmTable>(r#"{"power_w":[1e40]}"#).is_err());
    let samples: Vec<_> = ReadSamples::from_reader(Cursor::new(
        b"null\n{}\n[]\n{\"elapsed_ms\":1.5}\n{\"tctl_c\":null,\"pm_table\":null}\n",
    ))
    .collect();
    assert_eq!(samples, vec![TrialConditions::default(); 3]);
}

#[test]
fn error_sentinels_survive_context_and_join() {
    for (kind, text) in [
        (ErrorKind::Crashed, "the simulated machine crashed"),
        (ErrorKind::BackendMissing, "backend binary missing"),
        (
            ErrorKind::Containment,
            "trial cleanup could not be confirmed",
        ),
        (
            ErrorKind::BootMissing,
            "boot missing from the system journal",
        ),
        (
            ErrorKind::CursorMissing,
            "kernel log cursor cannot be resumed",
        ),
        (ErrorKind::Cancelled, "context canceled"),
    ] {
        assert_eq!(kind.to_string(), text);
        let error = Error::Joined(vec![
            ErrorKind::Crashed.into(),
            Error::Context {
                detail: "operation".to_owned(),
                source: Box::new(kind.into()),
            },
        ]);
        assert!(error.contains(kind));
        assert_eq!(
            error.to_string(),
            format!("the simulated machine crashed\noperation: {text}")
        );
    }
    let error = Error::Other(Box::new(std::io::Error::other("failed")));
    assert!(!error.contains(ErrorKind::Containment));
    assert_eq!(error.to_string(), "failed");
}

proptest! {
    #[test]
    fn clamp_offset_property(value in any::<i64>()) {
        let target = TargetOffset::clamped(value);
        prop_assert_eq!(target.get(), value.clamp(-50,0));
        prop_assert_eq!(TargetOffset::new(value).is_ok(), (-50..=0).contains(&value));
        prop_assert_eq!(serde_json::from_str::<ObservedOffset>(&value.to_string()).unwrap().0, value);
    }

    #[test]
    fn per_core_round_trip_property(values in prop::collection::vec(any::<i64>(),16), mask in any::<u16>()) {
        let mut readings = PerCore::default();
        for (index,value) in values.iter().enumerate() {
            if mask & (1 << index) != 0 { readings.set(CoreId::new(i64::try_from(index).unwrap()).unwrap(), *value); }
        }
        prop_assert_eq!(readings.len(), mask.count_ones() as usize);
        let json = serde_json::to_string(&readings).unwrap();
        prop_assert_eq!(serde_json::from_str::<PerCore<i64>>(&json).unwrap(), readings);
        let keys: Vec<_> = readings.all().map(|(id,_)| id.to_string()).collect();
        let mut string_keys = keys.clone();
        string_keys.sort();
        let expected = string_keys.into_iter().map(|key| format!("\"{key}\":{}", readings.get(CoreId::new(key.parse().unwrap()).unwrap()).unwrap())).collect::<Vec<_>>().join(",");
        prop_assert_eq!(json, format!("{{{expected}}}"));
    }

    #[test]
    fn vocabulary_unknown_round_trip(value in any::<String>()) {
        let signal = Signal::from(value.as_str());
        prop_assert_eq!(serde_json::from_str::<Signal>(&serde_json::to_string(&signal).unwrap()).unwrap(), signal);
    }
}

#[test]
fn read_samples_preserves_go_string_replacement() {
    let input = b"{\"tccd_c\":{\"\\ud800\":1,\"\\udc00\":2,\"\\ud800\\udc00\":3,\"\\ud800\\ud800\":4,\"\\xff\":5,\"\xff\":6},\"ignored\":\"\\\\ud800\"}\n";
    let samples: Vec<_> = ReadSamples::from_reader(Cursor::new(input)).collect();
    assert!(samples.is_empty());
    let input = b"{\"tccd_c\":{\"\\ud800\":1,\"\\udc00\":2,\"\\ud800\\udc00\":3,\"\\ud800\\ud800\":4,\"\xff\":6},\"ignored\":\"\\\\ud800\"}\n";
    let samples: Vec<_> = ReadSamples::from_reader(Cursor::new(input)).collect();
    let values = samples[0].tccd_c.as_ref().unwrap();
    assert_eq!(values.get("\u{fffd}"), Some(&6));
    assert_eq!(values.get("\u{10000}"), Some(&3));
    assert_eq!(values.get("\u{fffd}\u{fffd}"), Some(&4));
}

#[test]
fn error_sentinels_survive_external_wrappers() {
    let wrapped = Error::Other(Box::new(std::io::Error::other(Error::Joined(vec![
        ErrorKind::Containment.into(),
        ErrorKind::CursorMissing.into(),
    ]))));
    assert!(wrapped.contains(ErrorKind::Containment));
    assert!(wrapped.contains(ErrorKind::CursorMissing));
    assert!(!wrapped.contains(ErrorKind::BootMissing));
}

#[test]
fn conditions_decode_unicode_field_folding_and_all_pm_arrays() {
    let sample: TrialConditions = serde_json::from_str(
        r#"{"elapſed_mſ":7,"wor\u212aer_cpu_mſ":{"0":5},"pm_table":{"POWER_W":[1],"voltage_request_v":[2],"temperature_c":[3],"c0_pct":[4],"cc1_pct":[5],"cc6_pct":[6],"ignored":true}}"#
    ).unwrap();
    assert_eq!(sample.elapsed_ms, 7);
    assert_eq!(sample.worker_cpu_ms.get(core(0)), Some(5));
    let table = sample.pm_table.unwrap();
    assert_eq!(
        [
            table.power_w[0],
            table.voltage_request_v[0],
            table.temperature_c[0],
            table.c0_pct[0],
            table.cc1_pct[0],
            table.cc6_pct[0]
        ]
        .map(f32::to_bits),
        [1.0, 2.0, 3.0, 4.0, 5.0, 6.0].map(f32::to_bits)
    );
}

#[test]
#[should_panic(expected = "attempt to calculate the remainder with a divisor of zero")]
fn picking_unknown_regime_panics() {
    let _ = pick_workload(&Regime::Empty, 0);
}

#[test]
fn topology_and_pstore_preserve_null_and_empty_collections() {
    let info = CoreInfo::default();
    assert_eq!(
        serde_json::to_string(&info).unwrap(),
        "{\"core\":0,\"ccd\":0,\"cpus\":null}"
    );
    let info = CoreInfo {
        cpus: Some(vec![]),
        ..info
    };
    assert_eq!(
        serde_json::to_string(&info).unwrap(),
        "{\"core\":0,\"ccd\":0,\"cpus\":[]}"
    );
    assert_eq!(
        serde_json::from_str::<CoreInfo>("{}").unwrap(),
        CoreInfo::default()
    );
    let record = PstoreRecord {
        path: HostText::new(b"record\xff".to_vec()),
        lines: None,
    };
    assert_eq!(
        serde_json::to_string(&record).unwrap(),
        "{\"path\":\"record\\ufffd\",\"lines\":null}"
    );
    let record = PstoreRecord {
        lines: Some(vec![]),
        ..record
    };
    assert_eq!(
        serde_json::to_string(&record).unwrap(),
        "{\"path\":\"record\\ufffd\",\"lines\":[]}"
    );
}

#[test]
fn vocabulary_json_null_is_the_empty_value() {
    assert_eq!(
        serde_json::from_str::<Regime>("null").unwrap(),
        Regime::Empty
    );
    assert_eq!(
        serde_json::from_str::<Condition>("null").unwrap(),
        Condition::Empty
    );
    assert_eq!(
        serde_json::from_str::<Signal>("null").unwrap(),
        Signal::Empty
    );
    assert_eq!(
        serde_json::from_str::<BankType>("null").unwrap(),
        BankType::Empty
    );
    assert_eq!(
        serde_json::from_str::<ResetKind>("null").unwrap(),
        ResetKind::Empty
    );
    assert_eq!(
        serde_json::from_str::<Backend>("null").unwrap(),
        Backend::Empty
    );
    assert!(serde_json::from_str::<Regime>("1").is_err());
}
