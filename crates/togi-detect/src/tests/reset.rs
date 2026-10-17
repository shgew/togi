use serde_json::json;
use test_case::test_case;
use togi_machine::{Kernel as _, ResetKind, ResetReason};

use super::support::{
    CURRENT, LATER, MISSING, OLD, argv, boot, caused_by, exit, kernel, kernel_lines, locked, ok,
};
use crate::boot::JournalBoot;
use crate::reset::{Position, ResetScan};
use crate::{DecodeError, Error};

const THERMAL: &str =
    "x86/amd: Previous system reset reason [0x00000001]: internal CPU thermal limit was tripped";
const WATCHDOG: &str =
    "x86/amd: Previous system reset reason [0x00000001]: hardware watchdog timer expired";
const NO_BOOT: &str = "No journal boot entry found";

fn reason(kind: Option<ResetKind>, raw: &[&str], supported: bool) -> ResetReason {
    ResetReason {
        kind,
        raw: raw.iter().map(|text| (*text).to_owned()).collect(),
        supported,
    }
}

#[test_case("6.15", &[], None, false; "old kernel")]
#[test_case("6.16", &[], None, true; "boundary")]
#[test_case("7.0", &[], None, true; "new major")]
#[test_case("6.16", &["hardware watchdog timer expired"], Some(ResetKind::Watchdog), true; "watchdog")]
#[test_case(
    "6.16",
    &["an uncorrected error caused a data fabric sync flood event"],
    Some(ResetKind::SyncFlood),
    true;
    "fabric flood"
)]
#[test_case(
    "6.16",
    &["a software sync flood event occurred"],
    Some(ResetKind::SyncFlood),
    true;
    "software flood"
)]
#[test_case(
    "6.16",
    &["internal CPU shutdown event occurred"],
    Some(ResetKind::CpuShutdown),
    true;
    "cpu shutdown"
)]
#[test_case(
    "6.16",
    &["power button was pressed for 4 seconds"],
    Some(ResetKind::PowerButton),
    true;
    "power button"
)]
#[test_case(
    "6.16",
    &["thermal pin BP_THERMTRIP_L was tripped"],
    Some(ResetKind::ThermalTrip),
    true;
    "thermal pin"
)]
#[test_case(
    "6.16",
    &["internal CPU thermal limit was tripped"],
    Some(ResetKind::ThermalTrip),
    true;
    "thermal limit"
)]
#[test_case("6.16", &["unexpected reason"], Some(ResetKind::Unknown), true; "unknown")]
#[test_case(
    "6.16",
    &[
        "power button was pressed for 4 seconds",
        "hardware watchdog timer expired",
        "internal CPU thermal limit was tripped",
        "an uncorrected error caused a data fabric sync flood event",
    ],
    Some(ResetKind::ThermalTrip),
    true;
    "precedence"
)]
fn reset_reason_kinds_priority_and_support(
    version: &str,
    texts: &[&str],
    kind: Option<ResetKind>,
    supported: bool,
) {
    let mut messages = vec![format!("Linux version {version}.0-test")];
    messages.extend(
        texts
            .iter()
            .map(|text| format!("x86/amd: Previous system reset reason [0x00000001]: {text}")),
    );
    let out = kernel_lines(&messages.iter().map(String::as_str).collect::<Vec<_>>());
    let (mut kernel, calls) = kernel(&[], move |_| ok(&out));
    let got = kernel
        .reset_reason(&boot("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"))
        .unwrap();
    assert_eq!(got, reason(kind, texts, supported));
    assert_eq!(
        argv(&locked(&calls)[0]),
        [
            "-k",
            "-b",
            OLD,
            "-o",
            "json",
            "--output-fields=MESSAGE",
            "--grep",
            "Linux version|Previous system reset reason",
            "--no-pager",
            "-q",
        ]
    );
}

#[test]
fn a_vacuumed_boot_has_no_reset_reason() {
    let (mut kernel, _) = kernel(&[], |_| exit(1, b"", NO_BOOT));
    assert!(matches!(
        kernel.reset_reason(&boot(MISSING)),
        Err(togi_machine::Error::BootMissing)
    ));
}

