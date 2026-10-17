use std::time::Duration;

use serde_json::{Value, json};
use togi_machine::{Kernel as _, KernelCursor, Partial};

use super::support::{OLD, Reply, argv, boot, caused_by, core, exit, kernel, lines, locked, ok};
use crate::{DecodeError, StatusError};

const OTHER: &str = "ef01ef01ef01ef01ef01ef01ef01ef01";
const STATUS: &str = "[Hardware Error]: CPU:2 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135";

fn entry(cursor: &str, boot: &str, message: &str, monotonic: i64) -> Value {
    json!({
        "__CURSOR": cursor,
        "_BOOT_ID": boot,
        "MESSAGE": message,
        "__REALTIME_TIMESTAMP": "2000000",
        "__MONOTONIC_TIMESTAMP": monotonic.to_string(),
    })
}

fn cursor(text: &str) -> KernelCursor {
    KernelCursor::from(text.to_owned())
}

fn unresumable(out: Vec<u8>, after: Option<&str>) -> Partial<Vec<togi_machine::Mce>> {
    let (mut kernel, _) = kernel(&[(1, &[2])], move |_| ok(&out));
    let after = after.map(cursor);
    kernel.read_mces(&boot(OLD), after.as_ref()).unwrap_err()
}

#[test]
fn cursor_reads_keep_empty_intervals_resumable() {
    let initial = lines(&[entry("first", OLD, "Linux version 6.16", 0)]);
    let status = lines(&[entry("mce", OLD, STATUS, 1_000_000)]);
    let last = lines(&[entry("last", OLD, "ordinary kernel message", 2_000_000)]);
    let (anchor, mce, tail) = (initial.clone(), status, last.clone());
    let (mut kernel, calls) = kernel(&[(1, &[2])], move |args| {
        match args.last().map(String::as_str) {
            Some("--cursor=last") => ok(&tail),
            Some(arg) if arg.starts_with("--cursor=") => {
                ok([anchor.clone(), mce.clone(), tail.clone()].concat())
            }
            _ => ok(&initial),
        }
    });
    let first = kernel
        .read_mces(&boot("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"), None)
        .unwrap();
    assert!(first.mces.is_empty());
    assert_eq!(first.cursor, cursor("first"));
    let next = kernel.read_mces(&boot(OLD), Some(&first.cursor)).unwrap();
    assert_eq!(next.cursor, cursor("last"));
    assert_eq!(next.mces.len(), 1);
    assert_eq!(next.mces[0].core, Some(core(1)));
    assert_eq!(next.mces[0].monotonic, Duration::from_secs(1));
    let empty = kernel.read_mces(&boot(OLD), Some(&next.cursor)).unwrap();
    assert!(empty.mces.is_empty());
    assert_eq!(empty.cursor, cursor("last"));

    let calls = locked(&calls);
    let fields =
        "--output-fields=MESSAGE,__REALTIME_TIMESTAMP,__MONOTONIC_TIMESTAMP,__CURSOR,_BOOT_ID";
    let base = ["-k", "-b", OLD, "-o", "json", fields, "--no-pager", "-q"];
    assert_eq!(argv(&calls[0]), base);
    assert_eq!(argv(&calls[1]), [&base[..], &["--cursor=first"]].concat());
    assert_eq!(argv(&calls[2]), [&base[..], &["--cursor=last"]].concat());
}

#[test]
fn a_cursor_that_is_not_the_first_row_or_in_another_boot_is_unresumable() {
    let cases = [
        (
            "vacuumed cursor",
            lines(&[entry("nearest", OLD, "ordinary", 1)]),
            Some("gone"),
        ),
        (
            "wrong boot",
            lines(&[entry("saved", OTHER, "ordinary", 1)]),
            Some("saved"),
        ),
        ("past tail", Vec::new(), Some("saved")),
        (
            "missing metadata",
            lines(&[entry("", OLD, "ordinary", 1)]),
            None,
        ),
        (
            "no cursor field",
            lines(&[json!({"_BOOT_ID": OLD, "MESSAGE": "x"})]),
            None,
        ),
        (
            "no boot field",
            lines(&[json!({"__CURSOR": "c", "MESSAGE": "x"})]),
            None,
        ),
        ("empty boot", Vec::new(), None),
    ];
    for (name, out, after) in cases {
        let partial = unresumable(out, after);
        assert!(
            matches!(partial.error, togi_machine::Error::CursorMissing),
            "{name}"
        );
        assert!(partial.value.is_empty(), "{name}");
    }
}

#[test]
fn an_empty_cursor_reads_the_whole_boot() {
    let out = lines(&[entry("first", OLD, "Linux version 6.16", 0)]);
    let (mut kernel, calls) = kernel(&[], move |_| ok(&out));
    let read = kernel
        .read_mces(&boot(OLD), Some(&KernelCursor::default()))
        .unwrap();
    assert_eq!(read.cursor, cursor("first"));
    assert!(
        !argv(&locked(&calls)[0])
            .iter()
            .any(|arg| arg.starts_with("--cursor"))
    );
}

