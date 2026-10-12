use jiff::Timestamp;
use jiff::tz::TimeZone;
use togi_journal::{
    DeadEnd, DeadEndCondition, Dimension, Event, Payload, Refusal, Seq, SessionWarning, Shutdown,
    ShutdownReason, Stamp,
};
use togi_machine::{BootId, PerCore};
use togi_render::{OutputFacts, Renderer};
use togi_session::{SessionError, Stop};

use super::{conclude, print_clean_stop};
use crate::exit::Exit;

fn event(seq: u64, payload: Payload) -> Event {
    Event::new(
        Seq::new(seq).unwrap(),
        Stamp {
            time: Timestamp::UNIX_EPOCH,
            mono: None,
        },
        BootId::from("boot".to_owned()),
        Vec::new(),
        payload,
    )
}

fn dead_end(condition: DeadEndCondition, causes: Vec<Seq>, reboot: bool) -> Stop {
    Stop::DeadEnd {
        dead_end: DeadEnd {
            condition,
            core: None,
            detail: "operator intervention required".to_owned(),
            action: None,
        },
        causes,
        reboot,
    }
}

fn conclusion(stop: Option<&Stop>, errors: &[SessionError]) -> (super::Outcome, String) {
    let mut err = Vec::new();
    let outcome = conclude(
        &mut err,
        Renderer::default(),
        &TimeZone::UTC,
        stop,
        errors,
        &[],
    );
    (outcome, String::from_utf8(err).unwrap())
}

#[test]
fn a_signal_the_cycles_and_the_first_result_exit_zero_in_silence() {
    for stop in [Stop::Signal, Stop::Cycles, Stop::FirstResult] {
        let (outcome, err) = conclusion(Some(&stop), &[]);
        assert_eq!(outcome.exit, Exit::Ok, "{stop:?}");
        assert!(!outcome.reboot);
        assert_eq!(err, "");
    }
}

#[test]
fn every_dead_end_exits_with_its_own_status_and_prints_its_detail() {
    let table = [
        (DeadEndCondition::FailureAtZero, 10),
        (DeadEndCondition::Smu, 11),
        (DeadEndCondition::NoEvidence, 12),
        (DeadEndCondition::BootLoop, 13),
        (DeadEndCondition::Containment, 14),
        (DeadEndCondition::Preflight, 15),
        (DeadEndCondition::Defect, 17),
        (DeadEndCondition::ThermalTrip, 18),
    ];
    for (condition, code) in table {
        let (outcome, err) = conclusion(Some(&dead_end(condition, Vec::new(), false)), &[]);
        assert_eq!(outcome.exit.code(), code, "{condition:?}");
        assert_eq!(
            err,
            format!(
                "togi: dead end {}: operator intervention required\n",
                condition.as_str()
            )
        );
    }
}

#[test]
fn a_dead_end_that_asks_for_a_reboot_says_so() {
    let stop = dead_end(DeadEndCondition::BootLoop, Vec::new(), true);
    let (outcome, _) = conclusion(Some(&stop), &[]);
    assert!(outcome.reboot);
}

#[test]
fn the_summary_and_each_evidence_line_are_whole_decorated_lines_in_the_system_journal() {
    let renderer = Renderer::new(OutputFacts {
        is_terminal: false,
        no_color: false,
        journald: true,
    });
    let evidence = event(
        1,
        Payload::DeadEnd(DeadEnd {
            condition: DeadEndCondition::Smu,
            core: None,
            detail: "failed".to_owned(),
            action: None,
        }),
    );
    let stop = dead_end(DeadEndCondition::Smu, vec![evidence.seq], false);
    let mut err = Vec::new();
    let outcome = conclude(
        &mut err,
        renderer,
        &TimeZone::UTC,
        Some(&stop),
        &[],
        std::slice::from_ref(&evidence),
    );
    assert_eq!(outcome.exit, Exit::Smu);
    let text = String::from_utf8(err).unwrap();
    assert!(
        text.starts_with(
            "<3>\x1b[1;31mtogi: dead end smu: operator intervention required\x1b[0m\n"
        ),
        "{text:?}"
    );
    assert!(text.contains("\n<3>\x1b[1;31m  evidence: "), "{text:?}");
    assert!(text.ends_with("\x1b[0m\n"), "{text:?}");
}

#[test]
fn a_missing_core_is_a_usage_error_and_another_error_is_a_failure() {
    let core = togi_machine::CoreId::new(15).unwrap();
    let (outcome, err) = conclusion(None, &[SessionError::NoSuchCore { core }]);
    assert_eq!(outcome.exit, Exit::Usage);
    assert!(err.starts_with("togi run: "), "{err}");

    let other = SessionError::Panicked("read failed\x1b[2J\nforged".to_owned());
    let (outcome, err) = conclusion(None, &[other]);
    assert_eq!(outcome.exit, Exit::Error);
    assert_eq!(err, "togi run: panic: read failed\\x1b[2J\\nforged\n");
}

#[test]
fn a_refused_journal_is_exit_sixteen_and_beats_a_dead_end() {
    let refusal = Refusal {
        dimension: Dimension::Schema,
        recorded: 2,
        current: 1,
    };
    let stop = dead_end(DeadEndCondition::Smu, Vec::new(), false);
    let (outcome, err) = conclusion(Some(&stop), &[SessionError::Compatibility(refusal)]);
    assert_eq!(outcome.exit, Exit::Incompatible);
    assert_eq!(
        err,
        "togi run: the journal's schema is 2; this build's is 1\n"
    );
}

#[test]
fn errors_win_over_a_stop_and_every_error_is_printed() {
    let errors = [
        SessionError::Log(std::io::Error::other("full")),
        SessionError::Panicked("again".to_owned()),
    ];
    let (outcome, err) = conclusion(Some(&Stop::Signal), &errors);
    assert_eq!(outcome.exit, Exit::Error);
    assert_eq!(err.lines().count(), 2, "{err}");
}

#[test]
fn a_run_that_ended_without_a_stop_or_an_error_is_a_failure() {
    let (outcome, _) = conclusion(None, &[]);
    assert_eq!(outcome.exit, Exit::Error);
}

#[test]
fn the_clean_stop_repeats_the_restoration_and_the_shutdown_the_dashboard_hid() {
    let restored = event(
        2,
        Payload::ProfileRestored(togi_journal::ProfileRestored {
            offsets: PerCore::default(),
        }),
    );
    let shutdown = event(
        3,
        Payload::Shutdown(Shutdown {
            reason: ShutdownReason::Signal,
            kernel_cursor: None,
            kernel_error: None,
            cycles: None,
        }),
    );
    let warning = event(
        4,
        Payload::SessionWarning(SessionWarning {
            operation: "check".to_owned(),
            trial: None,
            error: "late".to_owned(),
        }),
    );
    let before = event(
        1,
        Payload::SessionWarning(SessionWarning {
            operation: "check".to_owned(),
            trial: None,
            error: "early".to_owned(),
        }),
    );
    let events = [before, restored, shutdown, warning];
    let mut err = Vec::new();
    print_clean_stop(&mut err, Renderer::default(), &TimeZone::UTC, &events);
    let text = String::from_utf8(err).unwrap();
    let kinds: Vec<&str> = text
        .lines()
        .map(|line| line.split_whitespace().nth(1).unwrap())
        .collect();
    assert_eq!(kinds, ["profile.restored", "shutdown"], "{text}");
}
