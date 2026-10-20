//! Journalctl rows are JSON objects. A JSON array of the same values in declaration order is
//! malformed output, never a row.

use serde_json::{Value, json};
use togi_machine::Kernel as _;

use super::support::{LATER, OLD, boot, caused_by, kernel, lines, ok};
use crate::DecodeError;

const STATUS: &str = "[Hardware Error]: CPU:2 (1a:44:0) MC1_STATUS[Over|CE|-]: 0xbc00000000010135";

fn object(cursor: &str, message: &str) -> Value {
    json!({
        "__CURSOR": cursor,
        "_BOOT_ID": OLD,
        "MESSAGE": message,
        "__REALTIME_TIMESTAMP": "2000000",
        "__MONOTONIC_TIMESTAMP": "1000000",
    })
}

/// The fields of an entry in the order `Entry` declares them, sized exactly like the object.
fn positional_entry(cursor: &str, message: &str) -> Value {
    json!([message, "2000000", "1000000", cursor, OLD])
}

#[test]
fn an_array_entry_in_a_cursor_read_is_malformed_output_after_the_earlier_checks() {
    let mut out = lines(&[object("mce", STATUS)]);
    out.extend(lines(&[positional_entry("tail", "ordinary")]));
    let (mut kernel, _) = kernel(&[(1, &[2])], move |_| ok(&out));
    let partial = kernel.read_mces(&boot(OLD), None).unwrap_err();
    assert!(
        caused_by::<DecodeError>(&partial).is_some(),
        "{}",
        partial.error
    );
    assert_eq!(partial.value.mces.len(), 1);
    assert_eq!(partial.value.mces[0].core, Some(super::support::core(1)));
}

#[test]
fn an_array_entry_alone_never_becomes_an_empty_interval() {
    let out = lines(&[positional_entry("tail", "ordinary")]);
    let (mut kernel, _) = kernel(&[], move |_| ok(&out));
    let partial = kernel.read_mces(&boot(OLD), None).unwrap_err();
    assert!(caused_by::<DecodeError>(&partial).is_some());
    assert!(partial.value.mces.is_empty());
}

#[test]
fn an_array_entry_in_a_whole_boot_read_is_a_decode_failure() {
    let out = lines(&[positional_entry("tail", STATUS)]);
    let (mut kernel, _) = kernel(&[(1, &[2])], move |_| ok(&out));
    let error = kernel
        .mces(&boot(OLD), std::time::Duration::ZERO)
        .unwrap_err();
    assert!(caused_by::<DecodeError>(&error).is_some(), "{error}");
}

#[test]
fn an_array_reset_row_is_a_decode_failure() {
    let out = lines(&[json!(["Linux version 7.2.8"])]);
    let (mut kernel, _) = kernel(&[], move |_| ok(&out));
    let error = kernel.reset_reason(&boot(OLD)).unwrap_err();
    assert!(caused_by::<DecodeError>(&error).is_some(), "{error}");
}

#[test]
fn an_array_boot_entry_is_a_decode_failure() {
    let list = json!([[OLD, 1], [LATER, 2]]).to_string().into_bytes();
    let valid = json!({"_BOOT_ID": OLD, "__SEQNUM_ID": "series", "__SEQNUM": "100"})
        .to_string()
        .into_bytes();
    let (mut kernel, calls) = kernel(&[], move |args| {
        if args[0] == "--list-boots" {
            ok(&list)
        } else {
            ok(&valid)
        }
    });
    let error = kernel.reset_reason_after(&boot(OLD)).unwrap_err();
    assert!(caused_by::<DecodeError>(&error).is_some(), "{error}");
    assert_eq!(super::support::locked(&calls).len(), 1);
}

#[test]
fn an_array_position_row_is_a_decode_failure() {
    let list = json!([{"boot_id": OLD}, {"boot_id": LATER}])
        .to_string()
        .into_bytes();
    let position = json!([OLD, "series", "100"]).to_string().into_bytes();
    let (mut kernel, _) = kernel(&[], move |args| {
        if args[0] == "--list-boots" {
            ok(&list)
        } else {
            ok(&position)
        }
    });
    let error = kernel.reset_reason_after(&boot(OLD)).unwrap_err();
    assert!(caused_by::<DecodeError>(&error).is_some(), "{error}");
}