#[test]
fn a_malformed_entry_keeps_the_earlier_checks_and_no_cursor() {
    let mut out = lines(&[entry("mce", OLD, STATUS, 1_000_000)]);
    out.extend(b"{broken\n");
    let partial = unresumable(out, None);
    assert_eq!(partial.value.len(), 1);
    assert!(partial.value[0].corrected);
    assert_eq!(partial.value[0].core, Some(core(1)));
    assert!(caused_by::<DecodeError>(&partial).is_some());
}

#[test]
fn an_earlier_check_on_an_unknown_cpu_keeps_no_core_before_a_malformed_entry() {
    let mut out = lines(&[entry("mce", OLD, STATUS, 1_000_000)]);
    out.extend(b"{broken\n");
    let (mut kernel, _) = kernel(&[], move |_| ok(&out));
    let partial = kernel.read_mces(&boot(OLD), None).unwrap_err();
    assert_eq!(partial.value.len(), 1);
    assert_eq!(partial.value[0].core, None);
    assert!(caused_by::<DecodeError>(&partial).is_some());
}

#[test]
fn a_status_line_with_a_non_numeric_cpu_or_bank_stops_the_read_without_a_cursor() {
    for (name, text, error) in [
        (
            "cpu",
            "[Hardware Error]: CPU:abc (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135",
            StatusError::Cpu("abc".to_owned()),
        ),
        (
            "bank",
            "[Hardware Error]: CPU:2 (1a:44:0) MCabc_STATUS[Over|CE|-]: 0xbc00000000010135",
            StatusError::Bank("abc".to_owned()),
        ),
        (
            "raw cpu",
            "[Hardware Error]: CPU abc: Machine Check: 0 Bank 1: bc00000000010135",
            StatusError::Cpu("abc".to_owned()),
        ),
        (
            "raw bank",
            "[Hardware Error]: CPU 2: Machine Check: 0 Bank abc: bc00000000010135",
            StatusError::Bank("abc".to_owned()),
        ),
    ] {
        let out = lines(&[
            entry("a", OLD, STATUS, 1_000_000),
            entry("b", OLD, text, 2_000_000),
            entry("c", OLD, "ordinary kernel message", 3_000_000),
        ]);
        let partial = unresumable(out, None);
        assert_eq!(partial.value.len(), 1, "{name}");
        assert_eq!(partial.value[0].core, Some(core(1)), "{name}");
        assert_eq!(
            caused_by::<StatusError>(&partial),
            Some(&error),
            "{name}: {}",
            partial.error
        );
    }
}

#[test]
fn lost_metadata_keeps_only_the_checks_read_before_the_bad_row() {
    for (name, cursor, boot) in [("no cursor", "", OLD), ("other boot", "wrong-boot", OTHER)] {
        let mut out = lines(&[entry("valid", OLD, STATUS, 1_000_000)]);
        out.extend(lines(&[entry(cursor, boot, STATUS, 2_000_000)]));
        let (mut kernel, _) = kernel(&[(1, &[2])], move |_| ok(&out));
        let partial = kernel
            .read_mces(&super::support::boot(OLD), None)
            .unwrap_err();
        assert!(
            matches!(partial.error, togi_machine::Error::CursorMissing),
            "{name}"
        );
        assert_eq!(partial.value.len(), 1, "{name}");
        assert_eq!(partial.value[0].monotonic, Duration::from_secs(1), "{name}");
        assert_eq!(partial.value[0].core, Some(core(1)), "{name}");
    }
}

#[test]
fn observation_failures_never_become_clean_intervals() {
    let cases: [(&str, Reply, Option<&str>, &str); 4] = [
        (
            "transport",
            Box::new(|| {
                Err(crate::runner::RunError::Command(
                    togi_command::Error::Spawn {
                        program: "journalctl".into(),
                        source: std::io::ErrorKind::NotFound.into(),
                    },
                ))
            }),
            None,
            "cannot read the kernel log",
        ),
        (
            "missing boot",
            Box::new(|| exit(1, b"", "No journal boot entry found")),
            None,
            "boot missing",
        ),
        (
            "vacuumed cursor",
            Box::new(|| exit(1, b"", "Failed to seek to cursor")),
            Some("saved"),
            "cannot be resumed",
        ),
        (
            "other exit",
            Box::new(|| exit(2, b"", "permission denied")),
            None,
            "permission denied",
        ),
    ];
    for (name, reply, after, text) in cases {
        let (mut kernel, _) = kernel(&[], move |_| reply());
        let after = after.map(cursor);
        let partial = kernel.read_mces(&boot(OLD), after.as_ref()).unwrap_err();
        assert!(partial.value.is_empty(), "{name}");
        assert!(
            partial.error.to_string().contains(text),
            "{name}: {}",
            partial.error
        );
    }
    let (mut seek, _) = kernel(&[], |_| exit(1, b"", "Failed to seek to cursor"));
    let partial = seek.read_mces(&boot(OLD), None).unwrap_err();
    assert!(!matches!(partial.error, togi_machine::Error::CursorMissing));
    let (mut silent, _) = kernel(&[], |_| exit(1, b"", ""));
    let partial = silent.read_mces(&boot(OLD), None).unwrap_err();
    assert!(!matches!(
        partial.error,
        togi_machine::Error::BootMissing | togi_machine::Error::CursorMissing
    ));
}

