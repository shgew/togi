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

#[test]
fn a_dead_end_a_retry_and_a_warning_with_controls_are_drawn_escaped() {
    let cases: [(&str, Payload); 3] = [
        (
            "dead end",
            DeadEnd {
                condition: DeadEndCondition::Containment,
                core: None,
                detail: HOSTILE.to_owned(),
                action: None,
            }
            .into(),
        ),
        (
            "backend retry",
            BackendRetry {
                backend: RetryTarget::Mprime,
                attempt: 1,
                wait_s: 5,
                reason: HOSTILE.to_owned(),
            }
            .into(),
        ),
        ("session warning", warning(HOSTILE)),
    ];
    for (name, payload) in cases {
        let mut script = Script::at_limit();
        script.append(payload, &[]);
        let fx = Fixture::of(&script.events);
        let mut shown = false;
        for view in [View::Main, View::Log] {
            let sc = screen(view, 300, 40, Colours::Full, true);
            let (buf, _) = fx.draw(&sc, fx.now);
            assert_inert(&buf, &format!("{name} {view:?}"));
            shown |= squashed(&buf).contains(ESCAPED);
        }
        assert!(
            shown,
            "{name}: not visibly escaped in the main or the log view"
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
            _ => {}
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
