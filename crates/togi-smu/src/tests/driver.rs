use super::fixture::{
    Fake, Tree, accepted_unvalidated, chain, core, open, open_error, read, smu_error, validated,
    write, write_all,
};
use crate::mailbox::{Command, MailboxError, Status};
use crate::{Driver, Error, Operation};
use std::fs;
use std::num::IntErrorKind;
use togi_machine::{BiosContext, CcdId, CpuId, ObservedOffset, TargetOffset};

#[test]
fn topology_groups_online_cpus_and_encodes_offsets() {
    let tree = Tree::new(8, true);
    tree.put("sys/devices/system/cpu/cpu32/topology/core_id", "99");
    tree.put("sys/devices/system/cpu/cpu32/online", "0\n");
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);

    let cores = driver.topology().unwrap();
    assert_eq!(cores.len(), 16);
    assert_eq!(cores[9].ccd, CcdId(1));
    assert_eq!(cores[9].cpus, [CpuId(9), CpuId(25)]);

    for (target, want) in [(-12, 0x1010_fff4), (-60, 0x1010_ffce), (5, 0x1010_0000)] {
        write(&mut driver, 9, TargetOffset::clamped(target)).unwrap();
        assert_eq!(fake.commands().last(), Some(&(Command::SetOffset, want)));
    }
    write_all(&mut driver, TargetOffset::clamped(-60)).unwrap();
    assert_eq!(
        fake.commands().last(),
        Some(&(Command::SetAllOffsets, 0xffce))
    );

    for (raw, want) in [(0xffec, -20), (0x000a, 10)] {
        fake.respond(Command::ReadOffset, raw);
        assert_eq!(read(&mut driver, 9).unwrap(), ObservedOffset(want));
        assert_eq!(
            fake.commands().last(),
            Some(&(Command::ReadOffset, 0x1010_0000))
        );
    }
}

#[test]
fn core_absent_from_a_single_ccd_topology_is_unknown() {
    let tree = Tree::with_ccds(1, 8, true);
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);
    assert!(driver.check_slot_mapping().ok);
    let error = read(&mut driver, 9).unwrap_err();
    assert!(matches!(
        smu_error(&error),
        Error::UnknownCore { core: unknown } if *unknown == core(9)
    ));
    let writes = write(&mut driver, 9, TargetOffset::ZERO).unwrap_err();
    assert!(matches!(smu_error(&writes), Error::UnknownCore { .. }));
    assert!(fake.commands().is_empty());
}

#[test]
fn open_touches_no_hardware_and_validate_maps_once() {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    let mut driver = open(&tree, &fake);
    assert_eq!(fake.hardware_calls(), 0);
    let _ = driver.validate().unwrap();
    let reads = fake.smn_reads();
    assert_eq!(
        reads,
        [
            0x03b1_0570,
            0x304a_03dc,
            0x03b1_0524,
            0x304a_03dc,
            0x03b1_0570,
            0x304a_03dc + (1 << 25),
            0x03b1_0524,
            0x304a_03dc + (1 << 25),
        ]
    );
    let _ = driver.validate().unwrap();
    assert_eq!(fake.smn_reads(), reads);
    assert!(fake.commands().is_empty());
}

#[test]
fn without_the_driver_directory_nothing_validates() {
    let tree = Tree::new(8, true);
    fs::remove_dir_all(tree.path("sys/kernel/ryzen_smu_drv")).unwrap();
    let mut driver = Driver::open(tree.root()).unwrap();
    let check = driver.check_driver();
    assert!(!check.ok && check.detail.contains("ryzen_smu is not loaded"));
    assert!(matches!(
        driver.validate().unwrap_err(),
        Error::DriverNotLoaded
    ));
    assert_eq!(accepted_unvalidated(&mut driver), Vec::<&str>::new());
}