#[test]
fn malformed_messages_keep_only_earlier_evidence() {
    let cases = [
        ("invalid message", json!({}), "2000000", "1000000"),
        (
            "invalid wall clock",
            json!("ordinary"),
            "invalid",
            "1000000",
        ),
        (
            "invalid monotonic clock",
            json!("ordinary"),
            "2000000",
            "invalid",
        ),
    ];
    for (name, message, wall, monotonic) in cases {
        let mut out = lines(&[entry("valid", OLD, STATUS, 1_000_000)]);
        out.extend(lines(&[json!({
            "MESSAGE": message,
            "__REALTIME_TIMESTAMP": wall,
            "__MONOTONIC_TIMESTAMP": monotonic,
            "__CURSOR": "bad",
            "_BOOT_ID": OLD,
        })]));
        let (mut kernel, _) = kernel(&[(1, &[2])], move |_| ok(&out));
        let partial = kernel.read_mces(&boot(OLD), None).unwrap_err();
        assert!(caused_by::<DecodeError>(&partial).is_some(), "{name}");
        assert_eq!(partial.value.len(), 1, "{name}");
        assert!(partial.value[0].corrected, "{name}");
        assert_eq!(partial.value[0].core, Some(core(1)), "{name}");
        let partial = kernel.mces(&boot(OLD), Duration::ZERO).unwrap_err();
        assert!(caused_by::<DecodeError>(&partial).is_some(), "{name}");
        assert_eq!(partial.value.len(), 1, "{name}");
        assert!(partial.value[0].corrected, "{name}");
        assert_eq!(partial.value[0].core, Some(core(1)), "{name}");
    }
}

#[test]
fn a_whole_boot_read_keeps_checks_before_a_malformed_status_line() {
    let out = lines(&[
        entry("valid", OLD, STATUS, 1_000_000),
        entry(
            "bad",
            OLD,
            "[Hardware Error]: CPU:abc (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135",
            2_000_000,
        ),
    ]);
    let (mut kernel, _) = kernel(&[(1, &[2])], move |_| ok(&out));
    let partial = kernel.mces(&boot(OLD), Duration::ZERO).unwrap_err();
    assert_eq!(partial.value.len(), 1);
    assert_eq!(partial.value[0].core, Some(core(1)));
    assert!(partial.value[0].corrected);
    assert_eq!(partial.value[0].monotonic, Duration::from_secs(1));
    assert_eq!(
        caused_by::<StatusError>(&partial),
        Some(&StatusError::Cpu("abc".to_owned()))
    );
}

#[test]
fn the_anchor_row_is_not_decoded() {
    let mut out = lines(&[json!({"__CURSOR": "saved", "_BOOT_ID": OLD, "MESSAGE": {}})]);
    out.extend(lines(&[entry("next", OLD, "ordinary", 1)]));
    let (mut kernel, _) = kernel(&[], move |_| ok(&out));
    let read = kernel
        .read_mces(&boot(OLD), Some(&cursor("saved")))
        .unwrap();
    assert_eq!(read.cursor, cursor("next"));
}

#[test]
fn a_row_with_an_impossible_cpu_stops_the_read_without_a_cursor() {
    let out = lines(&[
        entry("a", OLD, STATUS, 1),
        entry(
            "b",
            OLD,
            "[Hardware Error]: CPU:70000 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135",
            2,
        ),
    ]);
    let partial = unresumable(out, None);
    assert_eq!(partial.value.len(), 1);
}

#[test]
fn a_failed_read_keeps_the_error_kind_and_the_command_cause() {
    let (mut seek, _) = kernel(&[], |_| exit(1, b"", "Failed to seek to cursor"));
    let partial = seek
        .read_mces(&boot(OLD), Some(&cursor("saved")))
        .unwrap_err();
    assert!(matches!(partial.error, togi_machine::Error::CursorMissing));
    assert!(partial.value.is_empty());

    let (mut missing, _) = kernel(&[], |_| exit(1, b"", "No journal boot entry found"));
    let partial = missing
        .read_mces(&boot(OLD), Some(&cursor("saved")))
        .unwrap_err();
    assert!(matches!(partial.error, togi_machine::Error::BootMissing));
    assert!(partial.value.is_empty());

    let (mut transport, _) = kernel(&[], |_| {
        Err(crate::runner::RunError::Command(
            togi_command::Error::Spawn {
                program: "journalctl".into(),
                source: std::io::Error::other("journal unavailable"),
            },
        ))
    });
    let partial = transport.read_mces(&boot(OLD), None).unwrap_err();
    assert!(partial.value.is_empty());
    assert!(
        caused_by::<togi_command::Error>(&partial)
            .is_some_and(|source| matches!(source, togi_command::Error::Spawn { .. })),
        "{partial:?}"
    );
}
