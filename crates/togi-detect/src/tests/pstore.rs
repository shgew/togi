use std::ffi::OsString;
use std::fs;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::path::Path;
use std::time::Duration;

use rustix::fs::FileType;
use rustix::io::Errno;
use serde_json::json;
use tempfile::TempDir;
use togi_machine::{Kernel as _, PstoreRecord};

use super::support::{
    CURRENT, LATER, MISSING, OLD, Reply, boot, disk_pstore, exit, locked, ok, pstore, scripted,
};
use crate::Error;
use crate::boot::JournalBoot;
use crate::pstore::{Access, DumpId, PstoreError, Unsafe, is_directory, read_at, tail_lines};
use crate::runner::JOURNALCTL_BUDGET;

fn directory(path: &Path) {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o755)
        .create(path)
        .unwrap();
}

fn archive(files: &[(&str, &[u8])]) -> TempDir {
    let root = tempfile::tempdir().unwrap();
    for (name, data) in files {
        let path = root.path().join(name);
        directory(path.parent().unwrap());
        fs::write(path, data).unwrap();
    }
    root
}

fn boots(entries: &[(&str, Option<i64>)]) -> Vec<u8> {
    json!(
        entries
            .iter()
            .map(|(id, first)| json!({"boot_id": id, "first_entry": first}))
            .collect::<Vec<_>>()
    )
    .to_string()
    .into_bytes()
}

fn three_boots() -> Vec<u8> {
    boots(&[
        (OLD, Some(100_000_000)),
        (LATER, Some(200_000_000)),
        (CURRENT, Some(300_000_000)),
    ])
}

fn kernel_over(root: &Path, list: Vec<u8>) -> (crate::Kernel, super::support::Calls) {
    scripted(&[], disk_pstore(root), JOURNALCTL_BUDGET, move |_| {
        ok(&list)
    })
}

fn memory_kernel(files: &[(&str, &[u8])], list: Vec<u8>) -> (crate::Kernel, super::support::Calls) {
    scripted(&[], pstore(files), JOURNALCTL_BUDGET, move |_| ok(&list))
}

fn saved(files: &[(&str, &[u8])], id: &str) -> Option<PstoreRecord> {
    let (mut kernel, _) = memory_kernel(files, three_boots());
    kernel.saved_pstore(&boot(id)).unwrap()
}

fn record(path: &str, lines: &[&str]) -> PstoreRecord {
    PstoreRecord {
        path: path.to_owned(),
        lines: lines.iter().map(|line| (*line).to_owned()).collect(),
    }
}

type Case<'a> = (
    &'a str,
    &'a [(&'a str, &'a [u8])],
    &'a str,
    Option<PstoreRecord>,
);

#[test]
fn a_record_in_the_boots_wall_clock_span_is_selected() {
    let cases: [Case; 13] = [
        (
            "matching EFI record",
            &[("150/001/dmesg.txt", b"Panic#1 Part1\nfirst\nlast\x1b[2J\n")],
            OLD,
            Some(record(
                "/150/001/dmesg.txt",
                &["Panic#1 Part1", "first", "last\x1b[2J"],
            )),
        ),
        ("absent archive", &[], OLD, None),
        ("older record", &[("99/001/dmesg.txt", b"old")], OLD, None),
        (
            "following boot record",
            &[("250/001/dmesg.txt", b"later")],
            OLD,
            None,
        ),
        (
            "successor boundary excluded",
            &[("200/001/dmesg.txt", b"later")],
            OLD,
            None,
        ),
        (
            "first boundary included",
            &[("100/001/dmesg.txt", b"Panic#1 Part1\nfirst")],
            OLD,
            Some(record("/100/001/dmesg.txt", &["Panic#1 Part1", "first"])),
        ),
        (
            "unknown boot",
            &[("150/001/dmesg.txt", b"data")],
            MISSING,
            None,
        ),
        (
            "no successor",
            &[("350/001/dmesg.txt", b"data")],
            CURRENT,
            None,
        ),
        (
            "latest dump and count",
            &[
                ("150/001/dmesg.txt", b"Oops#1 Part1\nolder"),
                ("160/001/dmesg.txt", b"Oops#1 Part2\nolder count"),
                ("160/002/dmesg.txt", b"Panic#2 Part1\nlatest"),
            ],
            OLD,
            Some(record("/160/002/dmesg.txt", &["Panic#2 Part1", "latest"])),
        ),
        (
            "dump split across seconds retains newest messages",
            &[
                ("150/001/dmesg.txt", b"Oops#1 Part1\nold dump"),
                ("160/002/dmesg.txt", b"Panic#2 Part19\nnewest messages"),
                (
                    "161/002/dmesg.txt",
                    b"Panic#2 Part20\nolder messages from later chunk",
                ),
            ],
            OLD,
            Some(record(
                "/160/002/dmesg.txt",
                &["Panic#2 Part19", "newest messages"],
            )),
        ),
        (
            "count suffix wraps",
            &[
                ("150/999/dmesg.txt", b"Oops#999 Part1\nold dump"),
                ("160/000/dmesg.txt", b"Panic#1000 Part1\nlatest"),
            ],
            OLD,
            Some(record(
                "/160/000/dmesg.txt",
                &["Panic#1000 Part1", "latest"],
            )),
        ),
        (
            "repeated suffix retains full count",
            &[
                ("150/001/dmesg.txt", b"Oops#1 Part1\nold dump"),
                ("160/001/dmesg.txt", b"Panic#1001 Part1\nlatest"),
            ],
            OLD,
            Some(record(
                "/160/001/dmesg.txt",
                &["Panic#1001 Part1", "latest"],
            )),
        ),
        (
            "unknown backend",
            &[("150/dmesg.txt", b"ERST"), ("dmesg-ramoops-0", b"ramoops")],
            OLD,
            None,
        ),
    ];
    for (name, files, id, want) in cases {
        assert_eq!(saved(files, id), want, "{name}");
    }
}

