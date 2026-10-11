use togi_machine::{BankType, CoreInfo, CpuId};

use super::support::{core, cores, fixture, message, stamped, topology};
use crate::parse::{StatusError, bank_name};
use crate::{CpuCoreMap, DuplicateCpu, Message, parse};

const DESCRIPTION: &str = "[Hardware Error]: Corrected error, no action required.";
const DECODE_UNIT_STATUS: &str =
    "[Hardware Error]: CPU:0 (1a:44:0) MC3_STATUS[Over|CE|-]: 0xbc00000000010135";
const LOAD_STORE_STATUS: &str =
    "[Hardware Error]: CPU:9 (1a:44:0) MC0_STATUS[Over|CE|-]: 0xbc00000000010135";
const RAW_STATUS: &str = "[Hardware Error]: CPU 3: Machine Check: 0 Bank 5: bea0000000000108";
const DECODE_UNIT_BANK: &str = "[Hardware Error]: Decode Unit Ext. Error Code: 1";
const LOAD_STORE_BANK: &str = "[Hardware Error]: Load Store Unit Ext. Error Code: 13";

fn banks_of(messages: &[Message], map: &CpuCoreMap) -> Vec<BankType> {
    parse(messages, map)
        .unwrap()
        .iter()
        .map(|mce| mce.bank_type)
        .collect()
}

#[test]
fn captured_blocks_keep_every_line_and_the_status_time() {
    let topology = topology(&[(0, &[0, 16]), (8, &[8, 24]), (9, &[9, 25]), (13, &[13, 29])]);
    let map = CpuCoreMap::new(&topology).unwrap();
    for (name, cpus, bank) in [
        (
            "zen4-decode-unit.jsonl",
            vec![0_u16, 8, 13],
            BankType::DecodeUnit,
        ),
        ("zen5-load-store.jsonl", vec![9], BankType::LoadStore),
    ] {
        let messages = fixture(name);
        let got = parse(&messages, &map).unwrap();
        assert_eq!(got.len(), cpus.len(), "{name}");
        let statuses: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, message)| message.text.contains("_STATUS["))
            .map(|(index, _)| index)
            .collect();
        for ((mce, cpu), status) in got.iter().zip(&cpus).zip(statuses) {
            assert_eq!(mce.cpu, CpuId(*cpu), "{name}");
            assert_eq!(mce.core, Some(core(i64::from(*cpu))), "{name}");
            assert_eq!(mce.bank_type, bank, "{name}");
            assert!(mce.corrected && mce.bank_type.core_local(), "{name}");
            assert_eq!(mce.time, messages[status].time, "{name}");
            assert_eq!(mce.lines.len(), messages.len() / cpus.len() - 1, "{name}");
            assert_eq!(mce.lines[0], stamped(&messages[status - 1]), "{name}");
        }
    }
}

