use togi_tool_audit::Check::{Cause, Sequence};
use togi_tool_audit::Completion::InProgress;

use crate::fixtures::*;

#[test_case::test_case(case(
            "an earlier cause",
            vec![cause(phase(2, 0), &[1])],
            InProgress,
            &[],
        ); "an earlier cause")]
#[test_case::test_case(case(
            "a self cause and a later cause",
            vec![
                cause(phase(2, 0), &[2, 3]),
                phase(3, 0),
            ],
            InProgress,
            &[(2, Cause), (2, Cause)],
        ); "a self cause and a later cause")]
#[test_case::test_case(case(
            "a cause no event has",
            vec![cause(phase(2, 0), &[9])],
            InProgress,
            &[(2, Cause)],
        ); "a cause no event has")]
#[test_case::test_case(case(
            "a missing earlier cause",
            vec![
                phase(3, 0),
                cause(phase(4, 0), &[2]),
            ],
            InProgress,
            &[(3, Sequence), (4, Cause)],
        ); "a missing earlier cause")]
fn a_cause_names_an_existing_earlier_event(case: Case) {
    run(case);
}

#[test_case::test_case(case(
            "a gap",
            vec![phase(3, 0)],
            InProgress,
            &[(3, Sequence)],
        ); "a gap")]
#[test_case::test_case(case(
            "a repeat",
            vec![phase(2, 0), phase(2, 0)],
            InProgress,
            &[(2, Sequence)],
        ); "a repeat")]
fn sequences_count_up_from_one_and_start_with_the_session(case: Case) {
    run(case);
}

#[test]
fn the_first_event_must_be_session_start() {
    assert_eq!(
        found(&[phase(1, 0)], InProgress),
        vec![(1, Sequence)],
        "the first event is not session.start"
    );
}

#[test]
fn an_empty_journal_has_no_sequence_violation() {
    assert_eq!(found(&[], InProgress), vec![], "an empty journal");
}
