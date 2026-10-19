use std::collections::HashSet;

use test_case::test_case;
use togi_journal::DeadEndCondition;

use super::Exit;

#[test_case(Exit::Ok, 0 ; "ok")]
#[test_case(Exit::Error, 1 ; "error")]
#[test_case(Exit::Usage, 2 ; "usage")]
#[test_case(Exit::Locked, 3 ; "locked")]
#[test_case(Exit::FailureAtZero, 10 ; "failure at zero")]
#[test_case(Exit::Smu, 11 ; "smu")]
#[test_case(Exit::NoEvidence, 12 ; "no evidence")]
#[test_case(Exit::BootLoop, 13 ; "boot loop")]
#[test_case(Exit::Containment, 14 ; "containment")]
#[test_case(Exit::Preflight, 15 ; "preflight")]
#[test_case(Exit::Incompatible, 16 ; "incompatible")]
#[test_case(Exit::Defect, 17 ; "defect")]
#[test_case(Exit::ThermalTrip, 18 ; "thermal trip")]
fn every_status_has_the_code_the_spec_lists(exit: Exit, code: u8) {
    assert_eq!(exit.code(), code);
}

#[test]
fn all_lists_every_status_in_the_spec_order() {
    assert_eq!(
        Exit::ALL,
        [
            Exit::Ok,
            Exit::Error,
            Exit::Usage,
            Exit::Locked,
            Exit::FailureAtZero,
            Exit::Smu,
            Exit::NoEvidence,
            Exit::BootLoop,
            Exit::Containment,
            Exit::Preflight,
            Exit::Incompatible,
            Exit::Defect,
            Exit::ThermalTrip,
        ]
    );
}

#[test]
fn codes_are_distinct() {
    let codes: HashSet<u8> = Exit::ALL.iter().map(|exit| exit.code()).collect();
    assert_eq!(codes.len(), Exit::ALL.len());
}

#[test]
fn each_dead_end_has_its_own_code_between_10_and_18() {
    let codes: Vec<u8> = DeadEndCondition::ALL
        .iter()
        .map(|&condition| Exit::from(condition).code())
        .collect();
    let distinct: HashSet<u8> = codes.iter().copied().collect();
    assert_eq!(distinct.len(), DeadEndCondition::ALL.len());
    assert!(
        codes
            .iter()
            .all(|code| (10..=18).contains(code) && *code != 16)
    );
}

#[test_case(DeadEndCondition::FailureAtZero, 10 ; "failure at zero")]
#[test_case(DeadEndCondition::Smu, 11 ; "smu")]
#[test_case(DeadEndCondition::NoEvidence, 12 ; "no evidence")]
#[test_case(DeadEndCondition::BootLoop, 13 ; "boot loop")]
#[test_case(DeadEndCondition::Containment, 14 ; "containment")]
#[test_case(DeadEndCondition::Preflight, 15 ; "preflight")]
#[test_case(DeadEndCondition::Defect, 17 ; "defect")]
#[test_case(DeadEndCondition::ThermalTrip, 18 ; "thermal trip")]
fn dead_ends_map_by_name(condition: DeadEndCondition, want: u8) {
    assert_eq!(Exit::dead_end(condition).code(), want);
}

#[test]
fn a_status_becomes_the_process_exit_code() {
    for exit in Exit::ALL {
        assert_eq!(
            format!("{:?}", std::process::ExitCode::from(exit)),
            format!("{:?}", std::process::ExitCode::from(exit.code())),
            "{exit:?}",
        );
    }
}
