use std::error::Error as _;
use std::io;

use test_case::test_case;

use super::{Operation, RestoreFailed, combine};

fn failed(kind: io::ErrorKind) -> io::Result<()> {
    Err(io::Error::from(kind))
}

#[test]
fn a_successful_operation_and_restoration_succeed() {
    assert!(combine(Operation::Write, "LoaderEntryOneShot", Ok(()), Ok(())).is_ok());
}

#[test_case(failed(io::ErrorKind::InvalidInput), Ok(()), io::ErrorKind::InvalidInput; "operation")]
#[test_case(Ok(()), failed(io::ErrorKind::PermissionDenied), io::ErrorKind::PermissionDenied; "restoration")]
fn a_single_failure_is_returned_as_it_is(
    result: io::Result<()>,
    restore: io::Result<()>,
    kind: io::ErrorKind,
) {
    let error = combine(Operation::Write, "LoaderEntryOneShot", result, restore).unwrap_err();
    assert_eq!(error.kind(), kind);
    assert!(error.get_ref().is_none());
}

#[test_case(Operation::Write, "write"; "write")]
#[test_case(Operation::Delete, "delete"; "delete")]
fn a_double_failure_keeps_the_operation_error_as_source_and_the_restoration_error(
    operation: Operation,
    verb: &str,
) {
    let error = combine(
        operation,
        "LoaderEntryOneShot",
        failed(io::ErrorKind::InvalidInput),
        failed(io::ErrorKind::PermissionDenied),
    )
    .unwrap_err();
    let failure = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<RestoreFailed>())
        .unwrap();
    assert_eq!(failure.operation, operation);
    assert_eq!(failure.source.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(failure.restore.kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(
        error.source().map(ToString::to_string),
        Some(io::Error::from(io::ErrorKind::InvalidInput).to_string())
    );
    assert_eq!(
        error.to_string(),
        format!(
            "{verb} EFI variable LoaderEntryOneShot (restoring its immutable flag then failed: {})",
            io::Error::from(io::ErrorKind::PermissionDenied)
        )
    );
}
