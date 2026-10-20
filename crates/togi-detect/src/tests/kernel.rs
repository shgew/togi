use std::time::Duration;

use serde_json::json;
use test_case::test_case;
use togi_machine::{Kernel as _, KernelCursor, Partial};

use super::support::{
    CURRENT, MISSING, OLD, argv, at, boot, caused_by, core, exit, kernel, lines, locked, ok,
};
use crate::{BootIdError, DecodeError};

const STATUS: &str = "[Hardware Error]: CPU:2 (1a:44:0) MC0_STATUS[Over|CE]: 0xdc204000000d0175";
const NO_BOOT: &str = "No journal boot entry found\n";

fn byte_array(text: &str) -> serde_json::Value {
    json!(text.bytes().collect::<Vec<u8>>())
}

fn status_entry(wall: i64, monotonic: i64) -> Vec<u8> {
    lines(&[json!({
        "MESSAGE": STATUS,
        "__REALTIME_TIMESTAMP": wall.to_string(),
        "__MONOTONIC_TIMESTAMP": monotonic.to_string(),
    })])
}

#[test]
fn whole_boot_reads_the_grep_query_of_the_boot_without_hyphens() {
    let (mut kernel, calls) = kernel(&[(1, &[2, 18])], |_| ok(status_entry(2_000_000, 2_000_000)));
    let mces = kernel
        .mces(
            &boot("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
            Duration::ZERO,
        )
        .unwrap();
    assert_eq!(mces.len(), 1);
    let calls = locked(&calls);
    assert_eq!(
        argv(&calls[0]),
        [
            "-k",
            "-b",
            OLD,
            "-o",
            "json",
            "--output-fields=MESSAGE,__REALTIME_TIMESTAMP,__MONOTONIC_TIMESTAMP",
            "--grep",
            "Hardware Error",
            "--no-pager",
            "-q",
        ]
    );
}

#[test]
fn a_message_may_be_a_byte_array() {
    let out = lines(&[json!({
        "MESSAGE": byte_array(STATUS),
        "__REALTIME_TIMESTAMP": "2000000",
        "__MONOTONIC_TIMESTAMP": "2000000",
    })]);
    let (mut kernel, _) = kernel(&[(1, &[2, 18])], move |_| ok(&out));
    let mces = kernel.mces(&boot(OLD), Duration::from_secs(1)).unwrap();
    assert_eq!(mces.len(), 1);
    assert_eq!(mces[0].core, Some(core(1)));
    assert_eq!(mces[0].time, at(2_000_000));
    assert_eq!(mces[0].monotonic, Duration::from_secs(2));
}

#[test]
fn the_monotonic_lower_bound_decides_which_checks_are_eligible() {
    let mut out = status_entry(3_000_000, 1_000_000);
    out.extend(status_entry(3_000_000, 2_000_000));
    let (mut kernel, _) = kernel(&[(3, &[2])], move |_| ok(&out));
    let got = kernel
        .mces(&boot(OLD), Duration::from_millis(1500))
        .unwrap();
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].monotonic, Duration::from_secs(2));
    assert_eq!(got[0].time, at(3_000_000));
}

