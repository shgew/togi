use std::error::Error as _;
use std::io;

use super::{Error, Partial, error_chain};

#[derive(Debug, thiserror::Error)]
#[error("read request")]
struct Read(#[source] io::Error);

#[derive(Debug, thiserror::Error)]
#[error("start trial")]
struct Start(#[source] Read);

#[derive(Debug, thiserror::Error)]
#[error(transparent)]
struct Forward(#[from] Start);

#[test]
fn sources_are_rendered_in_order_even_when_one_is_a_substring() {
    let error = Start(Read(io::Error::other("read")));
    assert_eq!(error_chain(&error), "start trial: read request: read");
    assert!(error.source().unwrap().downcast_ref::<Read>().is_some());
    assert!(
        error
            .source()
            .unwrap()
            .source()
            .unwrap()
            .downcast_ref::<io::Error>()
            .is_some()
    );
}

#[test]
fn transparent_forwarding_adds_no_duplicate_operation() {
    let error = Forward(Start(Read(io::Error::other("read"))));
    assert_eq!(error_chain(&error), "start trial: read request: read");
}

#[test]
fn partial_progress_keeps_the_machine_error_downcastable() {
    let error = Partial {
        value: 7,
        error: Error::Containment(Box::new(Read(io::Error::other("read")))),
    };
    assert_eq!(
        error_chain(&error),
        "machine operation left partial progress: trial cleanup could not be confirmed: read request: read"
    );
    assert!(error.source().unwrap().downcast_ref::<Error>().is_some());
}

#[test_case::test_case(0, 0, Some(std::time::Duration::ZERO); "zero")]
#[test_case::test_case(i64::MAX, 999_999_999, Some(std::time::Duration::new(i64::MAX.cast_unsigned(), 999_999_999)); "largest kernel components")]
#[test_case::test_case(-1, 0, None; "negative seconds")]
#[test_case::test_case(0, -1, None; "negative nanoseconds")]
#[test_case::test_case(0, 1_000_000_000, None; "nanoseconds must not normalize")]
#[test_case::test_case(0, i64::MAX, None; "oversized nanoseconds")]
fn kernel_monotonic_components_are_checked(
    seconds: i64,
    nanoseconds: i64,
    expected: Option<std::time::Duration>,
) {
    let actual = crate::monotonic_duration(seconds, nanoseconds);
    assert_eq!(actual.as_ref().ok().copied(), expected);
    if expected.is_none() {
        assert!(matches!(
            actual,
            Err(Error::InvalidMonotonic { seconds: s, nanoseconds: n })
                if s == seconds && n == nanoseconds
        ));
    }
}
