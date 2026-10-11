use super::fixture::{
    CCD0_FUSE, CCD1_FUSE, Fake, RSMU_COMMAND, RSMU_RESPONSE, Tree, chain, open, read, smu_error,
    validated, write,
};
use crate::mailbox::{Command, MailboxError};
use crate::{Driver, Error, FuseRead, Misplaced, SlotError};
use togi_machine::{CcdId, CpuId, TargetOffset};

fn mapping_detail(driver: &mut Driver) -> (bool, String) {
    let check = driver.check_slot_mapping();
    assert_eq!(check.name, "slot_mapping");
    (check.ok, check.detail)
}

fn assert_per_core_refused(driver: &mut Driver, fake: &Fake) {
    for info in driver.topology().into_iter().flatten() {
        let id = i64::from(info.core.get());
        assert!(read(driver, id).is_err(), "read of core {id} accepted");
        assert!(
            write(driver, id, TargetOffset::clamped(-10)).is_err(),
            "write of core {id} accepted"
        );
    }
    assert!(fake.commands().is_empty(), "{:?}", fake.commands());
}

struct Case {
    name: &'static str,
    cores: usize,
    fuse: u32,
    ccd: u32,
    cache: bool,
    valid: bool,
}

#[test]
fn mapping_and_fallback() {
    let case = |name, cores, fuse, ccd, cache, valid| Case {
        name,
        cores,
        fuse,
        ccd,
        cache,
        valid,
    };
    for case in [
        case("full-cache-topology", 8, 0x00, 0, true, true),
        case("full-die-fallback", 8, 0x00, 0, false, true),
        case("full-fuse-upper-bits", 8, 0x100, 0, true, true),
        case("first-slot-disabled", 7, 0x01, 0, true, false),
        case("last-slot-disabled", 7, 0x80, 0, true, false),
        case("harvested-die-fallback", 6, 0x81, 0, false, false),
        case("interior-slots-disabled", 6, 0x42, 0, true, false),
        case("all-slots-disabled", 8, 0xff, 0, true, false),
        case("second-ccd-disabled", 8, 0x81, 1, true, false),
        case("fuse-topology-mismatch", 8, 0x01, 0, true, false),
        case("full-fuse-missing-cores", 6, 0x00, 0, true, false),
    ] {
        let tree = Tree::new(case.cores, case.cache);
        let fake = Fake::new();
        fake.set_value(CCD0_FUSE + (case.ccd << 25), case.fuse);
        if case.cores != 8 {
            fake.set_value(CCD1_FUSE, case.fuse);
        }
        let mut driver = validated(&tree, &fake);
        let (ok, detail) = mapping_detail(&mut driver);
        assert_eq!(ok, case.valid, "{}: {detail}", case.name);
        if !case.valid {
            let mask = case.fuse.to_le_bytes()[0];
            if mask != 0 {
                assert!(
                    detail.contains(&format!("CCD{}", case.ccd))
                        && detail.contains(&format!("{mask:#04x}"))
                        && detail.contains("harvested CCDs are not yet supported"),
                    "{}: {detail}",
                    case.name
                );
            }
            assert_per_core_refused(&mut driver, &fake);
            continue;
        }
        for info in driver.topology().unwrap() {
            write(
                &mut driver,
                i64::from(info.core.get()),
                TargetOffset::clamped(-10),
            )
            .unwrap();
            let want =
                u32::from(info.ccd.0) << 28 | (u32::from(info.core.get()) % 8) << 20 | 0xfff6;
            assert_eq!(
                fake.commands().last(),
                Some(&(Command::SetOffset, want)),
                "{}",
                case.name
            );
        }
    }
}

