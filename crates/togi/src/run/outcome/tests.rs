use jiff::Timestamp;
use jiff::tz::TimeZone;
use test_case::test_case;
use togi_journal::{
    DeadEnd, DeadEndCondition, Dimension, Event, EventKind, Payload, Refusal, Seq, Shutdown,
    ShutdownReason, Stamp,
};
use togi_machine::{BootId, PerCore};
use togi_render::{OutputFacts, Renderer};
use togi_session::{SessionError, Stop};

use super::{HiddenClosing, conclude, print_hidden};
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

#[test_case(&Stop::Signal ; "signal")]
fn a_signal_exits_zero_in_silence(stop: &Stop) {
    let (outcome, err) = conclusion(Some(stop), &[]);
    assert_eq!(outcome.exit, Exit::Ok);
    assert!(!outcome.reboot);
    assert_eq!(err, "");
}

#[test_case(SessionError::RecoveredElapsed(std::time::Duration::try_from(jiff::SignedDuration::from_secs(-1)).unwrap_err()); "inverted recovery interval")]
#[test_case(SessionError::WorkloadRotation(u8::try_from(256).unwrap_err()); "unrepresentable workload rotation")]
fn refused_recorded_numbers_exit_as_errors(error: SessionError) {
    let (outcome, err) = conclusion(None, &[error]);
    assert_eq!(outcome.exit, Exit::Error);
    assert!(!outcome.reboot);
    assert!(!err.is_empty());
}

