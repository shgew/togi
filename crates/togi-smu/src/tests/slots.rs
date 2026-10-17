use super::fixture::{
    CCD0_FUSE, CCD1_FUSE, Fake, RSMU_COMMAND, RSMU_RESPONSE, Tree, open, read, smu_error,
    validated, write,
};
use crate::mailbox::{Command, MailboxError};
use crate::{Driver, Error, FuseRead, Misplaced, Operation, SlotError};
use test_case::test_case;
use togi_machine::{CcdId, CpuId, TargetOffset, error_chain};

fn mapping_detail(driver: &mut Driver) -> (bool, String) {
    let check = driver.check_slot_mapping();
    assert_eq!(check.name, "slot_mapping");
    (check.ok, check.detail)
}

/// Every raw core id 0-15, independent of whether `topology()` converts, is refused for both a
/// read and a write by the slot mapping, and no command reaches the mailbox.
fn assert_per_core_refused(driver: &mut Driver, fake: &Fake) {
    for id in 0..16 {
        let error = read(driver, id).unwrap_err();
        assert!(
            matches!(
                smu_error(&error),
                Error::SlotMapping {
                    operation: Operation::ReadOffset(_),
                    ..
                }
            ),
            "read of core {id}: {error:?}"
        );
        let error = write(driver, id, TargetOffset::clamped(-10)).unwrap_err();
        assert!(
            matches!(
                smu_error(&error),
                Error::SlotMapping {
                    operation: Operation::SetOffset(_),
                    ..
                }
            ),
            "write of core {id}: {error:?}"
        );
    }
    assert!(fake.commands().is_empty(), "{:?}", fake.commands());
}

#[test_case(8, 0x00, 0, true; "full-cache-topology")]
#[test_case(8, 0x00, 0, false; "full-die-fallback")]
#[test_case(8, 0x100, 0, true; "full-fuse-upper-bits")]
fn full_mapping_writes_every_core_to_its_slot(cores: usize, fuse: u32, ccd: u32, cache: bool) {
    let tree = Tree::new(cores, cache);
    let fake = Fake::new();
    fake.set_value(CCD0_FUSE + (ccd << 25), fuse);
    let mut driver = validated(&tree, &fake);
    let (ok, detail) = mapping_detail(&mut driver);
    assert!(ok, "{detail}");
    for info in driver.topology().unwrap() {
        write(
            &mut driver,
            i64::from(info.core.get()),
            TargetOffset::clamped(-10),
        )
        .unwrap();
        let want = u32::from(info.ccd.0) << 28 | (u32::from(info.core.get()) % 8) << 20 | 0xfff6;
        assert_eq!(fake.commands().last(), Some(&(Command::SetOffset, want)));
    }
}

#[test_case(7, 0x01, 0, true; "first-slot-disabled")]
#[test_case(7, 0x80, 0, true; "last-slot-disabled")]
#[test_case(6, 0x81, 0, false; "harvested-die-fallback")]
#[test_case(6, 0x42, 0, true; "interior-slots-disabled")]
#[test_case(8, 0xff, 0, true; "all-slots-disabled")]
#[test_case(8, 0x81, 1, true; "second-ccd-disabled")]
#[test_case(8, 0x01, 0, true; "fuse-topology-mismatch")]
#[test_case(6, 0x00, 0, true; "full-fuse-missing-cores")]
fn a_mapping_that_is_not_full_refuses_per_core_access(
    cores: usize,
    fuse: u32,
    ccd: u32,
    cache: bool,
) {
    let tree = Tree::new(cores, cache);
    let fake = Fake::new();
    fake.set_value(CCD0_FUSE + (ccd << 25), fuse);
    if cores != 8 {
        fake.set_value(CCD1_FUSE, fuse);
    }
    let mut driver = validated(&tree, &fake);
    let (ok, detail) = mapping_detail(&mut driver);
    assert!(!ok, "{detail}");
    let mask = fuse.to_le_bytes()[0];
    if mask != 0 {
        assert!(
            detail.contains(&format!("CCD{ccd}"))
                && detail.contains(&format!("{mask:#04x}"))
                && detail.contains("harvested CCDs are not yet supported"),
            "{detail}"
        );
    }
    assert_per_core_refused(&mut driver, &fake);
}

const FULL_IDS: [[usize; 8]; 2] = [[0, 1, 2, 3, 4, 5, 6, 7], [8, 9, 10, 11, 12, 13, 14, 15]];

fn put_core_ids(tree: &Tree, cores: [[usize; 8]; 2], cache: bool) {
    for (ccd, cores) in cores.iter().enumerate() {
        for (slot, core) in cores.iter().enumerate() {
            let cpu = ccd * 8 + slot;
            tree.cpu(cpu, *core, ccd, cache);
            tree.cpu(cpu + 16, *core, ccd, cache);
        }
    }
}

