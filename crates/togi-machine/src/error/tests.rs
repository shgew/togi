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
