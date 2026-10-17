use super::fixture::Tree;
#[cfg(target_os = "linux")]
use crate::files::Files;
use crate::mailbox::{Command, MailboxError, Status, Transport};
use crate::sysfs::Sysfs;
use std::io::ErrorKind;
use tempfile::TempDir;
use test_case::test_case;

const DRIVER: &str = "sys/kernel/ryzen_smu_drv";

fn mailbox_tree() -> (Tree, Sysfs) {
    let tree = Tree::with_ccds(0, 0, true);
    tree.put(&format!("{DRIVER}/smu_args"), &"\0".repeat(24));
    tree.put(&format!("{DRIVER}/rsmu_cmd"), &"\0".repeat(4));
    tree.put(&format!("{DRIVER}/smn"), &"\0".repeat(4));
    let sysfs = Sysfs::from_readers(tree.root(), tree.source());
    (tree, sysfs)
}

#[test]
fn real_fs_an_absent_driver_directory_is_not_a_mailbox() {
    let empty = TempDir::new().unwrap();
    assert!(Sysfs::locate(empty.path()).is_none());
}

#[test_case(0xff, Status::Failed, "status 0xff (failed)"; "failed")]
#[test_case(0xfe, Status::UnknownCommand, "status 0xfe (unknown command)"; "unknown command")]
#[test_case(0xfd, Status::Prerequisite, "status 0xfd (rejected: prerequisite)"; "prerequisite")]
#[test_case(0xfc, Status::Busy, "status 0xfc (rejected: busy)"; "busy")]
#[test_case(0x42, Status::Unknown(0x42), "status 0x42 (unknown status)"; "unknown status")]
fn statuses_are_typed(code: u32, status: Status, text: &str) {
    let (_tree, sysfs) = mailbox_tree();
    let error = sysfs.transact(code, [0; 6]).unwrap_err();
    assert!(
        matches!(error, MailboxError::Status { command, status: got } if command == code && got == status),
        "{error:?}"
    );
    assert!(error.to_string().ends_with(text), "{error}");
}

#[test]
fn arguments_round_trip_in_24_little_endian_bytes() {
    let (tree, sysfs) = mailbox_tree();
    let args = [0x1234_5678, 4, 3, 2, 1, 0xffec];
    assert_eq!(sysfs.transact(1, args).unwrap(), args);
    let written = tree.bytes(&format!("{DRIVER}/smu_args"));
    assert_eq!(written.len(), 24);
    assert_eq!(written[..4], 0x1234_5678_u32.to_le_bytes());
    assert_eq!(
        tree.bytes(&format!("{DRIVER}/rsmu_cmd")),
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
        tree.bytes(&format!("{DRIVER}/rsmu_cmd")),
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

#[test_case("smu_args"; "arguments file")]
#[test_case("rsmu_cmd"; "command file")]
fn unavailable_mailbox_command_files_fail_at_the_open_stage(name: &str) {
    let (tree, mut sysfs) = mailbox_tree();
    tree.remove(&format!("{DRIVER}/{name}"));
    let error = sysfs.command(Command::SetOffset, [0; 6]).unwrap_err();
    let path = tree.path(&format!("{DRIVER}/{name}"));
    let MailboxError::Open {
        path: failed,
        source,
    } = &error
    else {
        panic!("{error:?}");
    };
    assert_eq!(failed, &path);
    assert_eq!(source.kind(), ErrorKind::NotFound);
    if name == "smu_args" {
        assert_eq!(
            tree.bytes(&format!("{DRIVER}/rsmu_cmd")),
            [0; 4],
            "command written after the argument write failed"
        );
    }
}

#[test]
fn an_unavailable_smn_file_fails_at_the_open_stage() {
    let (tree, mut sysfs) = mailbox_tree();
    tree.remove(&format!("{DRIVER}/smn"));
    let error = sysfs.read_smn(0).unwrap_err();
    let path = tree.path(&format!("{DRIVER}/smn"));
    let MailboxError::Open {
        path: failed,
        source,
    } = &error
    else {
        panic!("{error:?}");
    };
    assert_eq!(failed, &path);
    assert_eq!(source.kind(), ErrorKind::NotFound);
}

#[cfg(target_os = "linux")]
#[test]
fn a_failed_write_is_reported_with_its_path() {
    let tree = Tree::with_ccds(0, 0, true);
    let args = tree.path(&format!("{DRIVER}/smu_args"));
    let readers = crate::files::Readers {
        cpu_names: vec![],
        read: std::sync::Arc::new(|_| panic!("read after failed argument write")),
        write: std::sync::Arc::new(|_, _| Err(std::io::Error::other("write failed"))),
    };
    let sysfs = Sysfs::from_readers(tree.root(), Files::Readers(std::sync::Arc::new(readers)));
    let error = sysfs.transact(1, [0; 6]).unwrap_err();
    assert!(
        matches!(&error, MailboxError::Write { path, .. } if path == &args),
        "{error:?}"
    );
}

#[test]
fn the_driver_reaches_the_mailbox_only_through_validation() {
    let tree = Tree::new(8, true);
    tree.put(&format!("{DRIVER}/smn"), "\0\0\0\0");
    let mut driver = tree.open().unwrap();
    assert_eq!(
        tree.bytes(&format!("{DRIVER}/smn")),
        [0; 4],
        "open wrote to the mailbox"
    );
    let _ = driver.validate().unwrap();
    assert_eq!(
        tree.bytes(&format!("{DRIVER}/smn")),
        0x304a_03dc_u32.to_le_bytes()
    );
    let detail = driver.check_slot_mapping().detail;
    assert!(
        detail.contains("harvested CCDs are not yet supported"),
        "{detail}"
    );
}