#[test]
fn missing_boot_no_match_and_failure_are_told_apart() {
    let (mut missing, _) = kernel(&[], |_| exit(1, b"", NO_BOOT));
    assert!(matches!(
        missing.mces(&boot(MISSING), Duration::ZERO),
        Err(togi_machine::Partial {
            error: togi_machine::Error::BootMissing,
            ..
        })
    ));
    let (mut empty, _) = kernel(&[], |_| exit(1, b"", ""));
    assert!(empty.mces(&boot(OLD), Duration::ZERO).unwrap().is_empty());
    let (mut broken, _) = kernel(&[], |_| exit(2, b"", "permission denied\n"));
    let error = broken.mces(&boot(OLD), Duration::ZERO).unwrap_err();
    assert!(!matches!(error.error, togi_machine::Error::BootMissing));
    assert!(togi_machine::error_chain(&error).contains(OLD), "{error}");
    let (mut half_empty, _) = kernel(&[], |_| exit(1, b"{}\n", ""));
    assert!(half_empty.mces(&boot(OLD), Duration::ZERO).is_err());
    let (mut signalled, _) = kernel(&[], |_| {
        Ok(togi_command::Output {
            status: std::os::unix::process::ExitStatusExt::from_raw(9),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    });
    assert!(signalled.mces(&boot(OLD), Duration::ZERO).is_err());
}

#[test]
fn an_exit_error_names_the_operation_status_and_stderr() {
    let (kernel, _) = kernel(&[], |_| exit(2, b"", "  permission denied \n"));
    let error = kernel
        .read_whole_boot(
            &crate::boot::JournalBoot::parse(OLD).unwrap(),
            Duration::ZERO,
        )
        .unwrap_err();
    let text = togi_machine::error_chain(&error);
    assert!(text.contains("read the kernel log of boot"), "{text}");
    assert!(text.contains("exit status: 2"), "{text}");
    assert!(text.ends_with("permission denied"), "{text}");
}

#[test_case("", None; "empty")]
#[test_case("", Some("s=1;i=2"); "empty after a cursor")]
#[test_case("boot", None; "word")]
#[test_case("boot", Some("s=1;i=2"); "word after a cursor")]
#[test_case("1234-abcd", None; "short")]
#[test_case("1234-abcd", Some("s=1;i=2"); "short after a cursor")]
#[test_case("zzzzzzzzbbbbccccddddeeeeeeeeeeee", None; "non-hex")]
#[test_case("zzzzzzzzbbbbccccddddeeeeeeeeeeee", Some("s=1;i=2"); "non-hex after a cursor")]
#[test_case("aaaaaaaabbbbccccddddeeeeeeeeeeee0", None; "33 digits")]
#[test_case("aaaaaaaabbbbccccddddeeeeeeeeeeee0", Some("s=1;i=2"); "33 digits after a cursor")]
fn a_boot_that_is_not_32_hex_digits_is_refused_before_any_command(id: &str, after: Option<&str>) {
    let (mut kernel, calls) = kernel(&[], |_| ok(""));
    let error = kernel.mces(&boot(id), Duration::ZERO).unwrap_err();
    assert!(
        togi_machine::error_chain(&error).contains("is not a boot id"),
        "{id}: {error}"
    );
    let after = after.map(|text| KernelCursor::from(text.to_owned()));
    let partial = kernel.read_mces(&boot(id), after.as_ref()).unwrap_err();
    assert!(partial.value.mces.is_empty(), "{id} after {after:?}");
    assert!(
        !matches!(
            partial.error,
            togi_machine::Error::CursorMissing | togi_machine::Error::BootMissing
        ),
        "{id} after {after:?}: {:?}",
        partial.error
    );
    let text = partial.error.to_string();
    assert!(
        text.contains(&format!("{id:?} is not a boot id")),
        "{id} after {after:?}: {text}"
    );
    assert!(locked(&calls).is_empty());
}

#[test_case(json!({"MESSAGE": {}, "__REALTIME_TIMESTAMP": "1", "__MONOTONIC_TIMESTAMP": "1"}); "object message")]
#[test_case(json!({"__REALTIME_TIMESTAMP": "1", "__MONOTONIC_TIMESTAMP": "1"}); "no message")]
#[test_case(json!({"MESSAGE": [256], "__REALTIME_TIMESTAMP": "1", "__MONOTONIC_TIMESTAMP": "1"}); "byte over 255")]
#[test_case(json!({"MESSAGE": "x", "__REALTIME_TIMESTAMP": "invalid", "__MONOTONIC_TIMESTAMP": "1"}); "wall clock text")]
#[test_case(json!({"MESSAGE": "x", "__REALTIME_TIMESTAMP": 1, "__MONOTONIC_TIMESTAMP": "1"}); "wall clock number")]
#[test_case(json!({"MESSAGE": "x", "__MONOTONIC_TIMESTAMP": "1"}); "wall clock missing")]
#[test_case(json!({"MESSAGE": "x", "__REALTIME_TIMESTAMP": "9223372036854775807", "__MONOTONIC_TIMESTAMP": "1"}); "wall clock beyond jiff")]
#[test_case(json!({"MESSAGE": "x", "__REALTIME_TIMESTAMP": "9223372036854775808", "__MONOTONIC_TIMESTAMP": "1"}); "wall clock overflow")]
#[test_case(json!({"MESSAGE": "x", "__REALTIME_TIMESTAMP": "1", "__MONOTONIC_TIMESTAMP": "invalid"}); "monotonic text")]
#[test_case(json!({"MESSAGE": "x", "__REALTIME_TIMESTAMP": "1", "__MONOTONIC_TIMESTAMP": "-1"}); "monotonic negative")]
#[test_case(json!({"MESSAGE": "x", "__REALTIME_TIMESTAMP": "1"}); "monotonic missing")]
fn malformed_entries_are_errors_not_clean_intervals(entry: serde_json::Value) {
    let out = lines(&[entry]);
    let (mut kernel, _) = kernel(&[], move |_| ok(&out));
    let error = kernel.mces(&boot(OLD), Duration::ZERO).unwrap_err();
    assert!(caused_by::<DecodeError>(&error).is_some(), "{error:?}");
}

#[test_case(b"{broken"; "broken json")]
#[test_case(b"   \n"; "blank line")]
#[test_case(b"[]"; "empty array")]
fn malformed_output_is_an_error_not_a_clean_interval(out: &'static [u8]) {
    let (mut kernel, _) = kernel(&[], move |_| ok(out));
    assert!(kernel.mces(&boot(OLD), Duration::ZERO).is_err());
}

#[test]
fn empty_lines_are_ignored() {
    let mut out = b"\n\n".to_vec();
    out.extend(status_entry(2_000_000, 2_000_000));
    out.extend(b"\n");
    let (mut kernel, _) = kernel(&[(1, &[2])], move |_| ok(&out));
    assert_eq!(kernel.mces(&boot(OLD), Duration::ZERO).unwrap().len(), 1);
}

#[test]
fn a_status_with_an_impossible_cpu_is_an_error() {
    let out = lines(&[json!({
        "MESSAGE": "[Hardware Error]: CPU:70000 (1a:44:0) MC0_STATUS[Over|CE]: 0xdc204000000d0175",
        "__REALTIME_TIMESTAMP": "1",
        "__MONOTONIC_TIMESTAMP": "1",
    })]);
    let (mut kernel, _) = kernel(&[], move |_| ok(&out));
    let error = kernel.mces(&boot(OLD), Duration::ZERO).unwrap_err();
    assert!(
        togi_machine::error_chain(&error).contains("cannot read the kernel log"),
        "{error}"
    );
}

#[test]
fn a_boot_id_is_the_32_hex_digits_of_a_hyphenated_uuid() {
    let uuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\n";
    assert_eq!(
        crate::boot::checked(uuid).unwrap().as_str(),
        "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
    );
    assert_eq!(crate::boot::checked(OLD).unwrap().as_str(), OLD);
}

#[test_case(""; "empty")]
#[test_case("\n"; "newline")]
#[test_case("not a boot id"; "words")]
#[test_case("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeee"; "31 digits")]
fn a_malformed_boot_id_is_invalid(text: &str) {
    assert!(
        matches!(crate::boot::checked(text), Err(BootIdError::Invalid(_))),
        "{text:?}"
    );
}

#[test]
fn a_proc_boot_id_is_read_and_validated() {
    let id = crate::boot::read_boot_id(|| Ok("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee\n".to_owned()))
        .unwrap();
    assert_eq!(id.as_str(), "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee");
    assert!(crate::boot::JournalBoot::parse(id.as_str()).is_ok());

    let error = crate::boot::read_boot_id(|| {
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied))
    })
    .unwrap_err();
    let crate::Error::BootId(BootIdError::Read(source)) = error else {
        panic!("unexpected boot-id error: {error}");
    };
    assert_eq!(source.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(matches!(
        crate::boot::read_boot_id(|| Ok("not a boot id".to_owned())),
        Err(crate::Error::BootId(BootIdError::Invalid(_)))
    ));
}

#[test]
fn the_partial_of_a_failed_read_names_the_error() {
    let (mut kernel, _) = kernel(&[], |_| exit(1, b"", NO_BOOT));
    let Partial { value, error } = kernel.read_mces(&boot(CURRENT), None).unwrap_err();
    assert_eq!(value.mces, []);
    assert_eq!(value.faults, []);
    assert!(matches!(error, togi_machine::Error::BootMissing));
}