#[test]
fn every_smca_long_name_is_a_named_bank_or_other() {
    use BankType::{
        DataFabric, DecodeUnit, ExecutionUnit, FloatingPoint, InstructionFetch, L2Cache, L3Cache,
        LoadStore, MemoryController, Other,
    };
    let names = [
        ("Coherent Station", DataFabric),
        ("DACC Back-end Unit", Other),
        ("DACC Front-end Unit", Other),
        ("Decode Unit", DecodeUnit),
        ("eDDR5 CMN Unit", Other),
        ("Execution Unit", ExecutionUnit),
        ("Floating Point Unit", FloatingPoint),
        ("Global Memory Interconnect PCS Unit", Other),
        ("Global Memory Interconnect PHY Unit", Other),
        ("Instruction Fetch Unit", InstructionFetch),
        ("L2 Cache", L2Cache),
        ("L3 Cache", L3Cache),
        ("Load Store Unit", LoadStore),
        ("Microprocessor 5 Unit", Other),
        ("MPART Unit", Other),
        ("MPASP Unit", Other),
        ("MPDACC Unit", Other),
        ("MPDMA Unit", Other),
        ("MPM Unit", Other),
        ("MPRAS Unit", Other),
        ("NBIF Unit", Other),
        ("Northbridge IO Unit", Other),
        ("Parameter Block", Other),
        ("PCI Express Unit", Other),
        ("PCIe Link Unit", Other),
        ("Power, Interrupts, etc.", DataFabric),
        ("Platform Security Processor", Other),
        ("Reserved", Other),
        ("SATA Unit", Other),
        ("System Hub Unit", Other),
        ("System Management Unit", Other),
        ("Die to Die Interconnect Unit", Other),
        ("Unified Memory Controller", MemoryController),
        ("Unified Memory Controller v2", MemoryController),
        ("USB Unit", Other),
        ("WAFL PHY Unit", Other),
        ("Ext Global Memory Interconnect PCS Unit", Other),
        ("Ext Global Memory Interconnect PHY Unit", Other),
    ];
    assert_eq!(names.len(), 38);
    let core_local = [
        "Load Store Unit",
        "Instruction Fetch Unit",
        "L2 Cache",
        "Decode Unit",
        "Execution Unit",
        "Floating Point Unit",
    ];
    let map = cores(&[(0, &[0])]);
    let block = fixture("zen4-decode-unit.jsonl");
    for (name, bank) in names {
        let mut messages = block[..6].to_vec();
        messages[4].text = format!("[Hardware Error]: {name} Ext. Error Code: 1");
        let got = parse(&messages, &map).unwrap();
        assert_eq!(got.len(), 1, "{name}");
        assert_eq!(got[0].bank_type, bank, "{name}");
        assert_eq!(
            got[0].bank_type.core_local(),
            core_local.contains(&name),
            "{name}"
        );
    }
}

#[test]
fn statuses_decode_corrected_deferred_and_raw_bits() {
    let map = cores(&[(6, &[12])]);
    let cases = [
        (
            "decoded corrected",
            "[Hardware Error]: CPU:12 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135",
            true,
        ),
        (
            "decoded uncorrected",
            "[Hardware Error]: CPU:12 (1a:44:0) MC1_STATUS[Over|UE|-]: 0xbc00000000010135",
            false,
        ),
        (
            "decoded deferred",
            "[Hardware Error]: CPU:12 (1a:44:0) MC1_STATUS[Over|-|-]: 0xbc00000000010135",
            false,
        ),
        (
            "raw corrected",
            "[Hardware Error]: CPU 12: Machine Check: 0 Bank 1: 0000000000000000",
            true,
        ),
        (
            "raw uncorrected",
            "[Hardware Error]: CPU 12: Machine Check Exception: 0 Bank 1: 2000000000000000",
            false,
        ),
        (
            "raw deferred",
            "[Hardware Error]: CPU 12: Machine Check: 0 Bank 1: 0000100000000000",
            false,
        ),
    ];
    for (name, status, corrected) in cases {
        let messages = [
            message(100_000_013, DESCRIPTION),
            message(100_000_014, status),
            message(100_000_015, LOAD_STORE_BANK),
        ];
        let got = parse(&messages, &map).unwrap();
        let bank = if name.starts_with("raw") {
            BankType::Unknown
        } else {
            BankType::LoadStore
        };
        assert_eq!(got.len(), 1, "{name}");
        assert_eq!(got[0].corrected, corrected, "{name}");
        assert_eq!(got[0].bank_type, bank, "{name}");
        assert_eq!((got[0].core, got[0].bank), (Some(core(6)), 1), "{name}");
        let want: Vec<String> = messages.iter().map(stamped).collect();
        assert_eq!(got[0].lines, want, "{name}");
    }
}

#[test]
fn unknown_cpu_and_unrelated_lines_close_the_record() {
    let map = cores(&[(6, &[12])]);
    let at_micros = 100_000_013;
    let messages = [
        message(
            at_micros,
            "[Hardware Error]: CPU:99 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135",
        ),
        message(at_micros, "[Hardware Error]: Bank 1 is reserved."),
        message(at_micros, "unrelated"),
        message(at_micros, DECODE_UNIT_BANK),
        message(
            at_micros,
            "[Hardware Error]: Deferred error, no action required.",
        ),
        message(
            at_micros,
            "[Hardware Error]: CPU:12 (1a:44:0) MC1_STATUS[Over|UE|-]: 0xbc00000000010135",
        ),
    ];
    let got = parse(&messages, &map).unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(
        (got[0].core, got[0].bank_type, got[0].lines.len()),
        (None, BankType::Unknown, 2)
    );
    assert_eq!(
        (got[1].core, got[1].corrected, got[1].lines.len()),
        (Some(core(6)), false, 2)
    );

    let mut reserved = messages[..2].to_vec();
    reserved[0].text =
        "[Hardware Error]: CPU:12 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135".to_owned();
    assert_eq!(banks_of(&reserved, &map), [BankType::Other]);
}