#[test]
fn an_equal_dump_identity_keeps_the_earliest_chunk() {
    let got = saved(
        &[
            ("170/001/dmesg.txt", b"Panic#3 Part1\nsecond"),
            ("160/005/dmesg.txt", b"Panic#3 Part1\nfirst"),
        ],
        OLD,
    );
    assert_eq!(
        got,
        Some(record("/160/005/dmesg.txt", &["Panic#3 Part1", "first"]))
    );
}

#[test]
fn an_absent_or_empty_archive_runs_no_command() {
    let (mut kernel, calls) = memory_kernel(&[], three_boots());
    assert_eq!(kernel.saved_pstore(&boot(OLD)).unwrap(), None);
    assert!(locked(&calls).is_empty());
}

#[test]
fn real_fs_an_absent_or_empty_archive_runs_no_command() {
    let missing = tempfile::tempdir().unwrap();
    let (mut kernel, calls) = kernel_over(&missing.path().join("none"), three_boots());
    assert_eq!(kernel.saved_pstore(&boot(OLD)).unwrap(), None);
    let (mut kernel, calls_empty) = kernel_over(missing.path(), three_boots());
    assert_eq!(kernel.saved_pstore(&boot(OLD)).unwrap(), None);
    assert!(locked(&calls).is_empty() && locked(&calls_empty).is_empty());
}

#[test]
fn real_fs_entries_that_are_not_archive_directories_are_skipped() {
    let root = archive(&[("150/001/dmesg.txt", b"Panic#1 Part1\nreal")]);
    let elsewhere = archive(&[("160/001/dmesg.txt", b"Panic#9 Part1\nplanted")]);
    symlink(elsewhere.path().join("160"), root.path().join("170")).unwrap();
    directory(&root.path().join("180/12"));
    directory(&root.path().join("0/001"));
    directory(&root.path().join("-5/001"));
    directory(&root.path().join("+150/001"));
    directory(&root.path().join("150/1234"));
    for planted in ["150/abc", "150/+12"] {
        directory(&root.path().join(planted));
        fs::write(
            root.path().join(planted).join("dmesg.txt"),
            b"Panic#9 Part1\nplanted",
        )
        .unwrap();
    }
    directory(&root.path().join("150/003"));
    fs::write(
        root.path().join("150/004"),
        b"Panic#9 Part1\nnot a directory",
    )
    .unwrap();
    symlink(root.path().join("150/001"), root.path().join("150/002")).unwrap();
    let (mut kernel, _) = kernel_over(root.path(), three_boots());
    let got = kernel.saved_pstore(&boot(OLD)).unwrap().unwrap();
    assert!(got.path.ends_with("150/001/dmesg.txt"), "{got:?}");
}

