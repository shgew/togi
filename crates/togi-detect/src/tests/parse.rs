use test_case::test_case;
use togi_machine::{BankType, CcdId, CoreInfo, CpuId};

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

#[test_case("zen4-decode-unit.jsonl", &[0, 8, 13], BankType::DecodeUnit; "zen4 decode unit")]
#[test_case("zen5-load-store.jsonl", &[9], BankType::LoadStore; "zen5 load store")]
fn captured_blocks_keep_every_line_and_the_status_time(name: &str, cpus: &[u16], bank: BankType) {
    let topology = topology(&[(0, &[0, 16]), (8, &[8, 24]), (9, &[9, 25]), (13, &[13, 29])]);
    let map = CpuCoreMap::new(&topology).unwrap();
    let messages = fixture(name);
    let got = parse(&messages, &map).unwrap();
    assert_eq!(got.len(), cpus.len(), "{name}");
    let statuses: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, message)| message.text.contains("_STATUS["))
        .map(|(index, _)| index)
        .collect();
    for ((mce, cpu), status) in got.iter().zip(cpus).zip(statuses) {
        assert_eq!(mce.cpu, CpuId::from(*cpu), "{name}");
        assert_eq!(mce.core, Some(core(i64::from(*cpu))), "{name}");
        assert_eq!(mce.bank_type, bank, "{name}");
        assert!(mce.corrected && mce.bank_type.core_local(), "{name}");
        assert_eq!(mce.time, messages[status].time, "{name}");
        assert_eq!(mce.lines.len(), messages.len() / cpus.len() - 1, "{name}");
        assert_eq!(mce.lines[0], stamped(&messages[status - 1]), "{name}");
    }
}

#[test_case("Coherent Station", BankType::DataFabric; "Coherent Station")]
#[test_case("DACC Back-end Unit", BankType::Other; "DACC Back-end Unit")]
#[test_case("DACC Front-end Unit", BankType::Other; "DACC Front-end Unit")]
#[test_case("Decode Unit", BankType::DecodeUnit; "Decode Unit")]
#[test_case("eDDR5 CMN Unit", BankType::Other; "eDDR5 CMN Unit")]
#[test_case("Execution Unit", BankType::ExecutionUnit; "Execution Unit")]
#[test_case("Floating Point Unit", BankType::FloatingPoint; "Floating Point Unit")]
#[test_case("Global Memory Interconnect PCS Unit", BankType::Other; "Global Memory Interconnect PCS Unit")]
#[test_case("Global Memory Interconnect PHY Unit", BankType::Other; "Global Memory Interconnect PHY Unit")]
#[test_case("Instruction Fetch Unit", BankType::InstructionFetch; "Instruction Fetch Unit")]
#[test_case("L2 Cache", BankType::L2Cache; "L2 Cache")]
#[test_case("L3 Cache", BankType::L3Cache; "L3 Cache")]
#[test_case("Load Store Unit", BankType::LoadStore; "Load Store Unit")]
#[test_case("Microprocessor 5 Unit", BankType::Other; "Microprocessor 5 Unit")]
#[test_case("MPART Unit", BankType::Other; "MPART Unit")]
#[test_case("MPASP Unit", BankType::Other; "MPASP Unit")]
#[test_case("MPDACC Unit", BankType::Other; "MPDACC Unit")]
#[test_case("MPDMA Unit", BankType::Other; "MPDMA Unit")]
#[test_case("MPM Unit", BankType::Other; "MPM Unit")]
#[test_case("MPRAS Unit", BankType::Other; "MPRAS Unit")]
#[test_case("NBIF Unit", BankType::Other; "NBIF Unit")]
#[test_case("Northbridge IO Unit", BankType::Other; "Northbridge IO Unit")]
#[test_case("Parameter Block", BankType::Other; "Parameter Block")]
#[test_case("PCI Express Unit", BankType::Other; "PCI Express Unit")]
#[test_case("PCIe Link Unit", BankType::Other; "PCIe Link Unit")]
#[test_case("Power, Interrupts, etc.", BankType::DataFabric; "Power, Interrupts, etc.")]
#[test_case("Platform Security Processor", BankType::Other; "Platform Security Processor")]
#[test_case("Reserved", BankType::Other; "Reserved")]
#[test_case("SATA Unit", BankType::Other; "SATA Unit")]
#[test_case("System Hub Unit", BankType::Other; "System Hub Unit")]
#[test_case("System Management Unit", BankType::Other; "System Management Unit")]
#[test_case("Die to Die Interconnect Unit", BankType::Other; "Die to Die Interconnect Unit")]
#[test_case("Unified Memory Controller", BankType::MemoryController; "Unified Memory Controller")]
#[test_case("Unified Memory Controller v2", BankType::MemoryController; "Unified Memory Controller v2")]
#[test_case("USB Unit", BankType::Other; "USB Unit")]
#[test_case("WAFL PHY Unit", BankType::Other; "WAFL PHY Unit")]
#[test_case("Ext Global Memory Interconnect PCS Unit", BankType::Other; "Ext Global Memory Interconnect PCS Unit")]
#[test_case("Ext Global Memory Interconnect PHY Unit", BankType::Other; "Ext Global Memory Interconnect PHY Unit")]
fn every_smca_long_name_is_a_named_bank_or_other(name: &str, bank: BankType) {
    let core_local = [
        "Load Store Unit",
        "Instruction Fetch Unit",
        "L2 Cache",
        "Decode Unit",
        "Execution Unit",
        "Floating Point Unit",
    ];
    let map = cores(&[(0, &[0])]);
    let mut messages = fixture("zen4-decode-unit.jsonl")[..6].to_vec();
    messages[4].text = format!("[Hardware Error]: {name} Ext. Error Code: 1");
    let got = parse(&messages, &map).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].bank_type, bank);
    assert_eq!(got[0].bank_type.core_local(), core_local.contains(&name));
}