#[test]
fn slot_identity_preflight_accepts_the_full_topology() {
    let tree = Tree::new(8, true);
    put_core_ids(&tree, FULL_IDS, true);
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);
    let (ok, detail) = mapping_detail(&mut driver);
    assert!(ok, "{detail}");
    let readback = driver.check_readback();
    assert!(readback.ok, "{readback:?}");
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
    assert_eq!(fake.commands(), want);
}

#[test_case(
    [[0, 1, 2, 3, 4, 5, 6, 8], [7, 9, 10, 11, 12, 13, 14, 15]],
    true,
    "core IDs disagree with their CCD (core ID / 8): core 08 on CCD0, want CCD1; core 07 on CCD1, want CCD0",
    false;
    "cross-ccd-ids"
)]
#[test_case(
    [[0, 1, 2, 3, 4, 5, 8, 9], [6, 7, 10, 11, 12, 13, 14, 15]],
    true,
    "core IDs disagree with their CCD (core ID / 8): core 08 on CCD0, want CCD1; core 09 on CCD0, want CCD1; core 06 on CCD1, want CCD0; core 07 on CCD1, want CCD0",
    false;
    "multiple-offending-ids"
)]
#[test_case(
    [[0, 1, 2, 3, 4, 5, 6, 7], [8, 9, 10, 11, 12, 13, 14, 16]],
    true,
    "CCD1 core IDs 16 outside supported range 0-15",
    false;
    "second-ccd-id-outside-range"
)]
#[test_case(
    [[0, 1, 2, 3, 4, 5, 6, 7], [16, 17, 18, 19, 20, 21, 22, 23]],
    true,
    "CCD1 core IDs 16, 17, 18, 19, 20, 21, 22, 23 outside supported range 0-15",
    false;
    "ccd-numbered-16-to-23"
)]
#[test_case(
    [
        [16, 17, 18, 19, 20, 21, 22, 23],
        [24, 25, 26, 27, 28, 29, 30, 31],
    ],
    true,
    "CCD0 core IDs 16, 17, 18, 19, 20, 21, 22, 23; CCD1 core IDs 24, 25, 26, 27, 28, 29, 30, 31 outside supported range 0-15",
    false;
    "both-ccds-outside-range"
)]
#[test_case(
    [[0, 1, 2, 3, 4, 5, 6, 8], [7, 9, 10, 11, 12, 13, 14, 15]],
    false,
    "core IDs disagree with their CCD (core ID / 8): core 08 on CCD0, want CCD1; core 07 on CCD1, want CCD0",
    false;
    "cross-ccd-die-fallback"
)]
fn slot_identity_preflight_refuses_inconsistent_ids(
    cores: [[usize; 8]; 2],
    cache: bool,
    want_detail: &str,
    reads_smn: bool,
) {
    let tree = Tree::new(8, cache);
    put_core_ids(&tree, cores, cache);
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);
    let (ok, detail) = mapping_detail(&mut driver);
    assert!(!ok);
    assert_eq!(detail, want_detail);
    assert_eq!(!fake.smn_reads().is_empty(), reads_smn);
    let readback = driver.check_readback();
    let expected = if want_detail.contains("outside supported range") {
        "outside supported range 0-15"
    } else {
        want_detail
    };
    assert!(
        !readback.ok && readback.detail.contains(expected),
        "{readback:?}"
    );
    assert_per_core_refused(&mut driver, &fake);
}