#[test]
fn real_fs_a_stamp_directory_is_listed_only_when_it_is_in_the_span() {
    let root = archive(&[("150/001/dmesg.txt", b"Panic#1 Part1\nreal")]);
    directory(&root.path().join("90"));
    fs::set_permissions(root.path().join("90"), fs::Permissions::from_mode(0o777)).unwrap();
    let (mut kernel, _) = kernel_over(root.path(), three_boots());
    assert!(kernel.saved_pstore(&boot(OLD)).unwrap().is_some());
}

#[test]
fn tails_keep_the_last_32_lines_and_drop_a_cut_first_line() {
    let many = ["older\n".repeat(40), "final\n".to_owned()].concat();
    let mut want = vec!["older".to_owned(); 31];
    want.push("final".to_owned());
    assert_eq!(tail_lines(many.as_bytes(), false), want);

    assert_eq!(tail_lines(b"partial\nlast\n", true), ["last"]);
    assert_eq!(tail_lines(b"partial\nlast\n", false), ["partial", "last"]);
    assert_eq!(tail_lines(b"last", false), ["last"]);
    assert_eq!(tail_lines(b"a\n\nb\n\n", false), ["a", "", "b", ""]);
    assert_eq!(tail_lines(b"", false), [""]);
    assert_eq!(tail_lines(b"x", true), ["x"]);
    assert_eq!(tail_lines("界界".as_bytes().split_at(1).1, true), ["界"]);
    assert_eq!(tail_lines(b"\x80\x80ab", true), ["ab"]);
    assert_eq!(tail_lines(b"bad \xff byte\n", false), ["bad \u{fffd} byte"]);
}

#[test]
fn the_tail_window_is_the_budget_minus_the_header() {
    let header = b"Panic#1 Part1\n";
    let long = [&header[..], "x".repeat(5000).as_bytes()].concat();
    let got = saved(&[("150/001/dmesg.txt", &long)], OLD).unwrap();
    assert_eq!(got.lines, ["x".repeat(3968)]);

    let wide = [&header[..], "界".repeat(2000).as_bytes()].concat();
    let got = saved(&[("150/001/dmesg.txt", &wide)], OLD).unwrap();
    assert_eq!(got.lines, ["界".repeat(1322)]);

    let short_lines = [&header[..], "x".repeat(5000).as_bytes(), b"\nlast\n"].concat();
    let got = saved(&[("150/001/dmesg.txt", &short_lines)], OLD).unwrap();
    assert_eq!(got.lines, ["last"]);
}

#[test]
fn the_header_of_every_matching_chunk_must_identify_its_dump() {
    for (name, data) in [
        ("invalid header", &b"not a pstore dump\n"[..]),
        ("incomplete header", b"Panic#1 Part1"),
        ("invalid count", b"Panic#x Part1\n"),
        ("signed count", b"Panic#+1 Part1\n"),
        ("invalid part", b"Panic#1 Part0\n"),
        ("empty part", b"Panic#1 Part\n"),
        ("empty file", b""),
        (
            "unterminated within the header window",
            &[b"Panic#1 Part1".as_slice(), &[b'x'; 200]].concat(),
        ),
    ] {
        let (kernel, _) = memory_kernel(
            &[
                ("150/001/dmesg.txt", data),
                ("160/001/dmesg.txt", b"Panic#2 Part1\nfine"),
            ],
            three_boots(),
        );
        let error = kernel
            .read_saved_pstore(&JournalBoot::parse(OLD).unwrap())
            .unwrap_err();
        assert!(
            matches!(error, Error::Pstore(PstoreError::Identity { .. })),
            "{name}: {error:?}"
        );
    }
}

#[test]
fn dump_ids_come_from_the_first_line_only() {
    assert_eq!(
        DumpId::of_header(b"Panic#12 Part3\nignored #1 Part1"),
        DumpId::of_header(b"Oops#12 Part3\n")
    );
    assert!(DumpId::of_header(b"Panic#12 Part3\n").is_some());
    assert!(DumpId::of_header(b"\nPanic#12 Part3\n").is_none());
    assert!(DumpId::of_header(b"Panic#12 Part3 Part4\n").is_none());
}

#[test]
fn a_tail_read_cut_short_by_eof_is_an_error() {
    let file = b"abcd".to_vec();
    let mut window = [0; 16];

    assert_eq!(read_at(&file, &mut window, 0, false), Err(Errno::IO));
    assert_eq!(read_at(&file, &mut window, 0, true), Ok(4));
    assert_eq!(&window[..4], b"abcd");
}

