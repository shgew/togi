use test_case::test_case;

use super::{Flow, SessionError};
use crate::testing::core;
use crate::write::{ProfileFailure, Refused};

#[test_case(ProfileFailure::Refused(Refused::Unknown { core: core(0) }); "unknown offsets")]
#[test_case(ProfileFailure::Refused(Refused::NoSuchCore { core: core(9) }); "no such core")]
#[test_case(ProfileFailure::Shape {
        profile: 2,
        cores: 4,
    }; "misshapen profile")]
fn a_refused_or_misshapen_profile_is_a_fatal_session_error_never_dead_end_evidence(
    failure: ProfileFailure,
) {
    let text = failure.to_string();
    let Flow::Fatal(SessionError::Profile(kept)) = Flow::from(failure) else {
        panic!("{text}: not a fatal profile error");
    };
    assert_eq!(kept.to_string(), text);
}

#[test]
fn distinct_causes_with_identical_text_are_both_rendered() {
    #[derive(Debug, thiserror::Error)]
    #[error("read")]
    struct Read(#[source] std::io::Error);

    let error = Read(std::io::Error::other("read"));
    assert_eq!(togi_machine::error_chain(&error), "read: read");
}