#[test_case(DeadEndCondition::FailureAtZero, 10 ; "failure at zero")]
#[test_case(DeadEndCondition::Smu, 11 ; "smu")]
#[test_case(DeadEndCondition::NoEvidence, 12 ; "no evidence")]
#[test_case(DeadEndCondition::BootLoop, 13 ; "boot loop")]
#[test_case(DeadEndCondition::Containment, 14 ; "containment")]
#[test_case(DeadEndCondition::Preflight, 15 ; "preflight")]
#[test_case(DeadEndCondition::Defect, 17 ; "defect")]
#[test_case(DeadEndCondition::ThermalTrip, 18 ; "thermal trip")]
fn every_dead_end_exits_with_its_own_status_and_prints_its_detail(
    condition: DeadEndCondition,
    code: u8,
) {
    let (outcome, err) = conclusion(Some(&dead_end(condition, Vec::new(), false)), &[]);
    assert_eq!(outcome.exit.code(), code);
    assert_eq!(
        err,
        format!(
            "togi: dead end {}: operator intervention required\n",
            condition.as_str()
        )
    );
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
fn a_session_error_is_a_failure() {
    let other = SessionError::Panicked("read failed\x1b[2J\nforged".to_owned());
    let (outcome, err) = conclusion(None, &[other]);
    assert_eq!(outcome.exit, Exit::Error);
    assert_eq!(err, "togi run: panic: read failed\\x1b[2J\\nforged\n");
}

#[test]
fn a_log_cause_is_not_dropped_when_its_text_is_part_of_the_operation() {
    let (outcome, err) = conclusion(None, &[SessionError::Log(std::io::Error::other("log"))]);
    assert_eq!(outcome.exit, Exit::Error);
    assert_eq!(err, "togi run: write the event log: log\n");
}

#[test]
fn distinct_causes_with_identical_text_reach_the_cli_in_order() {
    #[derive(Debug)]
    struct Read(std::io::Error);
    impl std::fmt::Display for Read {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("read")
        }
    }
    impl std::error::Error for Read {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    let source = Read(std::io::Error::other("read"));
    let error = SessionError::Log(std::io::Error::other(source));
    let (outcome, err) = conclusion(None, &[error]);
    assert_eq!(outcome.exit, Exit::Error);
    assert_eq!(err, "togi run: write the event log: read: read\n");
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

fn closing(seq: u64, shutdown: bool) -> Event {
    event(
        seq,
        if shutdown {
            Payload::Shutdown(Shutdown {
                reason: ShutdownReason::Signal,
                kernel_cursor: None,
                kernel_error: None,
                cycles: None,
            })
        } else {
            Payload::ProfileRestored(togi_journal::ProfileRestored {
                offsets: PerCore::default(),
            })
        },
    )
}

#[test_case(&[2, 1], &["profile.restored", "shutdown"] ; "both")]
#[test_case(&[1], &["profile.restored"] ; "only the restoration")]
#[test_case(&[2], &["shutdown"] ; "only the shutdown")]
#[test_case(&[], &[] ; "neither")]
#[test_case(&[9], &[] ; "an unknown sequence number")]
fn only_the_events_the_dashboard_hid_are_printed_each_once_in_journal_order(
    hidden: &[u64],
    want: &[&str],
) {
    let events = [closing(1, false), closing(2, true), closing(3, true)];
    let hidden: Vec<Seq> = hidden.iter().map(|seq| Seq::new(*seq).unwrap()).collect();
    let mut err = Vec::new();
    print_hidden(
        &mut err,
        Renderer::default(),
        &TimeZone::UTC,
        &events,
        &hidden,
    );
    let text = String::from_utf8(err).unwrap();
    let kinds: Vec<&str> = text
        .lines()
        .map(|line| line.split_whitespace().nth(1).unwrap())
        .collect();
    assert_eq!(kinds, want, "{text}");
}

/// What a run appends, as `(kind, hidden by the dashboard)`, and the sequence numbers (1-based positions) the
/// tracker holds for the summary at the end.
#[test_case(
    &[
        (EventKind::TrialIntent, true),
        (EventKind::ProfileRestored, true),
        (EventKind::Shutdown, true),
    ],
    &[2, 3] ;
    "an ordinary stop"
)]
#[test_case(
    &[
        (EventKind::ProfileRestored, true),
        (EventKind::SessionWarning, true),
        (EventKind::Mce, true),
        (EventKind::Shutdown, true),
    ],
    &[1, 4] ;
    "warnings and machine checks of the shutdown's own read"
)]
#[test_case(
    &[
        (EventKind::ProfileRestored, true),
        (EventKind::TrialIntent, true),
        (EventKind::ProfileRestored, true),
        (EventKind::Shutdown, true),
    ],
    &[3, 4] ;
    "a resume's restoration at startup, then work, then the stop's own"
)]
#[test_case(
    &[
        (EventKind::ProfileRestored, true),
        (EventKind::TrialIntent, true),
        (EventKind::Shutdown, true),
    ],
    &[3] ;
    "a resume's restoration at startup, then work, and a stop that restores nothing"
)]
#[test_case(
    &[(EventKind::ProfileRestored, true), (EventKind::Shutdown, true)],
    &[1, 2] ;
    "a restoration at startup that no work follows is the stop's"
)]
#[test_case(
    &[(EventKind::ProfileRestored, true), (EventKind::Shutdown, false)],
    &[1] ;
    "the dashboard gave way between the two: the live shutdown is not tracked"
)]
#[test_case(
    &[(EventKind::ProfileRestored, false), (EventKind::Shutdown, false)],
    &[] ;
    "the dashboard gave way before the stop: nothing was hidden"
)]
#[test_case(
    &[
        (EventKind::ProfileRestored, true),
        (EventKind::TrialIntent, false),
        (EventKind::ProfileRestored, false),
        (EventKind::Shutdown, false),
    ],
    &[] ;
    "a startup restoration the dashboard hid, then the fallback, then a live stop"
)]
#[test_case(&[], &[] ; "no event")]
fn only_the_closing_events_of_the_stop_the_dashboard_hid_are_tracked(
    run: &[(EventKind, bool)],
    want: &[u64],
) {
    let mut closing = HiddenClosing::default();
    for (seq, (kind, hidden)) in (1..).zip(run) {
        closing.appended(*kind, Seq::new(seq).unwrap(), *hidden);
    }
    let held: Vec<u64> = closing.seqs().iter().map(|seq| seq.get()).collect();
    assert_eq!(held, want);
}