#[test]
fn a_boot_without_matching_lines_is_unsupported_and_empty() {
    let (mut kernel, _) = kernel(&[], |_| exit(1, b"", ""));
    assert_eq!(
        kernel.reset_reason(&boot(OLD)).unwrap(),
        reason(None, &[], false)
    );
}

#[test_case(Vec::new(), 2; "exit")]
#[test_case(b"{".to_vec(), 0; "invalid json")]
#[test_case(br#"{"MESSAGE":{}}"#.to_vec(), 0; "invalid message")]
#[test_case(kernel_lines(&["Linux version 99999999999999999999.1"]), 0; "kernel version beyond u64")]
fn malformed_output_never_invents_a_reason(out: Vec<u8>, code: i32) {
    let (mut kernel, _) = kernel(&[], move |_| exit(code, &out, ""));
    assert!(kernel.reset_reason(&boot(OLD)).is_err());
}

#[test]
fn a_byte_array_message_is_read_as_text() {
    let bytes = json!({"MESSAGE": "Linux version 7.0".bytes().collect::<Vec<u8>>()}).to_string();
    let (mut kernel, _) = kernel(&[], move |_| ok(&bytes));
    assert_eq!(
        kernel.reset_reason(&boot(OLD)).unwrap(),
        reason(None, &[], true)
    );
}

#[test]
fn reset_lines_need_the_exact_prefix_and_a_reason() {
    let mut scan = ResetScan::default();
    for text in [
        "x86/amd: Previous system reset reason [0x0000001]: hardware watchdog timer expired",
        "x86/amd: Previous system reset reason [0x0000000G]: hardware watchdog timer expired",
        "x86/amd: Previous system reset reason [0x00000001]: ",
        "x86/amd: Previous system reset reason [0x00000001] hardware watchdog timer expired",
        "x86/amd: Previous system reset reason [0x00000001]: two\nlines",
        "prefix x86/amd: Previous system reset reason [0x00000001]: hardware watchdog timer expired",
        "Linux version 6.x",
        "Linux version x.16",
        "Linux version 6.",
        "linux version 7.0",
    ] {
        scan.push(text).unwrap();
    }
    assert_eq!(scan.finish(), reason(None, &[], false));
}

#[test]
fn only_a_kernel_log_line_after_the_version_prefix_sets_support() {
    let mut scan = ResetScan::default();
    scan.push("Linux version 6.16.0-rc1 (build)").unwrap();
    scan.push("Linux version 5.99").unwrap();
    assert!(scan.finish().supported);
}

fn boundary(boot: &str, series: &str, sequence: &str) -> Vec<u8> {
    json!({"_BOOT_ID": boot, "__SEQNUM_ID": series, "__SEQNUM": sequence})
        .to_string()
        .into_bytes()
}

fn boots(ids: &[&str]) -> Vec<u8> {
    json!(
        ids.iter()
            .map(|id| json!({"boot_id": id}))
            .collect::<Vec<_>>()
    )
    .to_string()
    .into_bytes()
}

fn successor_kernel(
    list: Vec<u8>,
    last: Vec<u8>,
    first: Vec<u8>,
    kernel_log: Vec<u8>,
) -> (crate::Kernel, super::support::Calls) {
    kernel(&[], move |args| match args[0].as_str() {
        "--list-boots" => ok(&list),
        "-b" if args[1] == OLD => ok(&last),
        "-b" => ok(&first),
        _ if args[2] == MISSING => exit(1, b"", NO_BOOT),
        _ => ok(&kernel_log),
    })
}

#[test]
fn the_reset_reason_after_a_boot_is_its_immediate_successors() {
    let log = kernel_lines(&["Linux version 7.2.8", WATCHDOG]);
    let (mut kernel, calls) = successor_kernel(
        boots(&[OLD, LATER, CURRENT]),
        boundary(OLD, "series", "100"),
        boundary(LATER, "series", "101"),
        log,
    );
    let got = kernel.reset_reason_after(&boot(OLD)).unwrap();
    assert_eq!(
        got,
        reason(
            Some(ResetKind::Watchdog),
            &["hardware watchdog timer expired"],
            true
        )
    );
    let calls = locked(&calls);
    assert_eq!(
        argv(&calls[0]),
        ["--list-boots", "-o", "json", "--no-pager", "-q"]
    );
    assert_eq!(
        argv(&calls[1]),
        [
            "-b",
            OLD,
            "-n",
            "1",
            "-o",
            "json",
            "--output-fields=_BOOT_ID,__SEQNUM,__SEQNUM_ID",
            "--no-pager",
            "-q",
            "-r"
        ]
    );
    assert_eq!(
        argv(&calls[2]),
        [
            "-b",
            LATER,
            "-n",
            "+1",
            "-o",
            "json",
            "--output-fields=_BOOT_ID,__SEQNUM,__SEQNUM_ID",
            "--no-pager",
            "-q"
        ]
    );
    assert_eq!(argv(&calls[3])[..4], ["-k", "-b", LATER, "-o"]);
    assert_eq!(calls.len(), 4);
}