#[test_case(vec![1]; "first read")]
#[test_case(vec![2]; "second read")]
#[test_case(vec![1, 2]; "first and second read")]
fn stale_ccd_fuse_read_refuses_per_core_access(stale: Vec<u32>) {
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

#[test_case(0x00, 0x6e, true; "response")]
#[test_case(0x01, 0x00, true; "command")]
#[test_case(0x00, 0x00, false; "indistinguishable")]
fn fuse_equal_to_a_probe_is_accepted_when_fresh(response: u32, command: u32, valid: bool) {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    fake.set_value(RSMU_RESPONSE, response);
    fake.set_value(RSMU_COMMAND, command);
    let mut driver = validated(&tree, &fake);
    assert_eq!(driver.check_slot_mapping().ok, valid);
}

#[test_case(1, FuseRead::ResponseProbe; "response probe")]
#[test_case(2, FuseRead::Fuse; "fuse")]
#[test_case(3, FuseRead::CommandProbe; "command probe")]
#[test_case(4, FuseRead::RepeatFuse; "repeat fuse")]
fn fuse_read_failure_at_each_stage_refuses_per_core_access(failed: usize, stage: FuseRead) {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    fake.fail_smn_read(failed);
    let mut driver = validated(&tree, &fake);
    let (ok, detail) = mapping_detail(&mut driver);
    assert!(!ok && detail.contains("SMN unavailable"), "{detail}");
    let error = read(&mut driver, 0).unwrap_err();
    let Error::SlotMapping { source, .. } = smu_error(&error) else {
        panic!("{error:?}");
    };
    assert!(matches!(
        source.as_ref(),
        SlotError::Read { read, source: MailboxError::Read { .. }, .. } if *read == stage
    ));
    let error = write(&mut driver, 0, TargetOffset::clamped(-10)).unwrap_err();
    let Error::SlotMapping { operation, source } = smu_error(&error) else {
        panic!("{error:?}");
    };
    assert!(matches!(operation, Operation::SetOffset(_)));
    assert!(
        matches!(
            source.as_ref(),
            SlotError::Read { read, source: MailboxError::Read { .. }, .. } if *read == stage
        ),
        "{source:?}"
    );
    assert!(error_chain(smu_error(&error)).contains("SMN unavailable"));
    assert!(fake.commands().is_empty());
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
        Error::Topology(SlotError::Siblings(groups)) if groups.len() == 1 && groups[0].0 == 9
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
    let error = SlotError::CcdIdentity(vec![
        Misplaced {
            core: super::fixture::core(9),
            ccd: CcdId(0),
            want: CcdId(1),
        },
        Misplaced {
            core: super::fixture::core(3),
            ccd: CcdId(1),
            want: CcdId(0),
        },
    ]);
    assert_eq!(
        error_chain(&error),
        "core IDs disagree with their CCD (core ID / 8): core 09 on CCD0, want CCD1; core 03 on CCD1, want CCD0"
    );
    let siblings = SlotError::Siblings(vec![
        (3, vec![(CpuId(3), 20), (CpuId(19), 21)]),
        (5, vec![(CpuId(5), 20), (CpuId(21), 21)]),
    ]);
    assert_eq!(
        siblings.to_string(),
        "core 3 siblings report different CCD keys: cpu3 key 20, cpu19 key 21; core 5 siblings report different CCD keys: cpu5 key 20, cpu21 key 21"
    );
}

#[test]
fn every_disagreeing_sibling_group_is_named() {
    let tree = Tree::new(8, true);
    tree.cpu(20, 4, 1, true);
    tree.cpu(25, 9, 0, true);
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);
    let (ok, detail) = mapping_detail(&mut driver);
    assert!(!ok);
    assert_eq!(
        detail,
        "core 4 siblings report different CCD keys: cpu4 key 20, cpu20 key 21; core 9 siblings report different CCD keys: cpu9 key 21, cpu25 key 20"
    );
    assert!(matches!(
        driver.topology().unwrap_err(),
        Error::Topology(SlotError::Siblings(groups)) if groups.len() == 2
    ));
    assert!(fake.smn_reads().is_empty());
    assert_per_core_refused(&mut driver, &fake);
}

fn ccd_identity_detail(offenders: &[(usize, usize, usize)]) -> String {
    let list: Vec<String> = offenders
        .iter()
        .map(|(core, ccd, want)| format!("core {core:02} on CCD{ccd}, want CCD{want}"))
        .collect();
    format!(
        "core IDs disagree with their CCD (core ID / 8): {}",
        list.join("; ")
    )
}

#[test]
fn a_wholly_offline_ccd0_does_not_renumber_ccd1_onto_it() {
    let tree = Tree::new(8, true);
    for cpu in (0..8).chain(16..24) {
        tree.put(&format!("sys/devices/system/cpu/cpu{cpu}/online"), "0\n");
    }
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);
    let offenders: Vec<_> = (8..16).map(|core| (core, 0, 1)).collect();
    assert_eq!(
        mapping_detail(&mut driver),
        (false, ccd_identity_detail(&offenders))
    );
    assert!(fake.smn_reads().is_empty(), "{:?}", fake.smn_reads());
    assert_per_core_refused(&mut driver, &fake);
}

#[test]
fn swapped_ccd_key_groups_refuse_before_any_fuse_read() {
    let tree = Tree::new(8, true);
    for cpu in 0..16 {
        tree.cpu(cpu, cpu, 1 - cpu / 8, true);
        tree.cpu(cpu + 16, cpu, 1 - cpu / 8, true);
    }
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);
    let offenders: Vec<_> = (8..16)
        .map(|core| (core, 0, 1))
        .chain((0..8).map(|core| (core, 1, 0)))
        .collect();
    assert_eq!(
        mapping_detail(&mut driver),
        (false, ccd_identity_detail(&offenders))
    );
    assert!(fake.smn_reads().is_empty(), "{:?}", fake.smn_reads());
    assert_per_core_refused(&mut driver, &fake);
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