#[test]
fn missing_topology_is_an_open_error() {
    let empty = tempfile::tempdir().unwrap();
    assert!(matches!(
        Driver::open(empty.path()).unwrap_err(),
        Error::ListCpus { .. }
    ));
}

#[test]
fn unsupported_identity_never_reaches_hardware() {
    for (name, path, text) in [
        ("family", "proc/cpuinfo", "cpu family : 25\nmodel : 68\n"),
        ("model", "proc/cpuinfo", "cpu family : 26\nmodel : 80\n"),
        ("codename", "sys/kernel/ryzen_smu_drv/codename", "22\n"),
    ] {
        let tree = Tree::new(8, true);
        tree.put(path, text);
        let fake = Fake::new();
        let mut driver = open(&tree, &fake);
        driver.validate().unwrap_err();
        assert_eq!(driver.topology().unwrap().len(), 16, "{name}");
        assert_eq!(
            accepted_unvalidated(&mut driver),
            Vec::<&str>::new(),
            "{name}"
        );
        assert_eq!(fake.hardware_calls(), 0, "{name}");
    }
}

#[test]
fn identity_only_driver_checks_but_never_opens_the_mailbox() {
    let tree = Tree::new(8, true);
    tree.identity_files();
    let mut driver = Driver::identity_only(tree.root()).unwrap();
    assert!(driver.check_cpu().ok);
    assert!(driver.check_driver().ok);
    assert!(matches!(
        driver.validate().unwrap_err(),
        Error::IdentityOnly {
            operation: Operation::Validate
        }
    ));
    assert_eq!(accepted_unvalidated(&mut driver), Vec::<&str>::new());
    let error = driver.bios_context().unwrap_err();
    assert!(matches!(error, Error::IdentityOnly { .. }));
    let detail = driver.check_slot_mapping().detail;
    assert!(detail.contains("identity checks only"), "{detail}");
}

#[test]
fn checks_report_exact_details_and_bios_context() {
    let tree = Tree::new(8, true);
    tree.put(
        "proc/cpuinfo",
        "processor : 0\ncpu family : 26\nmodel : 68\nmodel name : Zen Test\nmicrocode : 0xb404038\n\nprocessor : 1\nmodel name : not first\n",
    );
    tree.identity_files();
    let fake = Fake::new();
    fake.respond(Command::ReadOffset, 0xfff4);
    fake.respond(Command::BoostLimit, 5750);
    let mut driver = validated(&tree, &fake);

    let cpu = driver.check_cpu();
    assert_eq!((cpu.name.as_str(), cpu.ok), ("cpu", true));
    assert_eq!(cpu.detail, "family 0x1A model 0x44 Zen Test");
    let smu = driver.check_driver();
    assert_eq!(smu.name, "ryzen_smu");
    assert_eq!(
        smu.detail,
        "codename 23 (Granite Ridge), driver 0.1, SMU 57.13"
    );
    let readback = driver.check_readback();
    assert!(readback.ok, "{readback:?}");
    assert_eq!(readback.name, "readback");
    assert!(
        readback
            .detail
            .starts_with("16 cores read back: 00:-12 01:-12"),
        "{}",
        readback.detail
    );
    assert_eq!(
        driver.bios_context().unwrap(),
        BiosContext {
            bios_version: "3.14".to_owned(),
            board: "ASRock X870E Taichi".to_owned(),
            cpu_model: "Zen Test".to_owned(),
            microcode: "0xb404038".to_owned(),
            boost_limit_mhz: 5750,
        }
    );
    assert_eq!(fake.commands().last(), Some(&(Command::BoostLimit, 0)));

    tree.put("sys/kernel/ryzen_smu_drv/codename", "22\n");
    let wrong = driver.check_driver();
    assert!(!wrong.ok);
    assert_eq!(wrong.detail, "codename 22, want 23 (Granite Ridge)");
    tree.put(
        "proc/cpuinfo",
        "processor : 0\ncpu family : 25\nmodel : 68\nmodel name : Older CPU\n\n",
    );
    let older = driver.check_cpu();
    assert!(!older.ok);
    assert!(
        older
            .detail
            .ends_with(": not Granite Ridge desktop (family 0x1A, model 0x40-0x4F)"),
        "{}",
        older.detail
    );
    fake.fail_commands(Status::Failed);
    let failed = driver.check_readback();
    assert!(
        !failed.ok && failed.detail.contains("core 00: "),
        "{failed:?}"
    );
}