#[test]
fn truncated_and_interleaved_records_attribute_conservatively() {
    use BankType::{DecodeUnit, ExecutionUnit, LoadStore, Unknown};
    let map = cores(&[(0, &[0]), (3, &[3]), (9, &[9])]);
    let cases: [(&str, Option<usize>, &[BankType]); 5] = [
        ("truncated-record.jsonl", None, &[Unknown, LoadStore]),
        (
            "truncated-interleaved-records.jsonl",
            None,
            &[Unknown, Unknown, ExecutionUnit],
        ),
        (
            "interleaved-records.jsonl",
            None,
            &[Unknown, Unknown, DecodeUnit],
        ),
        (
            "interleaved-separated-records.jsonl",
            Some(5),
            &[Unknown, DecodeUnit],
        ),
        (
            "interleaved-separated-records.jsonl",
            None,
            &[Unknown, Unknown, Unknown, LoadStore],
        ),
    ];
    for (name, prefix, want) in cases {
        let mut messages = fixture(name);
        if let Some(prefix) = prefix {
            messages.truncate(prefix);
        }
        assert_eq!(banks_of(&messages, &map), want, "{name} {prefix:?}");
    }
}

#[test]
fn a_surplus_bank_line_withdraws_every_tentative_record_back_to_the_first_unresolved() {
    let map = cores(&[(0, &[0]), (3, &[3]), (9, &[9])]);
    let messages = fixture("interleaved-separated-records.jsonl");
    let line_counts = |messages: &[Message]| -> Vec<usize> {
        parse(messages, &map)
            .unwrap()
            .iter()
            .map(|mce| mce.lines.len())
            .collect()
    };
    assert_eq!(line_counts(&messages[..5]), [2, 3]);
    assert_eq!(line_counts(&messages), [2, 2, 2, 3]);
}

#[test]
fn a_truncated_record_keeps_its_own_lines_and_no_bank() {
    let map = cores(&[(0, &[0]), (9, &[9])]);
    let got = parse(&fixture("truncated-record.jsonl"), &map).unwrap();
    let counts: Vec<usize> = got.iter().map(|mce| mce.lines.len()).collect();
    assert_eq!(counts, [3, 3]);
}

type Interleaving = (
    &'static str,
    &'static [u16],
    Option<&'static [u16]>,
    bool,
    &'static [BankType],
);

#[test]
fn interleaved_orders_are_unknown_and_sequential_records_are_attributed() {
    use BankType::{DecodeUnit, LoadStore, Unknown};
    let map = cores(&[(0, &[0]), (3, &[3]), (9, &[9])]);
    let status = |cpu: u16| match cpu {
        0 => DECODE_UNIT_STATUS,
        9 => LOAD_STORE_STATUS,
        _ => RAW_STATUS,
    };
    let bank = |cpu: u16| match cpu {
        0 => Some(DECODE_UNIT_BANK),
        9 => Some(LOAD_STORE_BANK),
        _ => None,
    };
    let cases: [Interleaving; 4] = [
        (
            "CPU 0 then CPU 9",
            &[0, 9],
            Some(&[0, 9]),
            false,
            &[Unknown, Unknown],
        ),
        (
            "CPU 9 then CPU 0 with banner",
            &[9, 0],
            Some(&[9, 0]),
            true,
            &[Unknown, Unknown],
        ),
        (
            "sequential records",
            &[0, 9],
            None,
            false,
            &[DecodeUnit, LoadStore],
        ),
        (
            "raw record then decoded",
            &[3, 0],
            None,
            false,
            &[Unknown, DecodeUnit],
        ),
    ];
    for (name, order, owners, banner, want) in cases {
        let mut messages: Vec<Message> = Vec::new();
        let mut add = |text: &str| {
            let micros = i64::try_from(messages.len()).unwrap();
            messages.push(message(micros, text));
        };
        for (index, &cpu) in order.iter().enumerate() {
            add(DESCRIPTION);
            add(status(cpu));
            if banner && index == 0 {
                add("mce: [Hardware Error]: Machine check events logged");
            }
            match owners {
                None => {
                    if let Some(line) = bank(cpu) {
                        add(line);
                    }
                }
                Some(owners) if index == order.len() - 1 => {
                    for &owner in owners {
                        add(bank(owner).unwrap());
                    }
                }
                Some(_) => {}
            }
        }
        assert_eq!(banks_of(&messages, &map), want, "{name}");
    }
}