#[test_case(boots(&[LATER, CURRENT]); "crashed boot missing")]
#[test_case(boots(&[OLD]); "no successor")]
#[test_case(boots(&[]); "no retained boots")]
#[test_case(boots(&[OLD, MISSING, CURRENT]); "successor log vacuumed")]
#[test_case(boots(&[OLD, ""]); "successor id empty")]
fn a_successor_that_cannot_be_found_or_read_is_a_missing_boot(list: Vec<u8>) {
    let log = kernel_lines(&["Linux version 7.2.8", THERMAL]);
    let (mut kernel, _) = successor_kernel(
        list,
        boundary(OLD, "series", "100"),
        boundary(MISSING, "series", "101"),
        log,
    );
    assert!(matches!(
        kernel.reset_reason_after(&boot(OLD)),
        Err(togi_machine::Error::BootMissing)
    ));
}

#[test_case(
    boundary(OLD, "series", "100"),
    boundary(LATER, "series", "101"),
    true;
    "continuous boundary"
)]
#[test_case(
    boundary(OLD, "series", "100"),
    boundary(LATER, "series", "200"),
    false;
    "omitted intermediate boot"
)]
#[test_case(
    boundary(OLD, "series", "100"),
    boundary(LATER, "other", "101"),
    false;
    "changed sequence identity"
)]
#[test_case(
    format!(r#"{{"_BOOT_ID":"{OLD}"}}"#).into_bytes(),
    boundary(LATER, "series", "101"),
    false;
    "missing predecessor metadata"
)]
#[test_case(
    boundary(OLD, "series", "100"),
    format!(r#"{{"_BOOT_ID":"{LATER}"}}"#).into_bytes(),
    false;
    "missing successor metadata"
)]
#[test_case(Vec::new(), boundary(LATER, "series", "101"), false; "missing predecessor tail")]
#[test_case(boundary(OLD, "series", "100"), Vec::new(), false; "missing successor start")]
#[test_case(
    boundary(CURRENT, "series", "100"),
    boundary(LATER, "series", "101"),
    false;
    "wrong boundary boot"
)]
#[test_case(
    boundary(OLD, "series", "18446744073709551615"),
    boundary(LATER, "series", "0"),
    false;
    "sequence wraparound"
)]
fn succession_needs_consecutive_sequence_numbers_of_one_journal(
    last: Vec<u8>,
    first: Vec<u8>,
    continuous: bool,
) {
    let log = kernel_lines(&["Linux version 7.2.8", THERMAL]);
    let (mut kernel, _) = successor_kernel(boots(&[OLD, LATER]), last, first, log);
    let got = kernel.reset_reason_after(&boot(OLD));
    if continuous {
        assert_eq!(
            got.unwrap(),
            reason(
                Some(ResetKind::ThermalTrip),
                &["internal CPU thermal limit was tripped"],
                true
            )
        );
    } else {
        assert!(matches!(got, Err(togi_machine::Error::BootMissing)));
    }
}

#[test]
fn positions_follow_each_other_by_exactly_one() {
    let at = |id: &str, sequence| Position::new(id.to_owned(), sequence).unwrap();
    assert!(at("s", 1).is_followed_by(&at("s", 2)));
    assert!(!at("s", 1).is_followed_by(&at("s", 3)));
    assert!(!at("s", 2).is_followed_by(&at("s", 1)));
    assert!(!at("s", 2).is_followed_by(&at("s", 2)));
    assert!(!at("s", 1).is_followed_by(&at("t", 2)));
    assert!(!at("s", u64::MAX).is_followed_by(&at("s", 1)));
    assert!(Position::new("s".to_owned(), 0).is_none());
}

