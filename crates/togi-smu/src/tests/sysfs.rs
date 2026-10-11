use super::fixture::Tree;
use crate::Driver;
use crate::mailbox::{Command, MailboxError, Status, Transport};
use crate::sysfs::Sysfs;
use std::fs;
use std::io::ErrorKind;
use tempfile::TempDir;

const DRIVER: &str = "sys/kernel/ryzen_smu_drv";

fn mailbox_tree() -> (Tree, Sysfs) {
    let tree = Tree::with_ccds(0, 0, true);
    tree.put(&format!("{DRIVER}/smu_args"), &"\0".repeat(24));
    tree.put(&format!("{DRIVER}/rsmu_cmd"), &"\0".repeat(4));
    tree.put(&format!("{DRIVER}/smn"), &"\0".repeat(4));
    let sysfs = Sysfs::locate(tree.root()).unwrap();
    (tree, sysfs)
}

#[test]
fn an_absent_driver_directory_is_not_a_mailbox() {
    let empty = TempDir::new().unwrap();
    assert!(Sysfs::locate(empty.path()).is_none());
}

#[test]
fn statuses_are_typed() {
    let (_tree, sysfs) = mailbox_tree();
    for (code, status, text) in [
        (0xff, Status::Failed, "status 0xff (failed)"),
        (
            0xfe,
            Status::UnknownCommand,
            "status 0xfe (unknown command)",
        ),
        (
            0xfd,
            Status::Prerequisite,
            "status 0xfd (rejected: prerequisite)",
        ),
        (0xfc, Status::Busy, "status 0xfc (rejected: busy)"),
        (0x42, Status::Unknown(0x42), "status 0x42 (unknown status)"),
    ] {
        let error = sysfs.transact(code, [0; 6]).unwrap_err();
        assert!(
            matches!(error, MailboxError::Status { command, status: got } if command == code && got == status),
            "{error:?}"
        );
        assert!(error.to_string().ends_with(text), "{error}");
    }
}

#[test]
fn arguments_round_trip_in_24_little_endian_bytes() {
    let (tree, sysfs) = mailbox_tree();
    let args = [0x1234_5678, 4, 3, 2, 1, 0xffec];
    assert_eq!(sysfs.transact(1, args).unwrap(), args);
    let written = fs::read(tree.path(&format!("{DRIVER}/smu_args"))).unwrap();
    assert_eq!(written.len(), 24);
    assert_eq!(written[..4], 0x1234_5678_u32.to_le_bytes());
    assert_eq!(
        fs::read(tree.path(&format!("{DRIVER}/rsmu_cmd"))).unwrap(),
        1_u32.to_le_bytes()
    );
}

#[test]
fn typed_commands_write_their_code() {
    let (tree, mut sysfs) = mailbox_tree();
    let error = sysfs.command(Command::ReadOffset, [0x1010_0000, 0, 0, 0, 0, 0]);
    assert!(matches!(
        error,
        Err(MailboxError::Status { command: 0xd5, .. })
    ));
    assert_eq!(
        fs::read(tree.path(&format!("{DRIVER}/rsmu_cmd"))).unwrap(),
        0xd5_u32.to_le_bytes()
    );
}

#[test]
fn smn_reads_write_the_address_and_read_the_value() {
    let (tree, mut sysfs) = mailbox_tree();
    assert_eq!(sysfs.read_smn(0x304a_03dc).unwrap(), 0x304a_03dc);
    tree.put(&format!("{DRIVER}/smn"), "xxxxx");
    let error = sysfs.read_smn(0).unwrap_err();
    assert!(
        matches!(
            error,
            MailboxError::Length {
                got: 5,
                want: 4,
                ..
            }
        ),
        "{error:?}"
    );
    assert!(error.to_string().ends_with("got 5 bytes, want 4"));
}

#[test]
fn a_response_of_the_wrong_length_is_refused() {
    let (tree, sysfs) = mailbox_tree();
    tree.put(&format!("{DRIVER}/smu_args"), &"\0".repeat(30));
    let error = sysfs.transact(1, [0; 6]).unwrap_err();
    assert!(
        matches!(
            error,
            MailboxError::Length {
                got: 30,
                want: 24,
                ..
            }
        ),
        "{error:?}"
    );
}

#[test]
fn unavailable_mailbox_files_fail_at_the_open_stage() {
    for name in ["smu_args", "rsmu_cmd", "smn"] {
        let (tree, mut sysfs) = mailbox_tree();
        tree.remove(&format!("{DRIVER}/{name}"));
        let error = if name == "smn" {
            sysfs.read_smn(0).unwrap_err()
        } else {
            sysfs.command(Command::SetOffset, [0; 6]).unwrap_err()
        };
        let path = tree.path(&format!("{DRIVER}/{name}"));
        let MailboxError::Open {
            path: failed,
            source,
        } = &error
        else {
            panic!("{name}: {error:?}");
        };
        assert_eq!(failed, &path);
        assert_eq!(source.kind(), ErrorKind::NotFound);
        if name == "smu_args" {
            assert_eq!(
                fs::read(tree.path(&format!("{DRIVER}/rsmu_cmd"))).unwrap(),
                [0; 4],
                "command written after the argument write failed"
            );
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_failed_write_is_reported_with_its_path() {
    let (tree, sysfs) = mailbox_tree();
    let args = tree.path(&format!("{DRIVER}/smu_args"));
    fs::remove_file(&args).unwrap();
    std::os::unix::fs::symlink("/dev/full", &args).unwrap();
    let error = sysfs.transact(1, [0; 6]).unwrap_err();
    assert!(
        matches!(&error, MailboxError::Write { path, .. } if path == &args),
        "{error:?}"
    );
}

#[test]
fn the_driver_reaches_the_real_mailbox_only_through_validation() {
    let tree = Tree::new(8, true);
    tree.put(&format!("{DRIVER}/smn"), "\0\0\0\0");
    let mut driver = Driver::open(tree.root()).unwrap();
    let smn = tree.path(&format!("{DRIVER}/smn"));
    assert_eq!(fs::read(&smn).unwrap(), [0; 4], "open wrote to the mailbox");
    let _ = driver.validate().unwrap();
    assert_eq!(fs::read(&smn).unwrap(), 0x304a_03dc_u32.to_le_bytes());
    let detail = driver.check_slot_mapping().detail;
    assert!(
        detail.contains("harvested CCDs are not yet supported"),
        "{detail}"
    );
}