#[test]
fn bios_strings_are_lossy_utf8() {
    let tree = Tree::new(8, true);
    tree.identity_files();
    fs::write(tree.path("sys/class/dmi/id/board_vendor"), b"Vendor\xff\n").unwrap();
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);
    assert_eq!(
        driver.bios_context().unwrap().board,
        "Vendor\u{fffd} X870E Taichi"
    );
}

#[test]
fn invalid_identity_files_prevent_hardware_access() {
    for (name, path, text, prefix) in [
        ("missing CPU", "proc/cpuinfo", None, "read "),
        (
            "malformed family",
            "proc/cpuinfo",
            Some("cpu family: invalid\nmodel: 68\n"),
            "parse cpu family",
        ),
        (
            "malformed model",
            "proc/cpuinfo",
            Some("cpu family: 26\nmodel: invalid\n"),
            "parse cpu model",
        ),
        (
            "missing codename",
            "sys/kernel/ryzen_smu_drv/codename",
            None,
            "read ",
        ),
        (
            "missing driver version",
            "sys/kernel/ryzen_smu_drv/drv_version",
            None,
            "read ",
        ),
        (
            "missing SMU version",
            "sys/kernel/ryzen_smu_drv/version",
            None,
            "read ",
        ),
    ] {
        let tree = Tree::new(8, true);
        match text {
            Some(text) => tree.put(path, text),
            None => tree.remove(path),
        }
        let fake = Fake::new();
        let mut driver = open(&tree, &fake);
        let refusal = chain(&driver.validate().unwrap_err());
        let detail = if path == "proc/cpuinfo" {
            driver.check_cpu().detail
        } else {
            driver.check_driver().detail
        };
        assert!(detail.starts_with(prefix), "{name}: {detail}");
        if prefix == "read " {
            assert!(
                detail.contains(&tree.path(path).display().to_string()),
                "{name}: {detail}"
            );
        }
        assert!(refusal.starts_with(prefix), "{name}: {refusal}");
        assert_eq!(
            accepted_unvalidated(&mut driver),
            Vec::<&str>::new(),
            "{name}"
        );
        assert_eq!(fake.hardware_calls(), 0, "{name}");
    }
}

#[test]
fn mailbox_failures_keep_the_cause_and_the_clamped_argument() {
    let tree = Tree::new(8, true);
    let fake = Fake::new();
    let mut driver = validated(&tree, &fake);
    fake.fail_commands(Status::Failed);

    let per_core = write(&mut driver, 9, TargetOffset::clamped(-60)).unwrap_err();
    assert_eq!(
        fake.commands().last(),
        Some(&(Command::SetOffset, 0x1010_ffce))
    );
    assert!(matches!(
        smu_error(&per_core),
        Error::Mailbox {
            operation: Operation::SetOffset(_),
            source: MailboxError::Status {
                status: Status::Failed,
                ..
            },
        }
    ));
    assert!(chain(smu_error(&per_core)).starts_with("set core 09 offset"));

    let all = write_all(&mut driver, TargetOffset::clamped(10)).unwrap_err();
    assert_eq!(fake.commands().last(), Some(&(Command::SetAllOffsets, 0)));
    assert!(matches!(
        smu_error(&all),
        Error::Mailbox {
            operation: Operation::SetAllOffsets,
            source: MailboxError::Status {
                command: 0x07,
                status: Status::Failed,
            },
        }
    ));
}