#[test_case(
    "[Hardware Error]: CPU:12 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135",
    true,
    BankType::LoadStore;
    "decoded corrected"
)]
#[test_case(
    "[Hardware Error]: CPU:12 (1a:44:0) MC1_STATUS[Over|UE|-]: 0xbc00000000010135",
    false,
    BankType::LoadStore;
    "decoded uncorrected"
)]
#[test_case(
    "[Hardware Error]: CPU:12 (1a:44:0) MC1_STATUS[Over|-|-]: 0xbc00000000010135",
    false,
    BankType::LoadStore;
    "decoded deferred"
)]
#[test_case(
    "[Hardware Error]: CPU 12: Machine Check: 0 Bank 1: 0000000000000000",
    true,
    BankType::Unknown;
    "raw corrected"
)]
#[test_case(
    "[Hardware Error]: CPU 12: Machine Check Exception: 0 Bank 1: 2000000000000000",
    false,
    BankType::Unknown;
    "raw uncorrected"
)]
#[test_case(
    "[Hardware Error]: CPU 12: Machine Check: 0 Bank 1: 0000100000000000",
    false,
    BankType::Unknown;
    "raw deferred"
)]
fn statuses_decode_corrected_deferred_and_raw_bits(status: &str, corrected: bool, bank: BankType) {
    let map = cores(&[(6, &[12])]);
    let messages = [
        message(100_000_013, DESCRIPTION),
        message(100_000_014, status),
        message(100_000_015, LOAD_STORE_BANK),
    ];
    let got = parse(&messages, &map).unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].corrected, corrected);
    assert_eq!(got[0].bank_type, bank);
    assert_eq!((got[0].core, got[0].bank), (Some(core(6)), 1));
    let want: Vec<String> = messages.iter().map(stamped).collect();
    assert_eq!(got[0].lines, want);
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

#[test_case("truncated-record.jsonl", None, &[BankType::Unknown, BankType::LoadStore]; "truncated record")]
#[test_case(
    "truncated-interleaved-records.jsonl",
    None,
    &[BankType::Unknown, BankType::Unknown, BankType::ExecutionUnit];
    "truncated interleaved records"
)]
#[test_case(
    "interleaved-records.jsonl",
    None,
    &[BankType::Unknown, BankType::Unknown, BankType::DecodeUnit];
    "interleaved records"
)]
#[test_case(
    "interleaved-separated-records.jsonl",
    Some(5),
    &[BankType::Unknown, BankType::DecodeUnit];
    "interleaved separated records cut at five lines"
)]
#[test_case(
    "interleaved-separated-records.jsonl",
    None,
    &[BankType::Unknown, BankType::Unknown, BankType::Unknown, BankType::LoadStore];
    "interleaved separated records"
)]
fn truncated_and_interleaved_records_attribute_conservatively(
    name: &str,
    prefix: Option<usize>,
    want: &[BankType],
) {
    let map = cores(&[(0, &[0]), (3, &[3]), (9, &[9])]);
    let mut messages = fixture(name);
    if let Some(prefix) = prefix {
        messages.truncate(prefix);
    }
    assert_eq!(banks_of(&messages, &map), want);
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

fn interleaving_status(cpu: u16) -> &'static str {
    match cpu {
        0 => DECODE_UNIT_STATUS,
        9 => LOAD_STORE_STATUS,
        _ => RAW_STATUS,
    }
}

fn interleaving_bank(cpu: u16) -> Option<&'static str> {
    match cpu {
        0 => Some(DECODE_UNIT_BANK),
        9 => Some(LOAD_STORE_BANK),
        _ => None,
    }
}

