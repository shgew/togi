use crate::*;
use flate2::{Compression, write::GzEncoder};
use proptest::prelude::*;
use serde::Serialize;
use std::io::{Cursor, Write};
use std::path::Path;

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
        "{\"bios_version\":\"F3\u{fffd}\u{fffd}\",\"board\":\"X870E \u{fffd}\",\"cpu_model\":\"AMD Ryzen 9 9950X\",\"microcode\":\"0xb404032\",\"boost_limit_mhz\":5700}"
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
    // Expected bytes are Go 1.27.1 output: json.Marshal for `json(true)`, the
    // journal writer (jsonv2.Marshal with EscapeForHTML(false)) for `json(false)`.
    let text = HostText::new(b"\xff\xfe\xe1\x80<>&\n\"\\\x01".to_vec());
    assert_eq!(
        text.normalized(),
        "\u{fffd}\u{fffd}\u{fffd}\u{fffd}<>&\n\"\\\u{1}"
    );
    assert_eq!(
        text.json(false),
        "\"\u{fffd}\u{fffd}\u{fffd}\u{fffd}<>&\\n\\\"\\\\\\u0001\""
    );
    assert_eq!(
        text.json(true),
        "\"\u{fffd}\u{fffd}\u{fffd}\u{fffd}\\u003c\\u003e\\u0026\\n\\\"\\\\\\u0001\""
    );
    assert_eq!(
        HostText::from("\u{fffd}\u{2028}\u{2029}\r\t\u{8}\u{c}").json(true),
        "\"\u{fffd}\\u2028\\u2029\\r\\t\\b\\f\""
    );
    assert_eq!(
        HostText::from("<>&").json(true),
        "\"\\u003c\\u003e\\u0026\""
    );
    for (bytes, expected) in [
        (&b"x\xffy\xef\xbf\xbd"[..], "\"x\u{fffd}y\u{fffd}\""),
        (b"\xe2\x82", "\"\u{fffd}\u{fffd}\""),
        (b"\xed\xa0\x80", "\"\u{fffd}\u{fffd}\u{fffd}\""),
    ] {
        assert_eq!(HostText::new(bytes).json(true), expected);
        assert_eq!(HostText::new(bytes).json(false), expected);
    }
    assert_eq!(
        HostText::from("\u{8}\u{c}\u{7f}\u{1}\u{1f}\u{2028}\u{2029}<>&\"\\\n\r\t").json(true),
        "\"\\b\\f\u{7f}\\u0001\\u001f\\u2028\\u2029\\u003c\\u003e\\u0026\\\"\\\\\\n\\r\\t\""
    );
    assert_eq!(
        HostText::from("\u{8}\u{c}\u{7f}\u{1}\u{1f}\u{2028}\u{2029}<>&\"\\\n\r\t").json(false),
        "\"\\b\\f\u{7f}\\u0001\\u001f\\u2028\\u2029<>&\\\"\\\\\\n\\r\\t\""
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

proptest! {
    #![proptest_config(fixed_config())]
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
        "{\"path\":\"record\u{fffd}\",\"lines\":null}"
    );
    let record = PstoreRecord {
        lines: Some(vec![]),
        ..record
    };
    assert_eq!(
        serde_json::to_string(&record).unwrap(),
        "{\"path\":\"record\u{fffd}\",\"lines\":[]}"
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

fn nested(depth: usize) -> String {
    format!("{}{}", "[".repeat(depth), "]".repeat(depth))
}

fn go_text(bytes: &[u8]) -> String {
    json_text(bytes).unwrap().into_owned()
}

fn floats(prefix: &[f32]) -> [f32; 16] {
    let mut values = [0.0; 16];
    values[..prefix.len()].copy_from_slice(prefix);
    values
}

fn cpus(values: &[i64]) -> Vec<CpuId> {
    values.iter().copied().map(CpuId).collect()
}

fn lines(values: &[&str]) -> Vec<HostText> {
    values.iter().copied().map(HostText::from).collect()
}

fn assert_marshals_as_go<T>(value: &str, json: &str)
where
    T: Serialize + for<'a> From<&'a str>,
{
    assert_eq!(serde_json::to_string(&T::from(value)).unwrap(), json);
}

#[test]
fn vocabulary_json_matches_go_marshal() {
    // Go 1.27.1: json.Marshal of a string-typed value, and the journal writer
    // (jsonv2.Marshal with EscapeForHTML(false)) for the second column.
    for (value, marshal, journal) in [
        ("R6", "\"R6\"", "\"R6\""),
        ("", "\"\"", "\"\""),
        (
            "R\u{2028}\u{2029}",
            "\"R\\u2028\\u2029\"",
            "\"R\\u2028\\u2029\"",
        ),
        ("a<b>&c", "\"a\\u003cb\\u003e\\u0026c\"", "\"a<b>&c\""),
        (
            "\u{8}\u{c}\u{7f}\u{1}\u{1f}\u{2028}\u{2029}<>&\"\\\n\r\t",
            "\"\\b\\f\u{7f}\\u0001\\u001f\\u2028\\u2029\\u003c\\u003e\\u0026\\\"\\\\\\n\\r\\t\"",
            "\"\\b\\f\u{7f}\\u0001\\u001f\\u2028\\u2029<>&\\\"\\\\\\n\\r\\t\"",
        ),
    ] {
        assert_marshals_as_go::<Regime>(value, marshal);
        assert_marshals_as_go::<Condition>(value, marshal);
        assert_marshals_as_go::<Signal>(value, marshal);
        assert_marshals_as_go::<BankType>(value, marshal);
        assert_marshals_as_go::<ResetKind>(value, marshal);
        assert_marshals_as_go::<Backend>(value, marshal);
        assert_eq!(Regime::from(value).json(true), marshal);
        assert_eq!(Regime::from(value).json(false), journal);
        assert_eq!(HostText::from(value).json(true), marshal);
        assert_eq!(HostText::from(value).json(false), journal);
    }
}

#[test]
fn vocabulary_has_one_value_per_string() {
    assert_eq!(Regime::from("R6"), Regime::R6);
    assert_eq!(Regime::from(String::from("R6")), Regime::R6);
    assert_eq!(Regime::from(""), Regime::Empty);
    assert_eq!(Regime::from(String::new()), Regime::Empty);
    assert!(Regime::from("R6").valid());
    assert!(Regime::from("R6").instance_per_core());
    assert_eq!(
        pick_workload(&Regime::from(String::from("R6")), 0),
        pick_workload(&Regime::R6, 0)
    );
    assert!(BankType::from("load_store").core_local());
    assert_eq!(Backend::from("mprime"), Backend::Mprime);
    assert_eq!(Condition::from(""), Condition::Empty);
    assert_eq!(Signal::from("crash"), Signal::Crash);
    assert_eq!(ResetKind::from("unknown"), ResetKind::UnknownReason);
    assert_eq!(
        serde_json::from_str::<Regime>("\"R6\"").unwrap(),
        Regime::R6
    );
    assert_eq!(
        serde_json::from_str::<Regime>("\"\"").unwrap(),
        Regime::Empty
    );
    for value in ["R8", "r6", " R6", "R6 ", "R\u{2028}", "<>&\u{fffd}"] {
        let regime = Regime::from(value);
        assert!(matches!(&regime, Regime::Unknown(unknown) if unknown.as_str() == value));
        assert_eq!(regime.as_str(), value);
        assert_eq!(
            serde_json::from_str::<Regime>(&serde_json::to_string(&regime).unwrap()).unwrap(),
            regime
        );
    }
}

#[test]
fn json_text_gives_go_replacements_for_invalid_bytes_and_surrogates() {
    // Go 1.27.1 json.Unmarshal of each input.
    let text = go_text(b"\"a\xffb\xe2\x82c\"");
    assert_eq!(
        serde_json::from_str::<HostText>(&text)
            .unwrap()
            .normalized(),
        "a\u{fffd}b\u{fffd}\u{fffd}c"
    );
    for (input, expected) in [
        (
            r#""\ud800x\udc00\ud800\udc00\ud800\ud800""#,
            "\u{fffd}x\u{fffd}\u{10000}\u{fffd}\u{fffd}",
        ),
        (r#""\udc00\ud800""#, "\u{fffd}\u{fffd}"),
        (r#""\ud800\u0041""#, "\u{fffd}A"),
        (r#""R6\ud800""#, "R6\u{fffd}"),
    ] {
        let text = go_text(input.as_bytes());
        assert_eq!(
            serde_json::from_str::<HostText>(&text)
                .unwrap()
                .normalized(),
            expected
        );
        assert_eq!(
            serde_json::from_str::<Signal>(&text).unwrap().as_str(),
            expected
        );
    }
    assert_eq!(
        serde_json::from_str::<Regime>(&go_text(b"\"R\xff\""))
            .unwrap()
            .as_str(),
        "R\u{fffd}"
    );
    let context: BiosContext = serde_json::from_str(&go_text(
        b"{\"bios_version\":\"F3\xff\",\"board\":\"\\ud800x\",\"cpu_model\":\"a\xe2\x82b\"}",
    ))
    .unwrap();
    assert_eq!(context.bios_version.normalized(), "F3\u{fffd}");
    assert_eq!(context.board.normalized(), "\u{fffd}x");
    assert_eq!(context.cpu_model.normalized(), "a\u{fffd}\u{fffd}b");
    let record: PstoreRecord = serde_json::from_str(&go_text(
        b"{\"path\":\"p\xff\",\"lines\":[\"\\udc00\",\"\xe1\x80\"]}",
    ))
    .unwrap();
    assert_eq!(record.path.normalized(), "p\u{fffd}");
    assert_eq!(record.lines, Some(lines(&["\u{fffd}", "\u{fffd}\u{fffd}"])));
    let info: CoreInfo =
        serde_json::from_str(&go_text(b"{\"cpus\":[1],\"\xff\":2,\"core\":3}")).unwrap();
    assert_eq!((info.core, info.cpus), (core(3), Some(cpus(&[1]))));
    let samples: Vec<_> = ReadSamples::from_reader(Cursor::new(
        &b"{\"elapsed_ms\":1,\"tccd_c\":{\"\\ud800\":1,\"\xff\":2,\"\\ud800\\udc00\":3}}\n"[..],
    ))
    .collect();
    let readings = samples[0].tccd_c.as_ref().unwrap();
    assert_eq!(
        readings
            .iter()
            .map(|(k, v)| (k.as_str(), *v))
            .collect::<Vec<_>>(),
        [("\u{fffd}", 2), ("\u{10000}", 3)]
    );
}

#[test]
fn json_text_enforces_go_nesting_limit_everywhere() {
    // Go 1.27.1: depth 10000 decodes, 10001 fails with "exceeded max depth",
    // ignored members included.
    assert!(json_text(nested(10_000).as_bytes()).is_ok());
    assert_eq!(json_text(nested(10_001).as_bytes()), Err(ExceededMaxDepth));
    let core_line = |depth: usize| format!("{{\"core\":3,\"x\":{}}}", nested(depth - 1));
    assert!(json_text(core_line(10_000).as_bytes()).is_ok());
    assert_eq!(
        json_text(core_line(10_001).as_bytes()),
        Err(ExceededMaxDepth)
    );
    let brackets_in_strings = format!(
        "{{\"a\\\"{}\":\"{}\"}}",
        "[".repeat(20_000),
        "{".repeat(20_000)
    );
    assert!(json_text(brackets_in_strings.as_bytes()).is_ok());
    assert_eq!(ExceededMaxDepth.to_string(), "exceeded max depth");

    let line = |depth: usize| format!("{{\"elapsed_ms\":1,\"ignored\":{}}}\n", nested(depth - 1));
    let count = |text: String| ReadSamples::from_reader(Cursor::new(text.into_bytes())).count();
    assert_eq!(count(line(10_000)), 1);
    assert_eq!(count(line(10_001)), 0);
    assert_eq!(count(line(200)), 1);
    let samples: Vec<_> = ReadSamples::from_reader(Cursor::new(
        format!("{}{{\"elapsed_ms\":2}}\n", line(10_001)).into_bytes(),
    ))
    .collect();
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].elapsed_ms, 2);
}

#[test]
fn duplicate_pm_arrays_keep_readings_under_null_elements() {
    // Go 1.27.1 json.Unmarshal into PMTable, which decodes arrays in place.
    let table = |json: &str| {
        serde_json::from_str::<TrialConditions>(json)
            .unwrap()
            .pm_table
            .unwrap()
    };
    assert_eq!(
        table(r#"{"pm_table":{"power_w":[7,8,9],"power_w":[null,2]}}"#).power_w,
        floats(&[7.0, 2.0])
    );
    let across = table(r#"{"pm_table":{"cc6_pct":[7,8,9]},"pm_table":{"cc6_pct":[null,2]}}"#);
    assert_eq!(across.cc6_pct, floats(&[7.0, 2.0]));
    assert_eq!(across.power_w, floats(&[]));
    let long: Vec<_> = (1..=17).map(|value| value.to_string()).collect();
    let long = table(&format!(
        "{{\"pm_table\":{{\"power_w\":[{}]}}}}",
        long.join(",")
    ));
    assert_eq!(
        long.power_w,
        floats(&(1..=16_u8).map(f32::from).collect::<Vec<_>>())
    );
    assert_eq!(
        table(r#"{"pm_table":{"power_w":[1],"power_w":null}}"#).power_w,
        floats(&[1.0])
    );
    for name in [
        "power_w",
        "voltage_request_v",
        "temperature_c",
        "c0_pct",
        "cc1_pct",
        "cc6_pct",
    ] {
        let json = format!("{{\"pm_table\":{{\"{name}\":[5,6,7],\"{name}\":[null,1]}}}}");
        let table = table(&json);
        let all = [
            table.power_w,
            table.voltage_request_v,
            table.temperature_c,
            table.c0_pct,
            table.cc1_pct,
            table.cc6_pct,
        ];
        let touched: Vec<_> = all
            .iter()
            .filter(|values| values.iter().any(|value| *value != 0.0))
            .collect();
        assert_eq!(touched, [&floats(&[5.0, 1.0])], "{name}");
    }
    let standalone: PmTable =
        serde_json::from_str(r#"{"POWER_W":[7,8,9],"power_w":[null,2]}"#).unwrap();
    assert_eq!(standalone.power_w, floats(&[7.0, 2.0]));
    let lines: Vec<_> = ReadSamples::from_reader(Cursor::new(
        &b"{\"pm_table\":{\"power_w\":[7,8,9],\"power_w\":[null,2]}}\n"[..],
    ))
    .collect();
    assert_eq!(lines[0].pm_table.unwrap().power_w, floats(&[7.0, 2.0]));
}

#[test]
fn core_info_decodes_as_go_structs() {
    // Expectations are Go 1.27.1 json.Unmarshal into machine.CoreInfo.
    let decode = |json: &str| serde_json::from_str::<CoreInfo>(json).unwrap();
    let info = decode(r#"{"Core":4,"CCD":-1,"CPUS":[1]}"#);
    assert_eq!(
        (info.core, info.ccd, info.cpus),
        (core(4), CcdId(-1), Some(cpus(&[1])))
    );
    assert_eq!(
        decode(r#"{"core":null,"ccd":null,"cpus":null}"#),
        CoreInfo::default()
    );
    assert_eq!(decode("null"), CoreInfo::default());
    let info = decode(r#"{"core":2,"core":null,"ccd":1,"ccd":3,"cpus":[1,2],"cpus":[null]}"#);
    assert_eq!(
        (info.core, info.ccd, info.cpus),
        (core(2), CcdId(3), Some(cpus(&[1])))
    );
    for (json, expected) in [
        (
            r#"{"cpus":[5,6],"cpus":[null,7,null]}"#,
            Some(cpus(&[5, 7, 0])),
        ),
        (
            r#"{"cpus":[1,2,3],"cpus":[9],"cpus":[null,null,null]}"#,
            Some(cpus(&[9, 2, 3])),
        ),
        (
            r#"{"cpus":[1,2,3,4,5],"cpus":[9],"cpus":[null,null,null,null,null,null,null]}"#,
            Some(cpus(&[9, 2, 3, 4, 5, 0, 0])),
        ),
        (
            r#"{"cpus":[1,2,3,4,5],"cpus":[],"cpus":[null,null,null,null,null,null,null]}"#,
            Some(cpus(&[0; 7])),
        ),
        (
            r#"{"cpus":[1,2,3],"cpus":null,"cpus":[null,null]}"#,
            Some(cpus(&[0, 0])),
        ),
        (
            r#"{"cpus":[1,2,3,4,5,6,7,8,9],"cpus":[0],"cpus":[null,null,null,null,null,null,null,null,null,null]}"#,
            Some(cpus(&[0, 2, 3, 4, 5, 6, 7, 8, 9, 0])),
        ),
        (r#"{"cpus":[null]}"#, Some(cpus(&[0]))),
        (r#"{"cpus":[1],"cpus":null}"#, None),
        (r#"{"cpus":[]}"#, Some(cpus(&[]))),
        ("{}", None),
    ] {
        assert_eq!(decode(json).cpus, expected, "{json}");
    }
    let info =
        decode(r#"{"cOrE":5,"ccd":2,"cpu\u017f":[3],"unknown":{"a":[1]},"c_o_r_e":9,"CO-RE":8}"#);
    assert_eq!(
        (info.core, info.ccd, info.cpus),
        (core(5), CcdId(2), Some(cpus(&[3])))
    );
    assert_eq!(decode(r#"{"\u212aey":1,"core":7}"#).core, core(7));
    for json in [
        r#"{"core":1.5}"#,
        r#"{"core":16}"#,
        r#"{"core":-1}"#,
        r#"{"cpus":{}}"#,
        "[]",
        r#"{"cpus":["a"]}"#,
    ] {
        assert!(serde_json::from_str::<CoreInfo>(json).is_err(), "{json}");
    }
    assert_eq!(serde_json::from_str::<CpuId>("null").unwrap(), CpuId(0));
    assert_eq!(serde_json::from_str::<CcdId>("null").unwrap(), CcdId(0));
    assert_eq!(serde_json::from_str::<CoreId>("null").unwrap(), core(0));
    assert!(serde_json::from_str::<CoreId>("16").is_err());
}

#[test]
fn bios_context_decodes_as_go_structs() {
    // Expectations are Go 1.27.1 json.Unmarshal into machine.BIOSContext.
    let decode = |json: &str| serde_json::from_str::<BiosContext>(json).unwrap();
    assert_eq!(
        decode(r#"{"BIOS_VERSION":"F3"}"#),
        BiosContext {
            bios_version: "F3".into(),
            ..BiosContext::default()
        }
    );
    assert_eq!(
        decode(r#"{"bios_version":"F3","bios_version":null,"board":null,"boost_limit_mhz":null}"#),
        BiosContext {
            bios_version: "F3".into(),
            ..BiosContext::default()
        }
    );
    assert_eq!(
        decode(
            r#"{"bios_version":"a","Bios_Version":"b","MICROCODE":"m","Boo\u017ft_limit_mhz":5700,"cpu_model":"c","other":1}"#
        ),
        BiosContext {
            bios_version: "b".into(),
            board: HostText::default(),
            cpu_model: "c".into(),
            microcode: "m".into(),
            boost_limit_mhz: 5700,
        }
    );
    assert_eq!(
        decode(r#"{"biosversion":"x","bios-version":"y"}"#),
        BiosContext::default()
    );
    assert_eq!(decode("null"), BiosContext::default());
    for json in [
        r#"{"boost_limit_mhz":"1"}"#,
        r#"{"board":1}"#,
        r#"{"board":"\x"}"#,
        "[]",
    ] {
        assert!(serde_json::from_str::<BiosContext>(json).is_err(), "{json}");
    }
}

#[test]
fn pstore_record_decodes_as_go_structs() {
    // Expectations are Go 1.27.1 json.Unmarshal into machine.PstoreRecord.
    let decode = |json: &str| serde_json::from_str::<PstoreRecord>(json).unwrap();
    let record = decode(r#"{"PATH":"p","LINES":["x"]}"#);
    assert_eq!(
        (record.path, record.lines),
        ("p".into(), Some(lines(&["x"])))
    );
    let record = decode(r#"{"path":"p","path":null,"lines":["a","b"],"lines":null}"#);
    assert_eq!((record.path, record.lines), ("p".into(), None));
    assert_eq!(
        decode(r#"{"lines":["a","b"],"lines":[null]}"#).lines,
        Some(lines(&["a"]))
    );
    assert_eq!(
        decode(r#"{"lines":["a","b","c"],"lines":["z"],"lines":[null,null,null]}"#).lines,
        Some(lines(&["z", "b", "c"]))
    );
    assert_eq!(
        decode(r#"{"lines":["a"],"lines":[]}"#).lines,
        Some(lines(&[]))
    );
    let record = decode(r#"{"pAtH":"q","line\u017f":["z"],"extra":true}"#);
    assert_eq!(
        (record.path, record.lines),
        ("q".into(), Some(lines(&["z"])))
    );
    assert_eq!(decode("null"), PstoreRecord::default());
    assert!(serde_json::from_str::<PstoreRecord>(r#"{"lines":"x"}"#).is_err());
}

/// A map that serializes its entries in the order given, as Go's sorted map keys arrive.
struct Entries<K, V>(Vec<(K, V)>);

impl<K: Serialize, V: Serialize> Serialize for Entries<K, V> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.0.iter().map(|(key, value)| (key, value)))
    }
}

#[test]
fn vocabulary_map_keys_encode_as_go() {
    // Go 1.27.1 json.Marshal of map[machine.Regime]int, map[machine.Signal]float64 (integral values),
    // map[machine.Regime]map[machine.Signal]float64 and the other vocabulary types as keys.
    fn encoded<K: Serialize, V: Serialize>(
        entries: Vec<(K, V)>,
    ) -> Result<String, serde_json::Error> {
        serde_json::to_string(&Entries(entries))
    }
    assert_eq!(encoded(vec![(Regime::R1, 1)]).unwrap(), "{\"R1\":1}");
    assert_eq!(
        encoded(
            ["", "R1", "R7", "future"]
                .map(Regime::from)
                .into_iter()
                .zip(1..)
                .collect()
        )
        .unwrap(),
        "{\"\":1,\"R1\":2,\"R7\":3,\"future\":4}"
    );
    assert_eq!(
        encoded(vec![(Signal::Crash, 3), (Signal::Stall, 1)]).unwrap(),
        "{\"crash\":3,\"stall\":1}"
    );
    assert_eq!(
        encoded(vec![(Regime::R7, Entries(vec![(Signal::Crash, 2)]))]).unwrap(),
        "{\"R7\":{\"crash\":2}}"
    );
    assert_eq!(
        encoded(vec![
            (ResetKind::from("future"), 2),
            (ResetKind::Watchdog, 1)
        ])
        .unwrap(),
        "{\"future\":2,\"watchdog\":1}"
    );
    assert_eq!(
        encoded(vec![(BankType::Empty, 2), (BankType::from("future"), 1)]).unwrap(),
        "{\"\":2,\"future\":1}"
    );
    assert_eq!(
        encoded(vec![(Condition::Empty, 2), (Condition::Alone, 1)]).unwrap(),
        "{\"\":2,\"alone\":1}"
    );
    assert_eq!(
        encoded(vec![(Backend::Mprime, 1), (Backend::from("other"), 2)]).unwrap(),
        "{\"mprime\":1,\"other\":2}"
    );
    // A key serde cannot write as Go does is an error; as a value it keeps Go's bytes.
    for special in ["a<b", "a\u{2028}"] {
        assert!(
            encoded(vec![(Regime::from(special), 1)]).is_err(),
            "{special}"
        );
    }
    assert_eq!(
        serde_json::to_string(&Regime::from("a<b")).unwrap(),
        "\"a\\u003cb\""
    );
    assert_eq!(
        serde_json::to_string(&[Regime::from("a\u{2028}")]).unwrap(),
        "[\"a\\u2028\"]"
    );
}