#[test]
fn malformed_topology_files_are_open_errors() {
    for relative in [
        "topology/core_id",
        "topology/physical_package_id",
        "cache/index3/id",
        "topology/die_id",
        "online",
    ] {
        let tree = Tree::new(8, relative != "topology/die_id");
        let path = tree.path(&format!("sys/devices/system/cpu/cpu0/{relative}"));
        if relative == "online" {
            fs::create_dir(&path).unwrap();
        } else {
            fs::write(&path, "invalid\n").unwrap();
        }
        let fake = Fake::new();
        let error = open_error(&tree, &fake);
        assert!(
            chain(&error).contains(&path.display().to_string()),
            "{relative}: {error}"
        );
        assert_eq!(fake.hardware_calls(), 0);
    }
}

#[test]
fn missing_topology_files_are_open_errors() {
    for relative in [
        "topology/core_id",
        "topology/physical_package_id",
        "topology/die_id",
    ] {
        let tree = Tree::new(8, relative != "topology/die_id");
        tree.remove(&format!("sys/devices/system/cpu/cpu0/{relative}"));
        let fake = Fake::new();
        let error = open_error(&tree, &fake);
        assert!(
            matches!(error, Error::ReadFile { .. }),
            "{relative}: {error:?}"
        );
    }
}

#[test]
fn no_online_cpu_is_an_open_error() {
    let tree = tempfile::tempdir().unwrap();
    let cpu = tree.path().join("sys/devices/system/cpu");
    fs::create_dir_all(cpu.join("cpunotnumber/topology")).unwrap();
    fs::write(cpu.join("cpunotnumber/topology/core_id"), "0").unwrap();
    fs::write(cpu.join("notcpu"), "ignored").unwrap();
    assert!(matches!(
        Driver::open(tree.path()).unwrap_err(),
        Error::NoOnlineCpus { .. }
    ));
}

#[test]
fn bios_context_read_failures_keep_their_cause() {
    for target in [
        "sys/class/dmi/id/bios_version",
        "sys/class/dmi/id/board_vendor",
        "sys/class/dmi/id/board_name",
        "proc/cpuinfo",
        "boost",
    ] {
        let tree = Tree::new(8, true);
        tree.identity_files();
        let fake = Fake::new();
        let mut driver = validated(&tree, &fake);
        if target == "boost" {
            fake.fail_commands(Status::Busy);
        } else {
            tree.remove(target);
        }
        let error = driver.bios_context().unwrap_err();
        match (target, &error) {
            ("boost", Error::Mailbox { operation, source }) => {
                assert_eq!(*operation, Operation::ReadBiosContext);
                assert!(matches!(
                    source,
                    MailboxError::Status {
                        status: Status::Busy,
                        ..
                    }
                ));
            }
            (_, Error::ReadFile { source, .. }) => {
                assert_eq!(source.kind(), std::io::ErrorKind::NotFound, "{target}");
                assert!(
                    fake.commands().is_empty(),
                    "{target}: boost read after missing context"
                );
            }
            _ => panic!("{target}: {error:?}"),
        }
    }
}

#[test]
fn oversized_identity_files_are_refused_not_truncated() {
    let padded = |tail: &str| format!("23{}{tail}", " ".repeat(65_534));
    let stanza_line = "pad : filler\n";
    let oversized_cpu = format!(
        "cpu family : 26\nmodel : 68\n{}",
        stanza_line.repeat(1024 * 1024 / stanza_line.len() + 1)
    );
    for (name, path, text) in [
        ("codename", "sys/kernel/ryzen_smu_drv/codename", padded("4")),
        (
            "drv_version",
            "sys/kernel/ryzen_smu_drv/drv_version",
            padded("4"),
        ),
        ("cpuinfo stanza", "proc/cpuinfo", oversized_cpu),
    ] {
        let tree = Tree::new(8, true);
        tree.put(path, &text);
        let fake = Fake::new();
        let mut driver = open(&tree, &fake);
        let error = driver.validate().unwrap_err();
        assert!(
            matches!(&error, Error::TooLarge { path: found, .. } if *found == tree.path(path)),
            "{name}: {error:?}"
        );
        assert_eq!(
            accepted_unvalidated(&mut driver),
            Vec::<&str>::new(),
            "{name}"
        );
        assert_eq!(fake.hardware_calls(), 0, "{name}");
    }
}