#[test]
fn slot_identity_preflight() {
    struct Identity {
        name: &'static str,
        cores: [[usize; 8]; 2],
        cache: bool,
        detail: &'static str,
        reads_smn: bool,
    }
    let ids = |name, cores, cache, detail, reads_smn| Identity {
        name,
        cores,
        cache,
        detail,
        reads_smn,
    };
    let full = [[0, 1, 2, 3, 4, 5, 6, 7], [8, 9, 10, 11, 12, 13, 14, 15]];
    for case in [
        ids("full-topology", full, true, "", true),
        ids(
            "cross-ccd-ids",
            [[0, 1, 2, 3, 4, 5, 6, 8], [7, 9, 10, 11, 12, 13, 14, 15]],
            true,
            "CCD0 core IDs disagree with modulo-eight slots: core 08 at slot 7, want slot 0",
            true,
        ),
        ids(
            "multiple-offending-ids",
            [[0, 1, 2, 3, 4, 5, 8, 9], [6, 7, 10, 11, 12, 13, 14, 15]],
            true,
            "CCD0 core IDs disagree with modulo-eight slots: core 08 at slot 6, want slot 0; core 09 at slot 7, want slot 1",
            true,
        ),
        ids(
            "second-ccd-id-outside-range",
            [[0, 1, 2, 3, 4, 5, 6, 7], [8, 9, 10, 11, 12, 13, 14, 16]],
            true,
            "CCD1 core IDs 16 outside supported range 0-15",
            false,
        ),
        ids(
            "ccd-numbered-16-to-23",
            [[0, 1, 2, 3, 4, 5, 6, 7], [16, 17, 18, 19, 20, 21, 22, 23]],
            true,
            "CCD1 core IDs 16, 17, 18, 19, 20, 21, 22, 23 outside supported range 0-15",
            false,
        ),
        ids(
            "both-ccds-outside-range",
            [
                [16, 17, 18, 19, 20, 21, 22, 23],
                [24, 25, 26, 27, 28, 29, 30, 31],
            ],
            true,
            "CCD0 core IDs 16, 17, 18, 19, 20, 21, 22, 23; CCD1 core IDs 24, 25, 26, 27, 28, 29, 30, 31 outside supported range 0-15",
            false,
        ),
        ids(
            "cross-ccd-die-fallback",
            [[0, 1, 2, 3, 4, 5, 6, 8], [7, 9, 10, 11, 12, 13, 14, 15]],
            false,
            "CCD0 core IDs disagree with modulo-eight slots: core 08 at slot 7, want slot 0",
            true,
        ),
    ] {
        let tree = Tree::new(8, case.cache);
        for (ccd, cores) in case.cores.iter().enumerate() {
            for (slot, core) in cores.iter().enumerate() {
                let cpu = ccd * 8 + slot;
                tree.cpu(cpu, *core, ccd, case.cache);
                tree.cpu(cpu + 16, *core, ccd, case.cache);
            }
        }
        let fake = Fake::new();
        let mut driver = validated(&tree, &fake);
        let (ok, detail) = mapping_detail(&mut driver);
        if case.detail.is_empty() {
            assert!(ok, "{}: {detail}", case.name);
            let readback = driver.check_readback();
            assert!(readback.ok, "{}: {readback:?}", case.name);
            let want: Vec<_> = driver
                .topology()
                .unwrap()
                .iter()
                .map(|info| {
                    (
                        Command::ReadOffset,
                        u32::from(info.ccd.0) << 28 | (u32::from(info.core.get()) % 8) << 20,
                    )
                })
                .collect();
            assert_eq!(fake.commands(), want, "{}", case.name);
            continue;
        }
        assert!(!ok);
        assert_eq!(detail, case.detail, "{}", case.name);
        assert_eq!(
            !fake.smn_reads().is_empty(),
            case.reads_smn,
            "{}",
            case.name
        );
        let readback = driver.check_readback();
        let expected = if case.reads_smn {
            case.detail
        } else {
            "outside supported range 0-15"
        };
        assert!(
            !readback.ok && readback.detail.contains(expected),
            "{}: {readback:?}",
            case.name
        );
        assert_per_core_refused(&mut driver, &fake);
    }
}

#[test]
fn stale_ccd_fuse_read_refuses_per_core_access() {
    for stale in [vec![1], vec![2], vec![1, 2]] {
        let tree = Tree::new(8, true);
        let fake = Fake::new();
        let mut previous = 0;
        let mut ccd1_reads = 0;
        fake.hook_smn(move |address, value| {
            if address == CCD1_FUSE {
                ccd1_reads += 1;
                if stale.contains(&ccd1_reads) {
                    return previous;
                }
            }
            previous = value;
            value
        });
        let mut driver = validated(&tree, &fake);
        assert!(!driver.check_slot_mapping().ok);
        assert!(read(&mut driver, 8).is_err());
        assert!(write(&mut driver, 8, TargetOffset::clamped(-10)).is_err());
        assert!(fake.commands().is_empty());
    }
}

#[test]
fn fuse_equal_to_a_probe_is_accepted_when_fresh() {
    for (name, response, command, valid) in [
        ("response", 0x00, 0x6e, true),
        ("command", 0x01, 0x00, true),
        ("indistinguishable", 0x00, 0x00, false),
    ] {
        let tree = Tree::new(8, true);
        let fake = Fake::new();
        fake.set_value(RSMU_RESPONSE, response);
        fake.set_value(RSMU_COMMAND, command);
        let mut driver = validated(&tree, &fake);
        assert_eq!(driver.check_slot_mapping().ok, valid, "{name}");
    }
}