#[test_case(b"", NO_BOOT, 1, true; "vacuumed boundary")]
#[test_case(b"", "", 1, true; "empty boundary")]
#[test_case(b"", "permission denied", 1, false; "permission error")]
#[test_case(b"{", "", 0, false; "malformed boundary")]
fn boundary_failures_are_told_apart(
    out: &'static [u8],
    stderr: &'static str,
    code: i32,
    missing: bool,
) {
    let list = boots(&[OLD, LATER]);
    let (kernel, _) = kernel(&[], move |args| {
        if args[0] == "--list-boots" {
            ok(&list)
        } else {
            exit(code, out, stderr)
        }
    });
    let got = kernel.read_reset_reason_after(&JournalBoot::parse(OLD).unwrap());
    assert_eq!(matches!(got, Err(Error::BootMissing)), missing);
    assert!(got.is_err());
}

#[test]
fn a_boundary_read_that_times_out_is_a_command_error() {
    let list = boots(&[OLD, LATER]);
    let (kernel, _) = kernel(&[], move |args| {
        if args[0] == "--list-boots" {
            ok(&list)
        } else {
            Err(crate::runner::RunError::Command(
                togi_command::Error::TimedOut {
                    program: "journalctl".into(),
                    timeout: std::time::Duration::from_secs(1),
                },
            ))
        }
    });
    let error = kernel
        .read_reset_reason_after(&JournalBoot::parse(OLD).unwrap())
        .unwrap_err();
    assert!(matches!(error, Error::Command { .. }));
    assert!(caused_by::<togi_command::Error>(&error).is_some());
}

#[test]
fn an_empty_system_journal_means_the_boot_is_missing() {
    let (mut kernel, _) = kernel(&[], |_| exit(1, b"", ""));
    assert!(matches!(
        kernel.reset_reason_after(&boot(OLD)),
        Err(togi_machine::Error::BootMissing)
    ));
}

type Exit = (i32, &'static [u8], &'static str);

#[test_case(None, true, false; "command error")]
#[test_case(Some((1, b"", "permission denied")), false, false; "nonzero exit")]
#[test_case(Some((0, b"[", "")), false, true; "malformed list")]
fn a_boot_list_that_cannot_be_read_is_never_clean_or_missing(
    reply: Option<Exit>,
    command_cause: bool,
    decode_cause: bool,
) {
    let (kernel, _) = kernel(&[], move |_| match reply {
        Some((code, out, stderr)) => exit(code, out, stderr),
        None => Err(crate::runner::RunError::Command(
            togi_command::Error::Spawn {
                program: "journalctl".into(),
                source: std::io::Error::other("journal unavailable"),
            },
        )),
    });
    let error = kernel
        .read_reset_reason_after(&JournalBoot::parse(OLD).unwrap())
        .unwrap_err();
    assert!(!matches!(error, Error::BootMissing));
    if command_cause {
        assert!(caused_by::<togi_command::Error>(&error).is_some());
    }
    if decode_cause {
        assert!(caused_by::<DecodeError>(&error).is_some());
    }
}

fn reset_line(text: &str) -> String {
    format!("x86/amd: Previous system reset reason [0x00000001]: {text}")
}

#[test_case(1, b"", "permission denied", Some("permission denied"); "exit status")]
#[test_case(0, b"{", "", None; "malformed entry")]
fn a_failed_reset_reason_read_names_the_operation_and_its_boot(
    code: i32,
    out: &'static [u8],
    stderr: &'static str,
    cause: Option<&str>,
) {
    let (mut kernel, _) = kernel(&[], move |_| exit(code, out, stderr));
    let error = kernel.reset_reason(&boot(OLD)).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("reset reason"), "{text}");
    assert!(!text.contains("journal boundary"), "{text}");
    assert!(text.contains(OLD), "{text}");
    if let Some(cause) = cause {
        assert!(text.contains(cause), "{text}");
    } else {
        assert!(caused_by::<DecodeError>(&error).is_some());
    }
}

