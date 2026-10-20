//! Untrusted text from the journal is drawn visibly escaped and never reaches the terminal as a control.

use jiff::tz::TimeZone;
use ratatui::buffer::Buffer;
use togi_journal::{BackendRetry, DeadEnd, DeadEndCondition, Payload, RetryTarget, TrialId};
use togi_watchtest::Scenario;

use super::{Fixture, draw_journal, rows_of, some_time};
use crate::frames::testing::screen;
use crate::screen::{Colours, Journal, Screen, View};
use crate::tests::kit::{Script, warning};

const HOSTILE: &str =
    "日本語\x1b[2J\x1b]52;c;data\x07\r\n\u{9b}31m\u{9d}0;title\u{9c}\u{2028}\u{2029}";
const ESCAPED: &str =
    r"\u65e5\u672c\u8a9e\x1b[2J\x1b]52;c;data\x07\r\n\u009b31m\u009d0;title\u009c\u2028\u2029";
const HOSTILE_ID: &str = "日本語\x1b]0;title\x07\r\n\u{9b}2J";
const ESCAPED_ID: &str = r"\u65e5\u672c\u8a9e\x1b]0;title\x07\r\n\u009b2J";

fn assert_inert(buf: &Buffer, what: &str) {
    for (row, text) in rows_of(buf).iter().enumerate() {
        for c in text.chars() {
            assert!(
                !c.is_control() && c != '\u{2028}' && c != '\u{2029}',
                "{what} row {row} draws the control {c:?}: {text:?}"
            );
        }
    }
}

fn squashed(buf: &Buffer) -> String {
    rows_of(buf)
        .iter()
        .flat_map(|row| row.split_whitespace())
        .collect()
}

#[test]
fn a_problem_with_controls_in_its_text_draws_them_visibly_escaped_live_and_in_one_frame() {
    let error = std::io::Error::other(HOSTILE);
    for one_frame in [false, true] {
        for view in [View::Main, View::Help, View::Log] {
            let sc = Screen {
                one_frame,
                ..screen(view, 300, 40, Colours::Full, !one_frame)
            };
            let (buf, _) =
                draw_journal(&Journal::Problem(&error), &sc, some_time(), &TimeZone::UTC);
            assert_inert(&buf, &format!("one_frame {one_frame} {view:?}"));
            if view == View::Main {
                assert!(
                    squashed(&buf).contains(ESCAPED),
                    "one_frame {one_frame}: the diagnostic is not visibly escaped, even across wrapped lines:\n{}",
                    super::text(&buf)
                );
            }
        }
    }
}

#[test_case::test_case("dead end", DeadEnd { condition: DeadEndCondition::Containment, core: None, detail: HOSTILE.to_owned(), action: None }.into(), View::Main; "dead end main")]
#[test_case::test_case("dead end", DeadEnd { condition: DeadEndCondition::Containment, core: None, detail: HOSTILE.to_owned(), action: None }.into(), View::Log; "dead end log")]
#[test_case::test_case("backend retry", BackendRetry { backend: RetryTarget::Mprime, attempt: 1, wait_s: 5, reason: HOSTILE.to_owned() }.into(), View::Main; "backend retry main")]
#[test_case::test_case("backend retry", BackendRetry { backend: RetryTarget::Mprime, attempt: 1, wait_s: 5, reason: HOSTILE.to_owned() }.into(), View::Log; "backend retry log")]
#[test_case::test_case("session warning", warning(HOSTILE), View::Main; "session warning main")]
#[test_case::test_case("session warning", warning(HOSTILE), View::Log; "session warning log")]
fn a_dead_end_a_retry_and_a_warning_with_controls_are_drawn_escaped(
    name: &str,
    payload: Payload,
    view: View,
) {
    let mut script = Script::at_limit();
    script.append(payload, &[]);
    let fx = Fixture::of(&script.events);
    let sc = screen(view, 300, 40, Colours::Full, true);
    let (buf, _) = fx.draw(&sc, fx.now);
    assert_inert(&buf, &format!("{name} {view:?}"));
    if view == View::Main {
        assert!(
            squashed(&buf).contains(ESCAPED),
            "{name}: not visibly escaped in the main view"
        );
    }
}

