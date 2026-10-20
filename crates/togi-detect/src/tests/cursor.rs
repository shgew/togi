use std::time::Duration;

use serde_json::{Value, json};
use test_case::test_case;
use togi_machine::{BankType, Kernel as _, KernelCursor, Partial};

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

fn unresumable(out: Vec<u8>, after: Option<&str>) -> Partial<togi_machine::KernelRead> {
    let (mut kernel, _) = kernel(&[(1, &[2])], move |_| ok(&out));
    let after = after.map(cursor);
    kernel.read_mces(&boot(OLD), after.as_ref()).unwrap_err()
}

#[test]
fn cursor_reads_keep_empty_intervals_resumable() {
    let initial = lines(&[entry("first", OLD, "Linux version 6.16", 0)]);
    let status = lines(&[entry("mce", OLD, STATUS, 1_000_000)]);
    let last = lines(&[entry("last", OLD, "ordinary kernel message", 2_000_000)]);
    let (anchor, mce, tail) = (initial.clone(), status, last);
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
    let fields = "--output-fields=MESSAGE,__REALTIME_TIMESTAMP,__MONOTONIC_TIMESTAMP,__CURSOR,_BOOT_ID,COREDUMP_PID,COREDUMP_COMM,COREDUMP_SIGNAL,COREDUMP_CGROUP";
    let base = [
        "-b",
        OLD,
        "-o",
        "json",
        fields,
        "--no-pager",
        "-q",
        "_TRANSPORT=kernel",
        "+",
        "MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1",
    ];
    assert_eq!(argv(&calls[0]), base);
    assert_eq!(argv(&calls[1]), [&base[..], &["--cursor=first"]].concat());
    assert_eq!(argv(&calls[2]), [&base[..], &["--cursor=last"]].concat());
}

#[test_case(lines(&[entry("nearest", OLD, "ordinary", 1)]), Some("gone"); "vacuumed cursor")]
#[test_case(lines(&[entry("saved", OTHER, "ordinary", 1)]), Some("saved"); "wrong boot")]
#[test_case(Vec::new(), Some("saved"); "past tail")]
#[test_case(lines(&[entry("", OLD, "ordinary", 1)]), None; "missing metadata")]
#[test_case(lines(&[json!({"_BOOT_ID": OLD, "MESSAGE": "x"})]), None; "no cursor field")]
#[test_case(lines(&[json!({"__CURSOR": "c", "MESSAGE": "x"})]), None; "no boot field")]
#[test_case(Vec::new(), None; "empty boot")]
fn a_cursor_that_is_not_the_first_row_or_in_another_boot_is_unresumable(
    out: Vec<u8>,
    after: Option<&str>,
) {
    let partial = unresumable(out, after);
    assert!(matches!(partial.error, togi_machine::Error::CursorMissing));
    assert!(partial.value.mces.is_empty());
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
fn an_ordinary_message_between_a_status_and_its_bank_keeps_the_bank() {
    let out = lines(&[
        entry("mce", OLD, STATUS, 1_000_000),
        entry("other", OLD, "ordinary kernel message", 1_000_001),
        entry(
            "bank",
            OLD,
            "[Hardware Error]: Decode Unit Ext. Error Code: 1",
            1_000_002,
        ),
    ]);
    let (mut kernel, _) = kernel(&[(1, &[2])], move |_| ok(&out));
    let read = kernel.read_mces(&boot(OLD), None).unwrap();
    assert_eq!(read.mces.len(), 1);
    assert_eq!(read.mces[0].bank_type, BankType::DecodeUnit);
    assert_eq!(read.cursor, cursor("bank"));
}

#[test]
fn a_malformed_entry_keeps_the_earlier_checks_and_no_cursor() {
    let mut out = lines(&[entry("mce", OLD, STATUS, 1_000_000)]);
    out.extend(b"{broken\n");
    let partial = unresumable(out, None);
    assert_eq!(partial.value.mces.len(), 1);
    assert!(partial.value.mces[0].corrected);
    assert_eq!(partial.value.mces[0].core, Some(core(1)));
    assert!(caused_by::<DecodeError>(&partial).is_some());
}

#[test]
fn an_earlier_check_on_an_unknown_cpu_keeps_no_core_before_a_malformed_entry() {
    let mut out = lines(&[entry("mce", OLD, STATUS, 1_000_000)]);
    out.extend(b"{broken\n");
    let (mut kernel, _) = kernel(&[], move |_| ok(&out));
    let partial = kernel.read_mces(&boot(OLD), None).unwrap_err();
    assert_eq!(partial.value.mces.len(), 1);
    assert_eq!(partial.value.mces[0].core, None);
    assert!(caused_by::<DecodeError>(&partial).is_some());
}

#[test_case(
    "[Hardware Error]: CPU:abc (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135",
    &StatusError::Cpu("abc".to_owned());
    "cpu"
)]
#[test_case(
    "[Hardware Error]: CPU:2 (1a:44:0) MCabc_STATUS[Over|CE|-]: 0xbc00000000010135",
    &StatusError::Bank("abc".to_owned());
    "bank"
)]
#[test_case(
    "[Hardware Error]: CPU abc: Machine Check: 0 Bank 1: bc00000000010135",
    &StatusError::Cpu("abc".to_owned());
    "raw cpu"
)]
#[test_case(
    "[Hardware Error]: CPU 2: Machine Check: 0 Bank abc: bc00000000010135",
    &StatusError::Bank("abc".to_owned());
    "raw bank"
)]
fn a_status_line_with_a_non_numeric_cpu_or_bank_stops_the_read_without_a_cursor(
    text: &str,
    error: &StatusError,
) {
    let out = lines(&[
        entry("a", OLD, STATUS, 1_000_000),
        entry("b", OLD, text, 2_000_000),
        entry("c", OLD, "ordinary kernel message", 3_000_000),
    ]);
    let partial = unresumable(out, None);
    assert_eq!(partial.value.mces.len(), 1);
    assert_eq!(partial.value.mces[0].core, Some(core(1)));
    assert_eq!(
        caused_by::<StatusError>(&partial),
        Some(error),
        "{}",
        partial.error
    );
}

