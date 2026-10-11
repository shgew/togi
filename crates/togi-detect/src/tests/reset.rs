use serde_json::json;
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

type Scenario<'a> = (&'a str, &'a str, &'a [&'a str], Option<ResetKind>, bool);

#[test]
fn reset_reason_kinds_priority_and_support() {
    use ResetKind::{CpuShutdown, PowerButton, SyncFlood, ThermalTrip, Unknown, Watchdog};
    let cases: [Scenario; 12] = [
        ("old kernel", "6.15", &[], None, false),
        ("boundary", "6.16", &[], None, true),
        ("new major", "7.0", &[], None, true),
        (
            "watchdog",
            "6.16",
            &["hardware watchdog timer expired"],
            Some(Watchdog),
            true,
        ),
        (
            "fabric flood",
            "6.16",
            &["an uncorrected error caused a data fabric sync flood event"],
            Some(SyncFlood),
            true,
        ),
        (
            "software flood",
            "6.16",
            &["a software sync flood event occurred"],
            Some(SyncFlood),
            true,
        ),
        (
            "cpu shutdown",
            "6.16",
            &["internal CPU shutdown event occurred"],
            Some(CpuShutdown),
            true,
        ),
        (
            "power button",
            "6.16",
            &["power button was pressed for 4 seconds"],
            Some(PowerButton),
            true,
        ),
        (
            "thermal pin",
            "6.16",
            &["thermal pin BP_THERMTRIP_L was tripped"],
            Some(ThermalTrip),
            true,
        ),
        (
            "thermal limit",
            "6.16",
            &["internal CPU thermal limit was tripped"],
            Some(ThermalTrip),
            true,
        ),
        (
            "unknown",
            "6.16",
            &["unexpected reason"],
            Some(Unknown),
            true,
        ),
        (
            "precedence",
            "6.16",
            &[
                "power button was pressed for 4 seconds",
                "hardware watchdog timer expired",
                "internal CPU thermal limit was tripped",
                "an uncorrected error caused a data fabric sync flood event",
            ],
            Some(ThermalTrip),
            true,
        ),
    ];
    for (name, version, texts, kind, supported) in cases {
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
        assert_eq!(got, reason(kind, texts, supported), "{name}");
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
            ],
            "{name}"
        );
    }
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

