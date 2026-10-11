use std::cell::Cell;

use togi_machine::{BiosContext, Check};

use super::{Survey, diagnose_with};
use crate::checks::{CheckName, Skip, Skipped};
use crate::preflight::Outcome;

fn recorded() -> BiosContext {
    BiosContext {
        bios_version: "3.14".to_owned(),
        board: "ASRock X870E Taichi".to_owned(),
        cpu_model: "Test CPU".to_owned(),
        microcode: "0xb404038".to_owned(),
        boost_limit_mhz: 5750,
    }
}

struct Fake {
    steps_pass: bool,
    watchdog_passes: bool,
    bios: Option<BiosContext>,
    bios_reads: Cell<usize>,
}

impl Fake {
    fn passing() -> Self {
        Self {
            steps_pass: true,
            watchdog_passes: true,
            bios: Some(recorded()),
            bios_reads: Cell::new(0),
        }
    }
}

fn check(name: &str, ok: bool) -> Check {
    Check {
        name: name.to_owned(),
        detail: String::new(),
        ok,
    }
}

impl Survey for Fake {
    fn steps(&self, privileged: bool) -> Vec<Outcome> {
        let mut outcomes = vec![Outcome::Ran(check("cpu", self.steps_pass))];
        if !privileged {
            outcomes.push(Outcome::Skipped(Skipped {
                name: CheckName::Readback,
                reason: Skip::NeedsRoot,
            }));
        }
        outcomes
    }

    fn watchdog(&self) -> Check {
        check("watchdog", self.watchdog_passes)
    }

    fn bios_context(&self) -> Result<BiosContext, togi_smu::Error> {
        self.bios_reads.set(self.bios_reads.get() + 1);
        self.bios.clone().ok_or(togi_smu::Error::MailboxLost)
    }
}

fn names(checks: &[Check]) -> Vec<&str> {
    checks.iter().map(|check| check.name.as_str()).collect()
}

#[test]
fn a_matching_context_is_compared_after_every_other_check_passed() {
    let survey = Fake::passing();
    let diagnosis = diagnose_with(&survey, Some(&recorded()), true);
    assert_eq!(names(&diagnosis.ran), ["cpu", "watchdog", "bios_context"]);
    let bios = diagnosis.ran.last().unwrap();
    assert!(bios.ok);
    assert_eq!(bios.detail, "matches the session");
    assert!(diagnosis.skipped.is_empty());
    assert!(diagnosis.bios_context_error.is_none());
}

#[test]
fn a_changed_context_fails_its_check_and_says_run_archives_the_session() {
    let survey = Fake {
        bios: Some(BiosContext {
            bios_version: "3.10".to_owned(),
            ..recorded()
        }),
        ..Fake::passing()
    };
    let diagnosis = diagnose_with(&survey, Some(&recorded()), true);
    let bios = diagnosis.ran.last().unwrap();
    assert!(!bios.ok);
    assert_eq!(
        bios.detail,
        "bios_version is 3.10; the session recorded 3.14; run archives this session and starts a new one"
    );
}

#[test]
fn no_recorded_context_makes_no_comparison_and_no_skip() {
    let survey = Fake::passing();
    let diagnosis = diagnose_with(&survey, None, true);
    assert_eq!(names(&diagnosis.ran), ["cpu", "watchdog"]);
    assert!(diagnosis.skipped.is_empty());
    assert_eq!(survey.bios_reads.get(), 0);
}

#[test]
fn a_failed_check_skips_the_comparison_without_reading_the_context() {
    let survey = Fake {
        steps_pass: false,
        ..Fake::passing()
    };
    let diagnosis = diagnose_with(&survey, Some(&recorded()), true);
    assert_eq!(names(&diagnosis.ran), ["cpu", "watchdog"]);
    assert_eq!(
        diagnosis.skipped,
        [Skipped {
            name: CheckName::BiosContext,
            reason: Skip::NeedsOtherChecks,
        }]
    );
    assert_eq!(survey.bios_reads.get(), 0);
}

#[test]
fn without_root_the_comparison_needs_root_and_reads_nothing() {
    let survey = Fake::passing();
    let diagnosis = diagnose_with(&survey, Some(&recorded()), false);
    assert_eq!(
        diagnosis.skipped,
        [
            Skipped {
                name: CheckName::Readback,
                reason: Skip::NeedsRoot,
            },
            Skipped {
                name: CheckName::BiosContext,
                reason: Skip::NeedsRoot,
            },
        ]
    );
    assert_eq!(survey.bios_reads.get(), 0);
}

#[test]
fn a_failing_watchdog_does_not_gate_the_comparison() {
    let survey = Fake {
        watchdog_passes: false,
        ..Fake::passing()
    };
    let diagnosis = diagnose_with(&survey, Some(&recorded()), true);
    assert_eq!(names(&diagnosis.ran), ["cpu", "watchdog", "bios_context"]);
    assert_eq!(survey.bios_reads.get(), 1);
}

#[test]
fn a_context_that_cannot_be_read_keeps_the_checks_already_made() {
    let survey = Fake {
        bios: None,
        ..Fake::passing()
    };
    let diagnosis = diagnose_with(&survey, Some(&recorded()), true);
    assert_eq!(names(&diagnosis.ran), ["cpu", "watchdog"]);
    assert!(matches!(
        diagnosis.bios_context_error,
        Some(togi_smu::Error::MailboxLost)
    ));
}