#[test_case("", OLD; "no cursor")]
#[test_case("wrong-boot", OTHER; "other boot")]
fn lost_metadata_keeps_only_the_checks_read_before_the_bad_row(cursor: &str, boot: &str) {
    let mut out = lines(&[entry("valid", OLD, STATUS, 1_000_000)]);
    out.extend(lines(&[entry(cursor, boot, STATUS, 2_000_000)]));
    let (mut kernel, _) = kernel(&[(1, &[2])], move |_| ok(&out));
    let partial = kernel
        .read_mces(&super::support::boot(OLD), None)
        .unwrap_err();
    assert!(matches!(partial.error, togi_machine::Error::CursorMissing));
    assert_eq!(partial.value.mces.len(), 1);
    assert_eq!(partial.value.mces[0].monotonic, Duration::from_secs(1));
    assert_eq!(partial.value.mces[0].core, Some(core(1)));
}

#[test_case(
    Box::new(|| {
        Err(crate::runner::RunError::Command(
            togi_command::Error::Spawn {
                program: "journalctl".into(),
                source: std::io::ErrorKind::NotFound.into(),
            },
        ))
    }),
    None,
    "cannot read the kernel log";
    "transport"
)]
#[test_case(
    Box::new(|| exit(1, b"", "No journal boot entry found")),
    None,
    "boot missing";
    "missing boot"
)]
#[test_case(
    Box::new(|| exit(1, b"", "Failed to seek to cursor")),
    Some("saved"),
    "cannot be resumed";
    "vacuumed cursor"
)]
#[test_case(
    Box::new(|| exit(2, b"", "permission denied")),
    None,
    "permission denied";
    "other exit"
)]
fn observation_failures_never_become_clean_intervals(
    reply: Reply,
    after: Option<&str>,
    text: &str,
) {
    let (mut kernel, _) = kernel(&[], move |_| reply());
    let after = after.map(cursor);
    let partial = kernel.read_mces(&boot(OLD), after.as_ref()).unwrap_err();
    assert!(partial.value.mces.is_empty());
    assert!(
        partial.error.to_string().contains(text),
        "{}",
        partial.error
    );
}

#[test]
fn a_seek_failure_without_a_cursor_is_not_a_missing_cursor() {
    let (mut seek, _) = kernel(&[], |_| exit(1, b"", "Failed to seek to cursor"));
    let partial = seek.read_mces(&boot(OLD), None).unwrap_err();
    assert!(!matches!(partial.error, togi_machine::Error::CursorMissing));
}

#[test]
fn a_silent_failure_is_neither_a_missing_boot_nor_a_missing_cursor() {
    let (mut silent, _) = kernel(&[], |_| exit(1, b"", ""));
    let partial = silent.read_mces(&boot(OLD), None).unwrap_err();
    assert!(!matches!(
        partial.error,
        togi_machine::Error::BootMissing | togi_machine::Error::CursorMissing
    ));
}

