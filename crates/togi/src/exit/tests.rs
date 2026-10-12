use std::collections::HashSet;

use togi_journal::DeadEndCondition;

use super::Exit;

#[test]
fn every_status_has_the_code_the_spec_lists() {
    let want = [
        (Exit::Ok, 0),
        (Exit::Error, 1),
        (Exit::Usage, 2),
        (Exit::Locked, 3),
        (Exit::FailureAtZero, 10),
        (Exit::Smu, 11),
        (Exit::NoEvidence, 12),
        (Exit::BootLoop, 13),
        (Exit::Containment, 14),
        (Exit::Preflight, 15),
        (Exit::Incompatible, 16),
        (Exit::Defect, 17),
        (Exit::ThermalTrip, 18),
    ];
    assert_eq!(want.map(|(exit, _)| exit), Exit::ALL);
    for (exit, code) in want {
        assert_eq!(exit.code(), code, "{exit:?}");
    }
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

#[test]
fn dead_ends_map_by_name() {
    for (condition, want) in [
        (DeadEndCondition::FailureAtZero, 10),
        (DeadEndCondition::Smu, 11),
        (DeadEndCondition::NoEvidence, 12),
        (DeadEndCondition::BootLoop, 13),
        (DeadEndCondition::Containment, 14),
        (DeadEndCondition::Preflight, 15),
        (DeadEndCondition::Defect, 17),
        (DeadEndCondition::ThermalTrip, 18),
    ] {
        assert_eq!(Exit::dead_end(condition).code(), want, "{condition}");
    }
}

#[test]
fn a_status_becomes_the_process_exit_code() {
    for exit in Exit::ALL {
        assert_eq!(
            format!("{:?}", std::process::ExitCode::from(exit)),
            format!("{:?}", std::process::ExitCode::from(exit.code())),
        );
    }
}