#[test_case(
    &[0, 9],
    Some(&[0, 9]),
    false,
    &[BankType::Unknown, BankType::Unknown];
    "CPU 0 then CPU 9"
)]
#[test_case(
    &[9, 0],
    Some(&[9, 0]),
    true,
    &[BankType::Unknown, BankType::Unknown];
    "CPU 9 then CPU 0 with banner"
)]
#[test_case(
    &[0, 9],
    None,
    false,
    &[BankType::DecodeUnit, BankType::LoadStore];
    "sequential records"
)]
#[test_case(
    &[3, 0],
    None,
    false,
    &[BankType::Unknown, BankType::DecodeUnit];
    "raw record then decoded"
)]
fn interleaved_orders_are_unknown_and_sequential_records_are_attributed(
    order: &[u16],
    owners: Option<&[u16]>,
    banner: bool,
    want: &[BankType],
) {
    let map = cores(&[(0, &[0]), (3, &[3]), (9, &[9])]);
    let mut messages: Vec<Message> = Vec::new();
    let mut add = |text: &str| {
        let micros = i64::try_from(messages.len()).unwrap();
        messages.push(message(micros, text));
    };
    for (index, &cpu) in order.iter().enumerate() {
        add(DESCRIPTION);
        add(interleaving_status(cpu));
        if banner && index == 0 {
            add("mce: [Hardware Error]: Machine check events logged");
        }
        match owners {
            None => {
                if let Some(line) = interleaving_bank(cpu) {
                    add(line);
                }
            }
            Some(owners) if index == order.len() - 1 => {
                for &owner in owners {
                    add(interleaving_bank(owner).unwrap());
                }
            }
            Some(_) => {}
        }
    }
    assert_eq!(banks_of(&messages, &map), want);
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

#[test_case(
    "[Hardware Error]: CPU:abc (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135",
    &StatusError::Cpu("abc".to_owned());
    "decoded alphabetic cpu"
)]
#[test_case(
    "[Hardware Error]: CPU:+0 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135",
    &StatusError::Cpu("+0".to_owned());
    "decoded signed cpu"
)]
#[test_case(
    "[Hardware Error]: CPU:0 (1a:44:0) MCabc_STATUS[Over|CE|-]: 0xbc00000000010135",
    &StatusError::Bank("abc".to_owned());
    "decoded alphabetic bank"
)]
#[test_case(
    "[Hardware Error]: CPU abc: Machine Check: 0 Bank 1: bc00000000010135",
    &StatusError::Cpu("abc".to_owned());
    "raw alphabetic cpu"
)]
#[test_case(
    "[Hardware Error]: CPU +0: Machine Check: 0 Bank 1: 0000000000000000",
    &StatusError::Cpu("+0".to_owned());
    "raw signed cpu"
)]
#[test_case(
    "[Hardware Error]: CPU 0: Machine Check Exception: 0 Bank x1: bc00000000010135",
    &StatusError::Bank("x1".to_owned());
    "raw alphanumeric bank"
)]
fn a_status_line_with_a_non_numeric_cpu_or_bank_stops_the_parse_and_keeps_earlier_checks(
    text: &str,
    error: &StatusError,
) {
    let map = cores(&[(0, &[0])]);
    let messages = [message(1, DECODE_UNIT_STATUS), message(2, text)];
    let failure = parse(&messages, &map).unwrap_err();
    assert_eq!(&failure.error, error);
    assert_eq!(failure.mces.len(), 1);
    assert_eq!(failure.mces[0].core, Some(core(0)));
}

#[test_case("[Hardware Error]: CPU:0 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xBC00000000010135"; "uppercase hex")]
#[test_case("[Hardware Error]: CPU:0 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc0000000001013"; "short status")]
#[test_case("[Hardware Error]: CPU:0 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135 "; "trailing space")]
#[test_case("[Hardware Error]: CPU:0 (1a:44) MC1_STATUS[Over|CE|-]: 0xbc00000000010135"; "short model")]
#[test_case("[Hardware Error]: CPU 0: Machine Check: 0 Bank 1: 000000000000000"; "short raw status")]
#[test_case("[Hardware Error]: CPU:abc (1a:44) MC1_STATUS[Over|CE|-]: 0xbc00000000010135"; "non-numeric cpu with short model")]
#[test_case("[Hardware Error]: CPU:abc is offline"; "offline cpu")]
#[test_case("[Hardware Error]: CPU abc: Machine Check: 0 Bank x1: 000000000000000"; "non-numeric raw cpu and bank with short status")]
#[test_case("[Hardware Error]: CPU abc: Machine Check pending"; "pending raw cpu")]
fn status_lines_are_matched_exactly(text: &str) {
    let map = cores(&[(0, &[0])]);
    assert!(parse(&[message(1, text)], &map).unwrap().is_empty());
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
    let info = |id| CoreInfo {
        core: core(id),
        ccd: CcdId::new(0).unwrap(),
        cpus: vec![CpuId::from(4)],
    };
    let duplicate = [info(1), info(2)];
    assert_eq!(
        CpuCoreMap::new(&duplicate),
        Err(DuplicateCpu {
            cpu: CpuId::from(4),
            first: core(1),
            second: core(2)
        })
    );
    let same_core = [CoreInfo {
        cpus: vec![CpuId::from(4), CpuId::from(4)],
        ..info(1)
    }];
    assert!(CpuCoreMap::new(&same_core).is_err());
    let map = cores(&[(1, &[2, 18])]);
    assert_eq!(
        (map.core(CpuId::from(18)), map.core(CpuId::from(3))),
        (Some(core(1)), None)
    );
}