#[test_case(&json!({}), "2000000", "1000000"; "invalid message")]
#[test_case(&json!("ordinary"), "invalid", "1000000"; "invalid wall clock")]
#[test_case(&json!("ordinary"), "2000000", "invalid"; "invalid monotonic clock")]
fn malformed_messages_keep_only_earlier_evidence(message: &Value, wall: &str, monotonic: &str) {
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
    assert!(caused_by::<DecodeError>(&partial).is_some());
    assert_eq!(partial.value.mces.len(), 1);
    assert!(partial.value.mces[0].corrected);
    assert_eq!(partial.value.mces[0].core, Some(core(1)));
    let partial = kernel.mces(&boot(OLD), Duration::ZERO).unwrap_err();
    assert!(caused_by::<DecodeError>(&partial).is_some());
    assert_eq!(partial.value.len(), 1);
    assert!(partial.value[0].corrected);
    assert_eq!(partial.value[0].core, Some(core(1)));
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
    assert_eq!(partial.value.mces.len(), 1);
}

#[test]
fn a_failed_read_keeps_the_error_kind_and_the_command_cause() {
    let (mut seek, _) = kernel(&[], |_| exit(1, b"", "Failed to seek to cursor"));
    let partial = seek
        .read_mces(&boot(OLD), Some(&cursor("saved")))
        .unwrap_err();
    assert!(matches!(partial.error, togi_machine::Error::CursorMissing));
    assert!(partial.value.mces.is_empty());

    let (mut missing, _) = kernel(&[], |_| exit(1, b"", "No journal boot entry found"));
    let partial = missing
        .read_mces(&boot(OLD), Some(&cursor("saved")))
        .unwrap_err();
    assert!(matches!(partial.error, togi_machine::Error::BootMissing));
    assert!(partial.value.mces.is_empty());

    let (mut transport, _) = kernel(&[], |_| {
        Err(crate::runner::RunError::Command(
            togi_command::Error::Spawn {
                program: "journalctl".into(),
                source: std::io::Error::other("journal unavailable"),
            },
        ))
    });
    let partial = transport.read_mces(&boot(OLD), None).unwrap_err();
    assert!(partial.value.mces.is_empty());
    assert!(
        caused_by::<togi_command::Error>(&partial)
            .is_some_and(|source| matches!(source, togi_command::Error::Spawn { .. })),
        "{partial:?}"
    );
}

#[test]
fn trap_location_and_coredump_abort_survive_a_later_decode_error() {
    let mut abort = entry("abort", OLD, "Process 314 (togi) dumped core.", 2_000_000);
    abort["COREDUMP_PID"] = json!("314");
    abort["COREDUMP_COMM"] = json!("togi");
    abort["COREDUMP_SIGNAL"] = json!("6");
    abort["COREDUMP_CGROUP"] = json!("/system.slice/togi.service");
    let mut out = lines(&[
        entry(
            "trap",
            OLD,
            "traps: mprime[99] general protection fault ip:55 sp:66 error:0 on CPU 2",
            1_000_000,
        ),
        abort,
    ]);
    out.extend(b"{broken\n");
    let (mut kernel, _) = kernel(&[(1, &[2])], move |_| ok(&out));
    let partial = kernel.read_mces(&boot(OLD), None).unwrap_err();
    assert_eq!(partial.value.faults.len(), 2);
    let trap = partial
        .value
        .faults
        .iter()
        .find(|fault| fault.pid == 99)
        .unwrap();
    assert_eq!(trap.cpu, Some(togi_machine::CpuId::from(2)));
    assert_eq!(trap.core, Some(core(1)));
    let abort = partial
        .value
        .faults
        .iter()
        .find(|fault| fault.pid == 314)
        .unwrap();
    assert_eq!(abort.kind, togi_machine::ProcessFaultKind::Abort);
    assert_eq!(abort.scope.as_deref(), Some("/system.slice/togi.service"));
    assert_eq!(abort.cpu, None);
    assert_eq!(partial.value.cursor.as_str(), "");
    assert!(caused_by::<DecodeError>(&partial).is_some());
}

#[test_case("4", togi_machine::ProcessFaultKind::IllegalInstruction; "sigill")]
#[test_case("6", togi_machine::ProcessFaultKind::Abort; "sigabrt")]
#[test_case("7", togi_machine::ProcessFaultKind::Bus; "sigbus")]
#[test_case("11", togi_machine::ProcessFaultKind::Segmentation; "sigsegv")]
fn coredump_signals_are_typed_without_parsing_the_localized_message(
    signal: &str,
    kind: togi_machine::ProcessFaultKind,
) {
    let mut record = entry("dump", OLD, "localized human-readable summary", 1_000_000);
    record["COREDUMP_PID"] = json!("821");
    record["COREDUMP_COMM"] = json!("mprime");
    record["COREDUMP_SIGNAL"] = json!(signal);
    let out = lines(&[record]);
    let (mut kernel, _) = kernel(&[], move |_| ok(&out));
    let read = kernel.read_mces(&boot(OLD), None).unwrap();
    assert_eq!(read.faults.len(), 1);
    assert_eq!(read.faults[0].kind, kind);
    assert_eq!(read.faults[0].pid, 821);
    assert_eq!(read.cursor, cursor("dump"));
}