#[test_case(OLD; "crashed boot")]
#[test_case(LATER; "successor")]
fn a_failed_boundary_read_names_the_operation_and_the_boot_it_read(failing: &'static str) {
    let list = boots(&[OLD, LATER]);
    let last = boundary(OLD, "series", "100");
    let (mut kernel, calls) = kernel(&[], move |args| match args[0].as_str() {
        "--list-boots" => ok(&list),
        "-b" if args[1] == failing => exit(1, b"", "permission denied"),
        "-b" => ok(&last),
        _ => ok(""),
    });
    let error = kernel.reset_reason_after(&boot(OLD)).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("journal boundary"), "{text}");
    assert!(!text.contains("reset reason"), "{text}");
    assert!(text.contains(failing), "{text}");
    assert!(text.contains("permission denied"), "{text}");
    assert!(
        locked(&calls).iter().all(|call| argv(call)[0] != "-k"),
        "{text}: no reset-reason query follows an unreadable boundary"
    );
}

#[test_case("abc", OLD; "not a number in the crashed boot")]
#[test_case("abc", LATER; "not a number in the successor")]
#[test_case("-1", OLD; "negative in the crashed boot")]
#[test_case("-1", LATER; "negative in the successor")]
#[test_case("184467440737095516150", OLD; "above u64 in the crashed boot")]
#[test_case("184467440737095516150", LATER; "above u64 in the successor")]
fn an_unreadable_sequence_number_is_a_decode_error_not_a_missing_boot(
    text: &str,
    unreadable: &str,
) {
    let good = |id: &str, sequence| boundary(id, "series", sequence);
    let (last, first) = if unreadable == OLD {
        (boundary(OLD, "series", text), good(LATER, "101"))
    } else {
        (good(OLD, "100"), boundary(LATER, "series", text))
    };
    let log = kernel_lines(&["Linux version 7.2.8", THERMAL]);
    let (mut kernel, calls) = successor_kernel(boots(&[OLD, LATER]), last, first, log);
    let error = kernel.reset_reason_after(&boot(OLD)).unwrap_err();
    assert!(!matches!(error, togi_machine::Error::BootMissing));
    assert!(
        matches!(
            caused_by::<DecodeError>(&error),
            Some(DecodeError::Number { field: "__SEQNUM", text: found, .. }) if found == text
        ),
        "{error:?}"
    );
    assert!(
        locked(&calls).iter().all(|call| argv(call)[0] != "-k"),
        "no reset-reason query runs"
    );
}

const UNKNOWN_REASON: &str = "unexpected reason";
const BUTTON_REASON: &str = "power button was pressed for 4 seconds";
const SHUTDOWN_REASON: &str = "internal CPU shutdown event occurred";
const FLOOD_REASON: &str = "a software sync flood event occurred";

#[test_case(UNKNOWN_REASON, BUTTON_REASON, ResetKind::PowerButton; "unknown and power button")]
#[test_case(UNKNOWN_REASON, SHUTDOWN_REASON, ResetKind::CpuShutdown; "unknown and cpu shutdown")]
#[test_case(UNKNOWN_REASON, FLOOD_REASON, ResetKind::SyncFlood; "unknown and sync flood")]
#[test_case(UNKNOWN_REASON, "hardware watchdog timer expired", ResetKind::Watchdog; "unknown and watchdog")]
#[test_case(UNKNOWN_REASON, "internal CPU thermal limit was tripped", ResetKind::ThermalTrip; "unknown and thermal trip")]
#[test_case(SHUTDOWN_REASON, BUTTON_REASON, ResetKind::CpuShutdown; "cpu shutdown and power button")]
#[test_case(SHUTDOWN_REASON, FLOOD_REASON, ResetKind::SyncFlood; "cpu shutdown and sync flood")]
#[test_case(UNKNOWN_REASON, UNKNOWN_REASON, ResetKind::Unknown; "unknown twice")]
fn mixed_reset_reasons_keep_the_dominant_kind_and_every_raw_reason(
    a: &str,
    b: &str,
    dominant: ResetKind,
) {
    for order in [[a, b], [b, a]] {
        let mut scan = ResetScan::default();
        for text in order {
            scan.push(&reset_line(text)).unwrap();
        }
        let got = scan.finish();
        assert_eq!(got.kind, Some(dominant), "{order:?}");
        assert_eq!(got.raw, order, "{order:?}");
    }
}