#[test]
fn malformed_output_never_invents_a_reason() {
    let cases: [(&str, Vec<u8>, i32); 4] = [
        ("exit", Vec::new(), 2),
        ("invalid json", b"{".to_vec(), 0),
        ("invalid message", br#"{"MESSAGE":{}}"#.to_vec(), 0),
        (
            "kernel version beyond u64",
            kernel_lines(&["Linux version 99999999999999999999.1"]),
            0,
        ),
    ];
    for (name, out, code) in cases {
        let (mut kernel, _) = kernel(&[], move |_| exit(code, &out, ""));
        assert!(kernel.reset_reason(&boot(OLD)).is_err(), "{name}");
    }
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

#[test]
fn a_successor_that_cannot_be_found_or_read_is_a_missing_boot() {
    let log = kernel_lines(&["Linux version 7.2.8", THERMAL]);
    for (name, list) in [
        ("crashed boot missing", boots(&[LATER, CURRENT])),
        ("no successor", boots(&[OLD])),
        ("no retained boots", boots(&[])),
        ("successor log vacuumed", boots(&[OLD, MISSING, CURRENT])),
        ("successor id empty", boots(&[OLD, ""])),
    ] {
        let (mut kernel, _) = successor_kernel(
            list,
            boundary(OLD, "series", "100"),
            boundary(MISSING, "series", "101"),
            log.clone(),
        );
        assert!(
            matches!(
                kernel.reset_reason_after(&boot(OLD)),
                Err(togi_machine::Error::BootMissing)
            ),
            "{name}"
        );
    }
}

#[test]
fn succession_needs_consecutive_sequence_numbers_of_one_journal() {
    let log = kernel_lines(&["Linux version 7.2.8", THERMAL]);
    let cases: [(&str, Vec<u8>, Vec<u8>, bool); 9] = [
        (
            "continuous boundary",
            boundary(OLD, "series", "100"),
            boundary(LATER, "series", "101"),
            true,
        ),
        (
            "omitted intermediate boot",
            boundary(OLD, "series", "100"),
            boundary(LATER, "series", "200"),
            false,
        ),
        (
            "changed sequence identity",
            boundary(OLD, "series", "100"),
            boundary(LATER, "other", "101"),
            false,
        ),
        (
            "missing predecessor metadata",
            format!(r#"{{"_BOOT_ID":"{OLD}"}}"#).into_bytes(),
            boundary(LATER, "series", "101"),
            false,
        ),
        (
            "missing successor metadata",
            boundary(OLD, "series", "100"),
            format!(r#"{{"_BOOT_ID":"{LATER}"}}"#).into_bytes(),
            false,
        ),
        (
            "missing predecessor tail",
            Vec::new(),
            boundary(LATER, "series", "101"),
            false,
        ),
        (
            "missing successor start",
            boundary(OLD, "series", "100"),
            Vec::new(),
            false,
        ),
        (
            "wrong boundary boot",
            boundary(CURRENT, "series", "100"),
            boundary(LATER, "series", "101"),
            false,
        ),
        (
            "sequence wraparound",
            boundary(OLD, "series", "18446744073709551615"),
            boundary(LATER, "series", "0"),
            false,
        ),
    ];
    for (name, last, first, continuous) in cases {
        let (mut kernel, _) = successor_kernel(boots(&[OLD, LATER]), last, first, log.clone());
        let got = kernel.reset_reason_after(&boot(OLD));
        if continuous {
            assert_eq!(
                got.unwrap(),
                reason(
                    Some(ResetKind::ThermalTrip),
                    &["internal CPU thermal limit was tripped"],
                    true
                ),
                "{name}"
            );
        } else {
            assert!(
                matches!(got, Err(togi_machine::Error::BootMissing)),
                "{name}"
            );
        }
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

#[test]
fn boundary_failures_are_told_apart() {
    type Reply = (&'static [u8], &'static str, i32);
    let cases: [(&str, Reply, bool); 4] = [
        ("vacuumed boundary", (b"", NO_BOOT, 1), true),
        ("empty boundary", (b"", "", 1), true),
        ("permission error", (b"", "permission denied", 1), false),
        ("malformed boundary", (b"{", "", 0), false),
    ];
    for (name, (out, stderr, code), missing) in cases {
        let list = boots(&[OLD, LATER]);
        let (kernel, _) = kernel(&[], move |args| {
            if args[0] == "--list-boots" {
                ok(&list)
            } else {
                exit(code, out, stderr)
            }
        });
        let got = kernel.read_reset_reason_after(&JournalBoot::parse(OLD).unwrap());
        assert_eq!(matches!(got, Err(Error::BootMissing)), missing, "{name}");
        assert!(got.is_err(), "{name}");
    }
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

type Exit<'a> = (i32, &'a [u8], &'a str);

#[test]
fn a_boot_list_that_cannot_be_read_is_never_clean_or_missing() {
    let cases: [(&str, Result<Exit, ()>); 3] = [
        ("command error", Err(())),
        ("nonzero exit", Ok((1, b"", "permission denied"))),
        ("malformed list", Ok((0, b"[", ""))),
    ];
    for (name, reply) in cases {
        let (kernel, _) = kernel(&[], move |_| match reply {
            Ok((code, out, stderr)) => exit(code, out, stderr),
            Err(()) => Err(crate::runner::RunError::Command(
                togi_command::Error::Spawn {
                    program: "journalctl".into(),
                    source: std::io::Error::other("journal unavailable"),
                },
            )),
        });
        let error = kernel
            .read_reset_reason_after(&JournalBoot::parse(OLD).unwrap())
            .unwrap_err();
        assert!(!matches!(error, Error::BootMissing), "{name}");
        if name == "command error" {
            assert!(caused_by::<togi_command::Error>(&error).is_some());
        }
        if name == "malformed list" {
            assert!(caused_by::<DecodeError>(&error).is_some());
        }
    }
}

fn reset_line(text: &str) -> String {
    format!("x86/amd: Previous system reset reason [0x00000001]: {text}")
}

#[test]
fn a_failed_reset_reason_read_names_the_operation_and_its_boot() {
    let cases: [(&str, Exit, Option<&str>); 2] = [
        (
            "exit status",
            (1, b"", "permission denied"),
            Some("permission denied"),
        ),
        ("malformed entry", (0, b"{", ""), None),
    ];
    for (name, (code, out, stderr), cause) in cases {
        let (mut kernel, _) = kernel(&[], move |_| exit(code, out, stderr));
        let error = kernel.reset_reason(&boot(OLD)).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("reset reason"), "{name}: {text}");
        assert!(!text.contains("journal boundary"), "{name}: {text}");
        assert!(text.contains(OLD), "{name}: {text}");
        if let Some(cause) = cause {
            assert!(text.contains(cause), "{name}: {text}");
        } else {
            assert!(caused_by::<DecodeError>(&error).is_some(), "{name}");
        }
    }
}

#[test]
fn a_failed_boundary_read_names_the_operation_and_the_boot_it_read() {
    for (name, failing) in [("crashed boot", OLD), ("successor", LATER)] {
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
        assert!(text.contains("journal boundary"), "{name}: {text}");
        assert!(!text.contains("reset reason"), "{name}: {text}");
        assert!(text.contains(failing), "{name}: {text}");
        assert!(text.contains("permission denied"), "{name}: {text}");
        assert!(
            locked(&calls).iter().all(|call| argv(call)[0] != "-k"),
            "{name}: no reset-reason query follows an unreadable boundary"
        );
    }
}

#[test]
fn an_unreadable_sequence_number_is_a_decode_error_not_a_missing_boot() {
    let too_big = u64::MAX.to_string() + "0";
    for (name, text) in [
        ("not a number", "abc"),
        ("negative", "-1"),
        ("above u64", too_big.as_str()),
    ] {
        for unreadable in [OLD, LATER] {
            let good = |id: &str, sequence| boundary(id, "series", sequence);
            let (last, first) = if unreadable == OLD {
                (boundary(OLD, "series", text), good(LATER, "101"))
            } else {
                (good(OLD, "100"), boundary(LATER, "series", text))
            };
            let log = kernel_lines(&["Linux version 7.2.8", THERMAL]);
            let (mut kernel, calls) = successor_kernel(boots(&[OLD, LATER]), last, first, log);
            let error = kernel.reset_reason_after(&boot(OLD)).unwrap_err();
            assert!(
                !matches!(error, togi_machine::Error::BootMissing),
                "{name} in {unreadable}"
            );
            assert!(
                matches!(
                    caused_by::<DecodeError>(&error),
                    Some(DecodeError::Number { field: "__SEQNUM", text: found, .. }) if found == text
                ),
                "{name} in {unreadable}: {error:?}"
            );
            assert!(
                locked(&calls).iter().all(|call| argv(call)[0] != "-k"),
                "{name} in {unreadable}: no reset-reason query runs"
            );
        }
    }
}

#[test]
fn mixed_reset_reasons_keep_the_dominant_kind_and_every_raw_reason() {
    use ResetKind::{CpuShutdown, PowerButton, SyncFlood, ThermalTrip, Unknown, Watchdog};
    const UNKNOWN: &str = "unexpected reason";
    const BUTTON: &str = "power button was pressed for 4 seconds";
    const SHUTDOWN: &str = "internal CPU shutdown event occurred";
    const FLOOD: &str = "a software sync flood event occurred";
    let pairs: [(&str, &str, ResetKind); 8] = [
        (UNKNOWN, BUTTON, PowerButton),
        (UNKNOWN, SHUTDOWN, CpuShutdown),
        (UNKNOWN, FLOOD, SyncFlood),
        (UNKNOWN, "hardware watchdog timer expired", Watchdog),
        (
            UNKNOWN,
            "internal CPU thermal limit was tripped",
            ThermalTrip,
        ),
        (SHUTDOWN, BUTTON, CpuShutdown),
        (SHUTDOWN, FLOOD, SyncFlood),
        (UNKNOWN, UNKNOWN, Unknown),
    ];
    for (a, b, dominant) in pairs {
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
}