#[test]
fn fuse_read_failure_at_each_stage_refuses_per_core_access() {
    for (failed, stage) in [
        (1, FuseRead::ResponseProbe),
        (2, FuseRead::Fuse),
        (3, FuseRead::CommandProbe),
        (4, FuseRead::RepeatFuse),
    ] {
        let tree = Tree::new(8, true);
        let fake = Fake::new();
        fake.fail_smn_read(failed);
        let mut driver = validated(&tree, &fake);
        let (ok, detail) = mapping_detail(&mut driver);
        assert!(
            !ok && detail.contains("SMN unavailable"),
            "{failed}: {detail}"
        );
        let error = read(&mut driver, 0).unwrap_err();
        let Error::SlotMapping { source, .. } = smu_error(&error) else {
            panic!("{failed}: {error:?}");
        };
        assert!(matches!(
            source.as_ref(),
            SlotError::Read { read, source: MailboxError::Read { .. }, .. } if *read == stage
        ));
        assert!(write(&mut driver, 0, TargetOffset::clamped(-10)).is_err());
        assert!(fake.commands().is_empty());
    }
}

#[test]
fn siblings_that_disagree_on_the_ccd_refuse_before_any_smn_read() {
    let tree = Tree::new(8, true);
    tree.cpu(25, 9, 0, true);
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);
    let (ok, detail) = mapping_detail(&mut driver);
    assert!(!ok);
    assert_eq!(
        detail,
        "core 9 siblings report different CCD keys: cpu9 key 21, cpu25 key 20"
    );
    assert!(fake.smn_reads().is_empty());
    assert!(matches!(
        driver.topology().unwrap_err(),
        Error::Topology(SlotError::Siblings { core_id: 9, .. })
    ));
    assert_per_core_refused(&mut driver, &fake);
}

#[test]
fn siblings_that_disagree_through_the_die_fallback_refuse_too() {
    let tree = Tree::new(8, false);
    tree.cpu(20, 4, 1, false);
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);
    let (ok, detail) = mapping_detail(&mut driver);
    assert!(!ok);
    assert_eq!(
        detail,
        "core 4 siblings report different CCD keys: cpu4 key 10, cpu20 key 11"
    );
}

#[test]
fn a_second_physical_package_refuses_naming_its_cpus() {
    let tree = Tree::new(8, true);
    for cpu in 16..32 {
        tree.put(
            &format!("sys/devices/system/cpu/cpu{cpu}/topology/physical_package_id"),
            "1",
        );
    }
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);
    let (ok, detail) = mapping_detail(&mut driver);
    assert!(!ok);
    assert!(
        detail.starts_with("CPUs span 2 physical packages: package 0: cpu0, cpu1,"),
        "{detail}"
    );
    assert!(detail.contains("; package 1: cpu16, cpu17,"), "{detail}");
    assert!(detail.ends_with("cpu30, cpu31"), "{detail}");
    assert!(fake.smn_reads().is_empty());
    assert_per_core_refused(&mut driver, &fake);
}

#[test]
fn a_raw_core_id_outside_0_to_15_is_a_typed_topology_error() {
    let tree = Tree::new(8, true);
    for cpu in [15, 31] {
        tree.put(
            &format!("sys/devices/system/cpu/cpu{cpu}/topology/core_id"),
            "16",
        );
    }
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);
    assert!(matches!(
        driver.topology().unwrap_err(),
        Error::CoreOutOfRange {
            core_id: 16,
            ccd: 1
        }
    ));
    assert_eq!(
        mapping_detail(&mut driver).1,
        "CCD1 core IDs 16 outside supported range 0-15"
    );
    assert!(!driver.check_readback().ok);
    assert!(fake.smn_reads().is_empty());
}

#[test]
fn slot_error_details_name_every_offender() {
    let error = SlotError::SlotOrder {
        ccd: CcdId(1),
        misplaced: vec![Misplaced {
            core: super::fixture::core(9),
            slot: 0,
            want: 1,
        }],
    };
    assert_eq!(
        chain(&error),
        "CCD1 core IDs disagree with modulo-eight slots: core 09 at slot 0, want slot 1"
    );
    let siblings = SlotError::Siblings {
        core_id: 3,
        cpus: vec![(CpuId(3), 20), (CpuId(19), 21)],
    };
    assert_eq!(
        siblings.to_string(),
        "core 3 siblings report different CCD keys: cpu3 key 20, cpu19 key 21"
    );
}

#[test]
fn open_driver_has_no_slot_map_until_validated() {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    let mut driver = open(&tree, &fake);
    let (ok, detail) = mapping_detail(&mut driver);
    assert!(!ok && detail.contains("not validated"), "{detail}");
    assert!(fake.smn_reads().is_empty());
}
