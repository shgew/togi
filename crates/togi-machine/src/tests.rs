use crate::*;
use flate2::{Compression, read::MultiGzDecoder, write::GzEncoder};
use proptest::prelude::*;
use std::io::{Cursor, Write};
use test_case::test_case;

mod evidence;
mod load;
mod object_only;
mod roles;
mod seams;

fn core(value: i64) -> CoreId {
    CoreId::new(value).unwrap()
}

/// Sixteen cores on two CCDs, each with an SMT sibling numbered 16 above it.
fn topology() -> Vec<CoreInfo> {
    (0..16_u8)
        .map(|value| CoreInfo {
            core: core(i64::from(value)),
            ccd: CcdId::try_from(value / 8).unwrap(),
            cpus: vec![
                CpuId::from(u16::from(value)),
                CpuId::from(u16::from(value) + 16),
            ],
        })
        .collect()
}

#[test]
fn clamp_offset_safety() {
    for value in -100..=100 {
        let offset = TargetOffset::clamped(value);
        assert_eq!(offset.get(), value.clamp(-50, 0), "{value}");
        assert_eq!(
            TargetOffset::new(value).is_ok(),
            (-50..=0).contains(&value),
            "{value}"
        );
    }
}

#[test_case(i64::MIN; "i64 minimum")]
#[test_case(-100; "far below the floor")]
#[test_case(-51; "just below the floor")]
#[test_case(-50; "floor")]
#[test_case(0; "zero")]
#[test_case(10; "positive")]
#[test_case(i64::MAX; "i64 maximum")]
fn offsets_json_preserves_raw_readbacks(value: i64) {
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

#[test]
fn an_observed_readback_differs_from_the_zero_target() {
    assert_ne!(ObservedOffset(10), TargetOffset::ZERO.observed());
}

#[test]
fn core_order() {
    let cores: Vec<_> = (0..16_u8)
        .rev()
        .map(|value| CoreInfo {
            core: core(i64::from(value)),
            ccd: CcdId::try_from(value / 8).unwrap(),
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
            ccd: CcdId::new(3).unwrap(),
            cpus: Vec::new(),
        },
        CoreInfo {
            core: core(2),
            ccd: CcdId::new(1).unwrap(),
            cpus: Vec::new(),
        },
        CoreInfo {
            core: core(0),
            ccd: CcdId::new(1).unwrap(),
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

#[test_case(Regime::R1, 3, 1, &[Regime::R1]; "R1")]
#[test_case(Regime::R2, 3, 1, &[Regime::R2]; "R2")]
#[test_case(Regime::R3, 6, 1, &[Regime::R1, Regime::R2]; "R3")]
#[test_case(Regime::R4, 9, 1, &[Regime::R1]; "R4")]
#[test_case(Regime::R5, 6, 2, &[Regime::R1, Regime::R2]; "R5")]
#[test_case(Regime::R6, 3, 1, &[Regime::R1]; "R6")]
#[test_case(Regime::R7, 3, 1, &[Regime::R2]; "R7")]
#[test_case(Regime::R8, 6, 1, &[Regime::R1]; "R8")]
#[test_case(Regime::R9, 3, 1, &[Regime::R1]; "R9")]
fn workload_regime_contracts(regime: Regime, count: usize, threads: u8, bases: &[Regime]) {
    let catalog = workloads(regime);
    assert_eq!(catalog.len(), count);
    let mut ids = std::collections::BTreeSet::new();
    for (index, workload) in catalog.iter().enumerate() {
        assert!(
            ids.insert(workload.id),
            "{regime}, index {index}: {workload:?}"
        );
        assert_eq!(
            workload.threads, threads,
            "{regime}, index {index}: {workload:?}"
        );
        assert_eq!(
            workload_by_id(workload.id),
            Some(workload),
            "{regime}, index {index}: {workload:?}"
        );
        let base = workload_by_id(workload.base).unwrap();
        assert_eq!(
            base.id, workload.base,
            "{regime}, index {index}: {workload:?}"
        );
        assert_eq!(base.id, base.base, "{regime}, index {index}: {workload:?}");
        assert_eq!(
            base.backend, workload.backend,
            "{regime}, index {index}: {workload:?}"
        );
        assert!(
            bases
                .iter()
                .any(|base_regime| workloads(*base_regime).contains(base)),
            "{regime}, index {index}: {workload:?}"
        );
        if regime == Regime::R4 {
            assert!(
                [25, 50, 75].contains(&workload.duty_pct.get()),
                "{regime}, index {index}: {workload:?}"
            );
            assert_eq!(
                workload.label,
                format!("{} {}% duty", base.label, workload.duty_pct),
                "{regime}, index {index}: {workload:?}"
            );
        } else {
            assert_eq!(
                workload.duty_pct.get(),
                0,
                "{regime}, index {index}: {workload:?}"
            );
        }
        assert_eq!(
            pick_workload(regime, index + count * 3),
            workload,
            "{regime}, index {index}: {workload:?}"
        );
    }
}

#[test]
fn workload_ids_name_every_catalog_entry() {
    let mut seen = std::collections::BTreeSet::new();
    for regime in ALL_REGIMES {
        for (index, workload) in workloads(regime).iter().enumerate() {
            let id = workload.id();
            assert_eq!(
                id,
                WorkloadId::new(regime, index).unwrap(),
                "{regime}, index {index}: {workload:?}"
            );
            assert_eq!(id.regime(), regime, "{regime}, index {index}: {workload:?}");
            assert_eq!(
                id.workload(),
                workload,
                "{regime}, index {index}: {workload:?}"
            );
            assert_eq!(
                id.as_str(),
                workload.id,
                "{regime}, index {index}: {workload:?}"
            );
            assert_eq!(
                id.to_string(),
                workload.id,
                "{regime}, index {index}: {workload:?}"
            );
            assert_eq!(
                workload.id.parse::<WorkloadId>().unwrap(),
                id,
                "{regime}, index {index}: {workload:?}"
            );
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(
                json,
                format!("\"{}\"", workload.id),
                "{regime}, index {index}: {workload:?}"
            );
            assert_eq!(
                serde_json::from_str::<WorkloadId>(&json).unwrap(),
                id,
                "{regime}, index {index}: {workload:?}"
            );
            assert!(seen.insert(id), "{regime}, index {index}: {workload:?}");
        }
    }
    assert_eq!(seen.len(), 42);
    assert!(seen.iter().zip(seen.iter().skip(1)).all(|(a, b)| a < b));
}

#[test]
fn unknown_workload_ids_are_rejected() {
    assert_eq!(workload_by_id("not-a-workload"), None);
    assert_eq!(WorkloadId::by_id("not-a-workload"), None);
    assert!("".parse::<WorkloadId>().is_err());
    assert!(serde_json::from_str::<WorkloadId>("\"not-a-workload\"").is_err());
    assert!(serde_json::from_str::<WorkloadId>("3").is_err());
    assert_eq!(WorkloadId::new(Regime::R1, 3), None);
}

#[test]
fn nested_vocabulary_types_refuse_unknown_fields() {
    assert!(serde_json::from_str::<CoreInfo>(r#"{"core":0,"ccd":0,"cpus":[0],"x":1}"#).is_err());
    assert!(serde_json::from_str::<PstoreRecord>(r#"{"path":"p","lines":[],"x":1}"#).is_err());
    assert!(serde_json::from_str::<LoadSchedule>(r#"{"kind":"random","seed":1,"x":1}"#).is_err());
    assert!(serde_json::from_str::<LoadSchedule>(r#"{"kind":"duty","pct":50,"x":1}"#).is_err());
    assert!(serde_json::from_str::<LoadSchedule>(r#"{"kind":"random","seed":1}"#).is_ok());
}

fn gzip(content: &str) -> Vec<u8> {
    let mut writer = GzEncoder::new(Vec::new(), Compression::default());
    writer.write_all(content.as_bytes()).unwrap();
    writer.finish().unwrap()
}

#[test_case("{\"elapsed_ms\":1000}\n{\"elapsed_ms\":2000}\n", &[1000, 2000]; "complete lines")]
#[test_case("{\"elapsed_ms\":1000}\n{\"elapsed_ms\":20", &[1000]; "torn tail")]
#[test_case("", &[]; "empty")]
fn read_samples_forms(content: &str, want: &[i64]) {
    let plain: Vec<_> = ReadSamples::from_reader(content.as_bytes())
        .collect::<Result<_, _>>()
        .unwrap();
    let compressed: Vec<_> =
        ReadSamples::from_reader(MultiGzDecoder::new(Cursor::new(gzip(content))))
            .collect::<Result<_, _>>()
            .unwrap();
    let expected: Vec<_> = want
        .iter()
        .map(|&elapsed_ms| TrialConditions {
            elapsed_ms,
            ..TrialConditions::default()
        })
        .collect();
    assert_eq!(plain, expected);
    assert_eq!(compressed, expected);
}

#[test]
fn read_samples_rejects_invalid_gzip() {
    let mut samples = ReadSamples::from_reader(MultiGzDecoder::new(Cursor::new(b"not gzip")));
    let next = samples.next();
    assert!(matches!(next, Some(Err(SampleError::Read(_)))), "{next:?}");
    assert!(samples.next().is_none());
}

struct SampleReadFailure;

impl std::io::Read for SampleReadFailure {
    fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
        Err(std::io::Error::other("injected samples read failure"))
    }
}

#[test]
fn read_samples_read_error_is_not_clean_eof() {
    let prefix = b"{\"elapsed_ms\":1000}\n";
    let broken = std::io::Read::chain(Cursor::new(prefix), SampleReadFailure);
    let mut samples = ReadSamples::from_reader(broken);
    assert_eq!(samples.next().unwrap().unwrap().elapsed_ms, 1000);
    let error = samples.next().unwrap().unwrap_err();
    assert!(matches!(error, SampleError::Read(_)), "{error:?}");
    assert_eq!(
        crate::error_chain(&error),
        "read conditions samples (including gzip decoding): injected samples read failure"
    );
    assert!(samples.next().is_none());
}

#[test]
fn read_samples_gzip_error_is_not_clean_eof() {
    let prefix = "{\"elapsed_ms\":1000}\n";
    let mut broken = gzip(prefix);
    broken.extend_from_slice(b"not a second gzip member");
    let mut samples = ReadSamples::from_reader(MultiGzDecoder::new(Cursor::new(broken)));
    assert_eq!(samples.next().unwrap().unwrap().elapsed_ms, 1000);
    let next = samples.next();
    assert!(matches!(next, Some(Err(SampleError::Read(_)))), "{next:?}");
    assert!(samples.next().is_none());
}

#[test]
fn read_samples_complete_decode_error_is_not_a_missing_sample() {
    let mut samples = ReadSamples::from_reader(Cursor::new(
        b"{\"elapsed_ms\":1000}\nnot json\n{\"elapsed_ms\":2000}\n",
    ));
    assert_eq!(samples.next().unwrap().unwrap().elapsed_ms, 1000);
    let next = samples.next();
    assert!(
        matches!(next, Some(Err(SampleError::Decode(_)))),
        "{next:?}"
    );
    assert!(samples.next().is_none());
}

#[test]
fn read_samples_stops_early() {
    let samples: Vec<_> = ReadSamples::from_reader(MultiGzDecoder::new(Cursor::new(gzip(
        "{\"elapsed_ms\":1}\n{\"elapsed_ms\":2}\n",
    ))))
    .take(1)
    .collect::<Result<_, _>>()
    .unwrap();
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
    for regime in ALL_REGIMES {
        let json = serde_json::to_string(&regime).unwrap();
        assert_eq!(
            serde_json::from_str::<Regime>(&json).unwrap(),
            regime,
            "{regime}"
        );
        assert_eq!(json, format!("\"{regime}\""), "{regime}");
        assert_eq!(
            regime.instance_per_core(),
            matches!(regime, Regime::R6 | Regime::R7 | Regime::R8),
            "{regime}"
        );
    }
    assert_eq!(
        serde_json::from_str::<ResetKind>("\"sync_flood\"").unwrap(),
        ResetKind::SyncFlood
    );
    assert_ne!(ResetKind::Unknown, ResetKind::Watchdog);
}

#[test_case("alone")]
#[test_case("together")]
#[test_case("parked")]
fn conditions_read_their_wire_strings(name: &str) {
    let json = format!("\"{name}\"");
    assert_eq!(
        serde_json::from_str::<Condition>(&json).unwrap().as_str(),
        name
    );
}

#[test_case("mprime")]
#[test_case("ycruncher")]
fn backends_read_their_wire_strings(name: &str) {
    let json = format!("\"{name}\"");
    assert_eq!(
        serde_json::from_str::<Backend>(&json).unwrap().as_str(),
        name
    );
}

#[test]
fn unknown_vocabulary_strings_are_rejected() {
    assert!(serde_json::from_str::<Regime>("\"R10\"").is_err());
    assert!(serde_json::from_str::<Regime>("\"\"").is_err());
    assert!(serde_json::from_str::<Regime>("null").is_err());
    assert!(serde_json::from_str::<Signal>("\"future\"").is_err());
    assert!(serde_json::from_str::<Backend>("\"future\"").is_err());
}

#[test_case("load_store", true)]
#[test_case("instruction_fetch", true)]
#[test_case("l2_cache", true)]
#[test_case("decode_unit", true)]
#[test_case("execution_unit", true)]
#[test_case("floating_point", true)]
#[test_case("l3_cache", false)]
#[test_case("unified_memory_controller", false)]
#[test_case("data_fabric", false)]
#[test_case("other", false)]
#[test_case("unknown", false)]
fn bank_attribution_scope(name: &str, local: bool) {
    let bank: BankType = serde_json::from_str(&format!("\"{name}\"")).unwrap();
    assert_eq!(bank.core_local(), local);
    assert_eq!(bank.as_str(), name);
    assert_eq!(serde_json::to_string(&bank).unwrap(), format!("\"{name}\""));
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

#[test_case(-1; "negative")]
#[test_case(16; "past the last core")]
#[test_case(i64::MAX; "i64 maximum")]
fn core_ids_reject_unsupported_values(id: i64) {
    assert!(CoreId::new(id).is_err());
}

#[test_case("{\"16\":1}"; "past the last core")]
#[test_case("{\"-1\":1}"; "negative core")]
#[test_case("{\"x\":1}"; "non numeric core")]
#[test_case("null"; "null")]
fn per_core_rejects_unsupported_cores(json: &str) {
    assert!(serde_json::from_str::<PerCore<i64>>(json).is_err());
}

#[test]
fn core_id_json_accepts_only_supported_cores() {
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
fn identities_refuse_values_outside_their_domain() {
    assert_eq!(CcdId::new(15).map(CcdId::get), Ok(15));
    assert_eq!(CcdId::new(16), Err(InvalidCcd(16)));
    assert_eq!(CcdId::new(-1), Err(InvalidCcd(-1)));
    assert_eq!(
        CcdId::try_from(usize::MAX),
        Err(InvalidCcd(usize::MAX as i128))
    );
    assert_eq!(InvalidCcd(16).to_string(), "ccd 16 outside 0–15");
    assert_eq!(CpuId::new(65535).map(CpuId::get), Ok(u16::MAX));
    assert_eq!(CpuId::new(65536), Err(InvalidCpu(65536)));
    assert_eq!(CpuId::new(-1), Err(InvalidCpu(-1)));
    assert_eq!(CpuId::try_from(65536_u32), Err(InvalidCpu(65536)));
    assert_eq!(InvalidCpu(-1).to_string(), "cpu -1 outside 0–65535");
    assert!(serde_json::from_str::<std::collections::BTreeMap<CcdId, u32>>(r#"{"16":1}"#).is_err());
}

#[test_case(r#"{"core":3,"ccd":16,"cpus":[]}"#; "ccd too large")]
#[test_case(r#"{"core":3,"ccd":-1,"cpus":[]}"#; "negative ccd")]
#[test_case(r#"{"core":3,"ccd":0,"cpus":[65536]}"#; "cpu too large")]
#[test_case(r#"{"core":3,"ccd":0,"cpus":[-1]}"#; "negative cpu")]
fn core_info_refuses_invalid_identities(json: &str) {
    assert!(serde_json::from_str::<CoreInfo>(json).is_err(), "{json}");
}

#[test]
fn topology_and_pstore_round_trip() {
    let info = CoreInfo {
        core: core(3),
        ccd: CcdId::new(1).unwrap(),
        cpus: vec![CpuId::from(3), CpuId::from(19)],
    };
    let json = serde_json::to_string(&info).unwrap();
    assert_eq!(json, r#"{"core":3,"ccd":1,"cpus":[3,19]}"#);
    assert_eq!(serde_json::from_str::<CoreInfo>(&json).unwrap(), info);
    assert!(serde_json::from_str::<CoreInfo>(r#"{"core":16,"ccd":0,"cpus":[]}"#).is_err());
    let clocks = std::collections::BTreeMap::from([(CcdId::new(1).unwrap(), 5200)]);
    let json = serde_json::to_string(&clocks).unwrap();
    assert_eq!(json, r#"{"1":5200}"#);
    assert_eq!(
        serde_json::from_str::<std::collections::BTreeMap<CcdId, u32>>(&json).unwrap(),
        clocks
    );
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
    let legacy = serde_json::from_str::<TrialConditions>(
        r#"{"elapsed_ms":1000,"core_mhz":{"0":5500},"package_power_w":142.5}"#,
    )
    .unwrap();
    assert_eq!(legacy.core_mhz.get(core(0)), Some(5500));
    assert_eq!(legacy.package_power_w, Some(142.5));
    assert_eq!(legacy.cores_power_w, None);
    let full = TrialConditions {
        elapsed_ms: 5000,
        tctl_c: Some(0),
        tccd_c: Some([("Tccd1".to_owned(), 60)].into()),
        core_mhz: (0..16).map(|id| (core(id), 5500)).collect(),
        worker_cpu_ms: [(core(0), 0), (core(7), 4000)].into_iter().collect(),
        package_power_w: Some(142.5),
        cores_power_w: Some(100.0),
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

#[test_case(b"\xff\xfe\n"; "invalid utf8")]
#[test_case(b"[]\n"; "non object")]
fn read_samples_rejects_complete_undecodable_lines(invalid: &[u8]) {
    let mut samples = ReadSamples::from_reader(Cursor::new(invalid));
    let next = samples.next();
    assert!(
        matches!(next, Some(Err(SampleError::Decode(_)))),
        "{next:?}"
    );
    assert!(samples.next().is_none());
}

#[test_case(b"{\"elapsed_ms\":3}"; "torn object")]
#[test_case(b"not json"; "torn non json")]
fn read_samples_ignores_torn_tails(torn: &[u8]) {
    assert!(ReadSamples::from_reader(Cursor::new(torn)).next().is_none());
}

#[test]
fn read_samples_reports_open_errors_but_missing_files_are_absent() {
    let mut denied = ReadSamples::<Cursor<Vec<u8>>>::from_open(Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "injected open failure",
    )));
    let next = denied.next();
    assert!(matches!(next, Some(Err(SampleError::Open(_)))), "{next:?}");
    assert!(denied.next().is_none());
    let mut missing = ReadSamples::<Cursor<Vec<u8>>>::from_open(Err(std::io::Error::from(
        std::io::ErrorKind::NotFound,
    )));
    assert!(missing.next().is_none());
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

#[test_case("[1,1]"; "duplicate")]
#[test_case("[16]"; "past the last core")]
#[test_case("[-1]"; "negative")]
#[test_case("[\"1\"]"; "string core")]
#[test_case("null"; "null")]
#[test_case("{}"; "object")]
fn core_set_rejects_duplicates_and_unsupported_cores(json: &str) {
    assert!(serde_json::from_str::<CoreSet>(json).is_err(), "{json}");
}

#[test]
fn core_sets_iterate_from_both_ends() {
    let set: CoreSet = [3_u8, 0, 15, 7]
        .into_iter()
        .map(|id| CoreId::try_from(id).unwrap())
        .collect();
    let ids = |iter: &mut dyn Iterator<Item = CoreId>| iter.map(CoreId::get).collect::<Vec<_>>();
    assert_eq!(ids(&mut set.iter()), [0, 3, 7, 15]);
    assert_eq!(ids(&mut set.iter().rev()), [15, 7, 3, 0]);
    assert_eq!(set.first().map(CoreId::get), Some(0));
    assert_eq!(set.last().map(CoreId::get), Some(15));
    let mut both = set.iter();
    assert_eq!(both.next().map(CoreId::get), Some(0));
    assert_eq!(both.next_back().map(CoreId::get), Some(15));
    assert_eq!(both.len(), 2);
    assert_eq!(both.next_back().map(CoreId::get), Some(7));
    assert_eq!(both.next().map(CoreId::get), Some(3));
    assert_eq!((both.next(), both.next_back()), (None, None));
    assert_eq!(CoreSet::default().first(), None);
    assert_eq!(CoreSet::default().last(), None);
}

#[test]
fn core_ids_convert_from_unsigned_integers() {
    assert_eq!(CoreId::try_from(15_u8).unwrap().get(), 15);
    assert_eq!(CoreId::try_from(15_usize).unwrap().index(), 15);
    assert_eq!(CoreId::try_from(16_u8), Err(InvalidCore(16)));
    assert_eq!(CoreId::try_from(16_usize), Err(InvalidCore(16)));
    assert_eq!(
        CoreId::try_from(usize::MAX),
        Err(InvalidCore(usize::MAX as i128))
    );
}

#[test]
fn containment_keeps_its_cause_behind_a_fixed_message() {
    use std::error::Error as _;
    let error = Error::Containment("scope still has 2 processes".into());
    assert_eq!(error.to_string(), "trial cleanup could not be confirmed");
    assert_eq!(
        error.source().unwrap().to_string(),
        "scope still has 2 processes"
    );
}

#[test]
fn ccd_and_cpu_ids_display_as_numbers() {
    assert_eq!(CcdId::new(1).unwrap().to_string(), "1");
    assert_eq!(CpuId::from(23).to_string(), "23");
}

#[test_case::test_case(65536; "first unrepresentable cpu")]
#[test_case::test_case(usize::MAX; "largest platform input")]
fn unsigned_invalid_identity_diagnostics_preserve_the_input(value: usize) {
    let core = CoreId::try_from(value).unwrap_err();
    let ccd = CcdId::try_from(value).unwrap_err();
    let cpu = CpuId::try_from(value).unwrap_err();
    assert_eq!(core.0, value as i128);
    assert_eq!(ccd.0, value as i128);
    assert_eq!(cpu.0, value as i128);
    assert!(core.to_string().contains(&value.to_string()));
    assert!(ccd.to_string().contains(&value.to_string()));
    assert!(cpu.to_string().contains(&value.to_string()));
}

#[test]
fn every_core_set_mask_preserves_its_indices_and_count() {
    for mask in 0..=u16::MAX {
        let expected: Vec<_> = (0_u8..16)
            .filter(|index| mask & (1_u16 << index) != 0)
            .map(|index| CoreId::try_from(index).unwrap())
            .collect();
        let set: CoreSet = expected.iter().copied().collect();
        assert_eq!(set.len(), expected.len(), "mask {mask}");
        assert_eq!(set.iter().len(), expected.len(), "mask {mask}");
        assert_eq!(set.iter().collect::<Vec<_>>(), expected, "mask {mask}");
        assert_eq!(
            set.iter().rev().collect::<Vec<_>>(),
            expected.into_iter().rev().collect::<Vec<_>>(),
            "mask {mask}"
        );
    }
}