#[test]
fn a_trial_id_with_controls_is_drawn_escaped() {
    let mut events = Scenario::Crashed.events().expect("the scenario's journal");
    let old = Fixture::of(&events)
        .model
        .snapshot()
        .trial
        .expect("a crashed trial")
        .id
        .clone();
    let hostile = TrialId::from(HOSTILE_ID.to_owned());
    for event in &mut events {
        match &mut event.payload {
            Payload::TrialIntent(intent) if intent.trial == old => intent.trial = hostile.clone(),
            Payload::TrialStart(start) if start.trial == old => start.trial = hostile.clone(),
            Payload::SessionStart(_)
            | Payload::SessionContext(_)
            | Payload::SessionBaseline(_)
            | Payload::SessionNotice(_)
            | Payload::SessionWarning(_)
            | Payload::SessionArchived(_)
            | Payload::SessionCarried(_)
            | Payload::ConfigLoaded(_)
            | Payload::PreflightCheck(_)
            | Payload::SmuIntent(_)
            | Payload::SmuWrite(_)
            | Payload::SmuReadback(_)
            | Payload::SmuError(_)
            | Payload::ProfileApplied(_)
            | Payload::ProfileChange(_)
            | Payload::ProfileRestored(_)
            | Payload::TrialIntent(_)
            | Payload::TrialStart(_)
            | Payload::TrialProgress(_)
            | Payload::TrialSignal(_)
            | Payload::TrialSample(_)
            | Payload::TrialEnd(_)
            | Payload::TrialCarried(_)
            | Payload::Failure(_)
            | Payload::FailureCarried(_)
            | Payload::Mce(_)
            | Payload::CrashDetected(_)
            | Payload::TunerDecision(_)
            | Payload::CorePhase(_)
            | Payload::CheckingCycle(_)
            | Payload::CheckingStep(_)
            | Payload::CheckingChain(_)
            | Payload::HostRanking(_)
            | Payload::HuntStart(_)
            | Payload::HuntGroup(_)
            | Payload::HuntEnd(_)
            | Payload::HuntSkipped(_)
            | Payload::Combination(_)
            | Payload::DeepeningRound(_)
            | Payload::TunerWarning(_)
            | Payload::BackendRetry(_)
            | Payload::CommandReset(_)
            | Payload::DefectFound(_)
            | Payload::DefectAnswered(_)
            | Payload::DeadEnd(_)
            | Payload::BootSavedEntry(_)
            | Payload::BootLeaveReason(_)
            | Payload::Shutdown(_)
            | Payload::JournalTorn(_)
            | Payload::DayBootStart(_)
            | Payload::DayCheckpoint(_)
            | Payload::DayOffsetsChanged(_)
            | Payload::DayCleanStop(_)
            | Payload::DayUncleanEnd(_)
            | Payload::DayAnswer(_)
            | Payload::StateRebuilt(_) => {}
        }
    }
    let fx = Fixture::of(&events);
    assert!(
        fx.model
            .snapshot()
            .trial
            .is_some_and(|trial| trial.id == hostile)
    );
    for view in [View::Main, View::Log] {
        let (buf, _) = fx.draw(&screen(view, 300, 40, Colours::Full, true), fx.now);
        assert_inert(&buf, &format!("{view:?}"));
    }
    let (main, _) = fx.draw(&screen(View::Main, 300, 40, Colours::Full, true), fx.now);
    assert!(
        squashed(&main).contains(ESCAPED_ID),
        "the trial id is not visibly escaped:\n{}",
        super::text(&main)
    );
}
