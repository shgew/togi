use togi_tool_audit::Check::{Cause, Sequence};
use togi_tool_audit::Completion::InProgress;

use crate::fixtures::*;

#[test]
fn a_cause_names_an_existing_earlier_event() {
    run(vec![
        case(
            "an earlier cause",
            vec![cause(decision(2, 0, "backoff", None), &[1])],
            InProgress,
            &[],
        ),
        case(
            "a self cause and a later cause",
            vec![
                cause(decision(2, 0, "backoff", None), &[2, 3]),
                phase(3, 0, None),
            ],
            InProgress,
            &[(2, Cause), (2, Cause)],
        ),
        case(
            "a cause no event has",
            vec![cause(decision(2, 0, "backoff", None), &[9])],
            InProgress,
            &[(2, Cause)],
        ),
    ]);
}

#[test]
fn sequences_count_up_from_one_and_start_with_the_session() {
    run(vec![
        case(
            "a gap",
            vec![phase(3, 0, None)],
            InProgress,
            &[(3, Sequence)],
        ),
        case(
            "a repeat",
            vec![phase(2, 0, None), phase(2, 0, None)],
            InProgress,
            &[(2, Sequence)],
        ),
    ]);
    assert_eq!(
        found(&[phase(1, 0, None)], InProgress),
        vec![(1, Sequence)],
        "the first event is not session.start"
    );
    assert_eq!(found(&[], InProgress), vec![], "an empty journal");
}
