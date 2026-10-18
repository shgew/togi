use togi_tool_audit::Check::{Cause, Sequence};
use togi_tool_audit::Completion::InProgress;

use crate::fixtures::*;

#[test_case::test_case(case(
            "an earlier cause",
            vec![cause(decision(2, 0, "backoff", None), &[1])],
            InProgress,
            &[],
        ); "an earlier cause")]
#[test_case::test_case(case(
            "a self cause and a later cause",
            vec![
                cause(decision(2, 0, "backoff", None), &[2, 3]),
                phase(3, 0, None),
            ],
            InProgress,
            &[(2, Cause), (2, Cause)],
        ); "a self cause and a later cause")]
#[test_case::test_case(case(
            "a cause no event has",
            vec![cause(decision(2, 0, "backoff", None), &[9])],
            InProgress,
            &[(2, Cause)],
        ); "a cause no event has")]
#[test_case::test_case(case(
            "a missing earlier cause",
            vec![
                phase(3, 0, None),
                cause(decision(4, 0, "backoff", None), &[2]),
            ],
            InProgress,
            &[(3, Sequence), (4, Cause)],
        ); "a missing earlier cause")]
fn a_cause_names_an_existing_earlier_event(case: Case) {
    run(case);
}

#[test_case::test_case(case(
            "a gap",
            vec![phase(3, 0, None)],
            InProgress,
            &[(3, Sequence)],
        ); "a gap")]
#[test_case::test_case(case(
            "a repeat",
            vec![phase(2, 0, None), phase(2, 0, None)],
            InProgress,
            &[(2, Sequence)],
        ); "a repeat")]
fn sequences_count_up_from_one_and_start_with_the_session(case: Case) {
    run(case);
}