#[test]
fn an_identity_file_exactly_at_the_limit_is_accepted() {
    let tree = Tree::new(8, true);
    tree.put(
        "sys/kernel/ryzen_smu_drv/codename",
        &format!("23{}", " ".repeat(65_534)),
    );
    let fake = Fake::new();
    let mut driver = open(&tree, &fake);
    let _ = driver.validate().unwrap();
}

#[test]
fn malformed_cpu_identity_keeps_the_failing_field_and_its_cause() {
    for (name, text, field, value, kind) in [
        (
            "family",
            "cpu family : invalid\nmodel : 68\n",
            "family",
            "invalid",
            IntErrorKind::InvalidDigit,
        ),
        (
            "model",
            "cpu family : 26\nmodel : 0x44\n",
            "model",
            "0x44",
            IntErrorKind::InvalidDigit,
        ),
        (
            "missing model",
            "cpu family : 26\n",
            "model",
            "",
            IntErrorKind::Empty,
        ),
        (
            "family overflow",
            "cpu family : 99999999999\nmodel : 68\n",
            "family",
            "99999999999",
            IntErrorKind::PosOverflow,
        ),
    ] {
        let tree = Tree::new(8, true);
        tree.put("proc/cpuinfo", text);
        let fake = Fake::new();
        let mut driver = open(&tree, &fake);
        let error = driver.validate().unwrap_err();
        let Error::ParseCpuInfo {
            field: found,
            text: found_text,
            source,
        } = &error
        else {
            panic!("{name}: {error:?}");
        };
        assert_eq!((*found, found_text.as_str()), (field, value), "{name}");
        assert_eq!(source.kind(), &kind, "{name}");
        assert!(
            driver
                .check_cpu()
                .detail
                .starts_with(&format!("parse cpu {field}")),
            "{name}"
        );
        assert_eq!(fake.hardware_calls(), 0, "{name}");
    }
}

#[test]
fn an_unreadable_cache_id_is_a_fatal_open_error_not_a_die_fallback() {
    let tree = Tree::new(8, true);
    let path = tree.path("sys/devices/system/cpu/cpu0/cache/index3/id");
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    let fake = Fake::new();
    let error = open_error(&tree, &fake);
    let Error::ReadFile {
        path: failed,
        source,
    } = &error
    else {
        panic!("{error:?}");
    };
    assert_eq!(failed, &path);
    assert_ne!(source.kind(), std::io::ErrorKind::NotFound);
    assert!(chain(&error).contains(&path.display().to_string()));
    assert_eq!(fake.hardware_calls(), 0);
}

#[test]
fn the_logical_cpu_number_bound_is_65535() {
    let tree = Tree::new(8, true);
    tree.cpu(65535, 0, 0, true);
    let fake = Fake::new();
    let driver = open(&tree, &fake);
    assert_eq!(
        driver.topology().unwrap()[0].cpus,
        [CpuId(0), CpuId(16), CpuId(65535)]
    );

    for name in ["cpu65536", "cpu99999999999999999999999"] {
        let tree = Tree::new(8, true);
        tree.put(&format!("sys/devices/system/cpu/{name}/online"), "0\n");
        let fake = Fake::new();
        let error = open_error(&tree, &fake);
        assert!(
            matches!(&error, Error::CpuNumber { name: found } if found == name),
            "{name}: {error:?}"
        );
        assert_eq!(fake.hardware_calls(), 0, "{name}");
    }
}