#[test]
fn real_fs_an_unknown_entry_type_is_inspected_without_following_links() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    directory(&root.join("150"));
    fs::write(root.join("file"), b"x").unwrap();
    symlink(root.join("150"), root.join("link")).unwrap();
    let parent = OwnedFd::from(fs::File::open(root).unwrap());
    let inspect =
        |name: &str| is_directory(&parent, &OsString::from(name), FileType::Unknown, root);

    assert!(inspect("150").unwrap());
    assert!(!inspect("file").unwrap());
    assert!(!inspect("link").unwrap());
    let error = inspect("missing").unwrap_err();
    assert!(
        matches!(
            &error,
            PstoreError::Io { access: Access::Inspect, path, source }
                if *path == root.join("missing")
                    && source.kind() == std::io::ErrorKind::NotFound
        ),
        "{error:?}"
    );
}

#[test]
fn real_fs_a_root_that_is_not_a_directory_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("pstore");
    fs::write(&file, b"x").unwrap();
    let link = dir.path().join("link");
    symlink(dir.path(), &link).unwrap();
    for root in [&file, &link] {
        let (kernel, calls) = kernel_over(root, three_boots());
        let error = kernel
            .read_saved_pstore(&JournalBoot::parse(OLD).unwrap())
            .unwrap_err();
        assert!(
            matches!(
                error,
                Error::Pstore(PstoreError::Io {
                    access: Access::Open,
                    ..
                })
            ),
            "{root:?}: {error:?}"
        );
        assert!(locked(&calls).is_empty());
    }
}

#[test]
fn real_fs_a_failed_open_of_a_matching_dump_keeps_its_io_cause() {
    let root = archive(&[("150/001/real.txt", b"Panic#1 Part1\nlast")]);
    let path = root.path().join("150/001/dmesg.txt");
    // A dump inside the requested boot's span that O_NOFOLLOW refuses, even to root.
    symlink(root.path().join("150/001/real.txt"), &path).unwrap();
    let (kernel, _) = kernel_over(root.path(), three_boots());
    let error = kernel
        .read_saved_pstore(&JournalBoot::parse(OLD).unwrap())
        .unwrap_err();
    assert!(
        matches!(
            &error,
            Error::Pstore(PstoreError::Io {
                access: Access::Open,
                path: failed_path,
                source,
            }) if *failed_path == path
                && source.raw_os_error() == Some(Errno::LOOP.raw_os_error())
        ),
        "{error:?}"
    );
    let cause = std::error::Error::source(&error)
        .unwrap()
        .downcast_ref::<std::io::Error>()
        .unwrap();
    assert_eq!(cause.raw_os_error(), Some(Errno::LOOP.raw_os_error()));
}

#[test]
fn real_fs_unsafe_archive_entries_are_refused_from_the_descriptor() {
    let read = |root: &Path| {
        let (kernel, _) = kernel_over(root, three_boots());
        kernel.read_saved_pstore(&JournalBoot::parse(OLD).unwrap())
    };

    let linked = archive(&[("150/001/dmesg.txt", b"Panic#1 Part1\nx")]);
    fs::hard_link(
        linked.path().join("150/001/dmesg.txt"),
        linked.path().join("alias"),
    )
    .unwrap();
    assert!(matches!(
        read(linked.path()),
        Err(Error::Pstore(PstoreError::Unsafe {
            reason: Unsafe::Linked,
            ..
        }))
    ));

    let symlinked = archive(&[("150/001/real.txt", b"Panic#1 Part1\nx")]);
    symlink(
        symlinked.path().join("150/001/real.txt"),
        symlinked.path().join("150/001/dmesg.txt"),
    )
    .unwrap();
    assert!(matches!(
        read(symlinked.path()),
        Err(Error::Pstore(PstoreError::Io {
            access: Access::Open,
            ..
        }))
    ));

    let fifo = archive(&[]);
    directory(&fifo.path().join("150/001"));
    nix::unistd::mkfifo(
        &fifo.path().join("150/001/dmesg.txt"),
        nix::sys::stat::Mode::from_bits_truncate(0o600),
    )
    .unwrap();
    assert!(matches!(
        read(fifo.path()),
        Err(Error::Pstore(PstoreError::Unsafe {
            reason: Unsafe::NotRegular,
            ..
        }))
    ));

    let writable = archive(&[("150/001/dmesg.txt", b"Panic#1 Part1\nx")]);
    fs::set_permissions(
        writable.path().join("150"),
        fs::Permissions::from_mode(0o777),
    )
    .unwrap();
    assert!(matches!(
        read(writable.path()),
        Err(Error::Pstore(PstoreError::Unsafe {
            reason: Unsafe::Writable,
            ..
        }))
    ));

    let root_writable = archive(&[("150/001/dmesg.txt", b"Panic#1 Part1\nx")]);
    fs::set_permissions(root_writable.path(), fs::Permissions::from_mode(0o775)).unwrap();
    assert!(matches!(
        read(root_writable.path()),
        Err(Error::Pstore(PstoreError::Unsafe {
            reason: Unsafe::Writable,
            ..
        }))
    ));
}