#[test]
fn a_cpu_number_beyond_the_machine_stops_the_parse_and_keeps_earlier_checks() {
    let map = cores(&[(0, &[0])]);
    let messages = [
        message(1, DESCRIPTION),
        message(2, DECODE_UNIT_STATUS),
        message(
            3,
            "[Hardware Error]: CPU:70000 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135",
        ),
        message(
            4,
            "[Hardware Error]: CPU:0 (1a:44:0) MC70000_STATUS[Over|CE|-]: 0xbc00000000010135",
        ),
    ];
    let failure = parse(&messages, &map).unwrap_err();
    assert_eq!(failure.error, StatusError::Cpu("70000".to_owned()));
    assert_eq!(failure.mces.len(), 1);

    let failure = parse(&[messages[1].clone(), messages[3].clone()], &map).unwrap_err();
    assert_eq!(failure.error, StatusError::Bank("70000".to_owned()));
    assert_eq!(failure.mces.len(), 1);
}

#[test]
fn status_lines_are_matched_exactly() {
    let map = cores(&[(0, &[0])]);
    for text in [
        "[Hardware Error]: CPU:0 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xBC00000000010135",
        "[Hardware Error]: CPU:0 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc0000000001013",
        "[Hardware Error]: CPU:0 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135 ",
        "[Hardware Error]: CPU:0 (1a:44) MC1_STATUS[Over|CE|-]: 0xbc00000000010135",
        "[Hardware Error]: CPU 0: Machine Check: 0 Bank 1: 000000000000000",
        "[Hardware Error]: CPU +0: Machine Check: 0 Bank 1: 0000000000000000",
    ] {
        assert!(
            parse(&[message(1, text)], &map).unwrap().is_empty(),
            "{text}"
        );
    }
}

#[test]
fn a_bank_line_names_everything_before_its_last_error_code() {
    assert_eq!(
        bank_name("[Hardware Error]: Foo Ext. Error Code: 1 Ext. Error Code: 2"),
        Some("Foo Ext. Error Code: 1")
    );
    assert_eq!(bank_name("[Hardware Error]:  Ext. Error Code: 2"), None);
    assert_eq!(bank_name("[Hardware Error]: Foo Ext. Error Code: x"), None);
    assert_eq!(bank_name("Foo Ext. Error Code: 2"), None);
    assert_eq!(
        bank_name("[Hardware Error]: Foo\nBar Ext. Error Code: 2"),
        None
    );
}

#[test]
fn a_cpu_listed_twice_is_a_topology_error() {
    let duplicate = [
        CoreInfo {
            core: core(1),
            ..CoreInfo::default()
        },
        CoreInfo {
            core: core(2),
            ..CoreInfo::default()
        },
    ]
    .map(|info| CoreInfo {
        cpus: vec![CpuId(4)],
        ..info
    });
    assert_eq!(
        CpuCoreMap::new(&duplicate),
        Err(DuplicateCpu {
            cpu: CpuId(4),
            first: core(1),
            second: core(2)
        })
    );
    let same_core = [CoreInfo {
        core: core(1),
        cpus: vec![CpuId(4), CpuId(4)],
        ..CoreInfo::default()
    }];
    assert!(CpuCoreMap::new(&same_core).is_err());
    let map = cores(&[(1, &[2, 18])]);
    assert_eq!(
        (map.core(CpuId(18)), map.core(CpuId(3))),
        (Some(core(1)), None)
    );
}