#[test]
fn real_fs_group_or_other_writable_dumps_are_refused() {
    for mode in [0o620, 0o602, 0o666] {
        let root = archive(&[("150/001/dmesg.txt", b"Panic#1 Part1\nx")]);
        let path = root.path().join("150/001/dmesg.txt");
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        let (kernel, _) = kernel_over(root.path(), three_boots());
        let result = kernel.read_saved_pstore(&JournalBoot::parse(OLD).unwrap());
        assert!(
            matches!(
                &result,
                Err(Error::Pstore(PstoreError::Unsafe {
                    path: failed_path,
                    reason: Unsafe::Writable,
                })) if *failed_path == path
            ),
            "mode {mode:o}: {result:?}"
        );
    }
}

#[test]
fn a_boot_list_that_cannot_be_read_is_an_error_not_an_absent_record() {
    let cases: [(&str, Reply); 4] = [
        (
            "command",
            Box::new(|| {
                Err(crate::runner::RunError::Command(
                    togi_command::Error::TimedOut {
                        program: "journalctl".into(),
                        timeout: Duration::from_secs(1),
                    },
                ))
            }),
        ),
        ("malformed list", Box::new(|| ok("bad JSON"))),
        ("nonzero exit", Box::new(|| exit(2, b"", "unreadable"))),
        ("empty exit 1", Box::new(|| exit(1, b"", ""))),
    ];
    for (name, reply) in cases {
        let (mut kernel, _) = scripted(
            &[],
            pstore(&[("150/001/dmesg.txt", b"last")]),
            JOURNALCTL_BUDGET,
            move |_| reply(),
        );
        let error = kernel.saved_pstore(&boot(OLD)).unwrap_err();
        assert!(!matches!(error, togi_machine::Error::BootMissing), "{name}");
        if name == "nonzero exit" {
            assert!(error.to_string().contains("exit status: 2"), "{error}");
        }
    }
}

#[test]
fn an_unknown_boot_span_records_no_dump() {
    for (name, list) in [
        ("missing timestamps", boots(&[(OLD, None), (LATER, None)])),
        (
            "null timestamps",
            br#"[{"boot_id":"x","first_entry":null}]"#.to_vec(),
        ),
        (
            "clock moved backwards",
            boots(&[(OLD, Some(300_000_000)), (LATER, Some(100_000_000))]),
        ),
        (
            "equal timestamps",
            boots(&[(OLD, Some(100_000_000)), (LATER, Some(100_000_000))]),
        ),
        (
            "no start",
            boots(&[(OLD, Some(0)), (LATER, Some(200_000_000))]),
        ),
    ] {
        let (mut kernel, _) = memory_kernel(
            &[("150/001/dmesg.txt", b"Panic#1 Part1\nunrelated dump")],
            list,
        );
        assert_eq!(kernel.saved_pstore(&boot(OLD)).unwrap(), None, "{name}");
    }
}

#[test]
fn a_dump_survives_a_lost_journal_tail() {
    let list = boots(&[(OLD, Some(100_000_000)), (LATER, Some(200_000_000))]);
    let (mut kernel, calls) = scripted(
        &[],
        pstore(&[("150/001/dmesg.txt", b"Panic#1 Part1\nkernel page dump\n")]),
        JOURNALCTL_BUDGET,
        move |args| {
            if args[0] == "--list-boots" {
                ok(&list)
            } else {
                exit(1, b"", "journal tail is corrupt")
            }
        },
    );
    let got = kernel.saved_pstore(&boot(OLD)).unwrap().unwrap();
    assert_eq!(got.lines, ["Panic#1 Part1", "kernel page dump"]);
    let calls = locked(&calls);
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].args,
        ["--list-boots", "-o", "json", "--no-pager", "-q"]
    );
}
