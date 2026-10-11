use std::io;

use jiff::tz::{self, TimeZone};
use serde_json::{Value, json};
use togi_journal::Event;

use crate::{OutputFacts, Renderer, Style, escape_text, style_for};

fn event_with(kind: &str, msg: &str, data: &Value) -> Event {
    let line = json!({
        "seq": 1,
        "time": "2026-10-02T01:14:07Z",
        "boot": "boot-a",
        "kind": kind,
        "msg": msg,
        "data": data,
    });
    Event::decode(&line.to_string()).unwrap()
}

fn event(kind: &str, data: &Value) -> Event {
    event_with(kind, "stored message", data)
}

fn utc() -> TimeZone {
    TimeZone::UTC
}

fn plain() -> Renderer {
    Renderer::default()
}

fn colored() -> Renderer {
    Renderer::new(OutputFacts { is_terminal: true, no_color: false, journald: false })
}

fn journald() -> Renderer {
    Renderer::new(OutputFacts { is_terminal: false, no_color: false, journald: true })
}

fn journald_no_color() -> Renderer {
    Renderer::new(OutputFacts { is_terminal: false, no_color: true, journald: true })
}

fn trial_end(outcome: &str) -> Event {
    event("trial.end", &json!({"trial": "0413", "outcome": outcome, "duration_s": 90}))
}

fn failure_event() -> Event {
    event("failure", &json!({"signal": "crash", "attribution": "attributed"}))
}

fn decision(decision: &str) -> Event {
    event(
        "tuner.decision",
        &json!({
            "core": 7, "activity": "search", "decision": decision,
            "from_offset": -32, "to_offset": -37, "reason": "r",
        }),
    )
}

fn transition(from: Option<&str>, to: &str) -> Event {
    let mut data = json!({"core": 7, "to": to, "offset": -32, "reason": "r"});
    if let Some(from) = from {
        data["from"] = json!(from);
    }
    event("core.phase", &data)
}

fn cycle(kind_event: &str, passed: bool, full: bool) -> Event {
    event("checking.cycle", &json!({"cycle": 1, "event": kind_event, "passed": passed, "full": full}))
}

fn round(kind_event: &str, passed: bool) -> Event {
    event("deepening.round", &json!({"round": 1, "event": kind_event, "passed": passed}))
}

fn hunt_end(result: &str) -> Event {
    event("hunt.end", &json!({"hunt": 1, "result": result, "groups": 2, "reason": "r"}))
}

fn hunt_group(inferred: Option<&str>) -> Event {
    let mut data = json!({
        "hunt": 1, "group": 1, "cores": [1], "profile": {}, "set": [1], "granularity": 1,
        "stage": "probe", "index": 0, "duration_s": 60,
    });
    if let Some(inferred) = inferred {
        data["inferred"] = json!(inferred);
    }
    event("hunt.group", &data)
}

fn hunt_start() -> Event {
    event(
        "hunt.start",
        &json!({
            "hunt": 1, "failure": 5, "regime": "R7", "workload": "mprime-avx2-36k-248k", "cores": [1, 2],
            "duration_s": 60, "failing": {"1": -20}, "parked": {"1": 0}, "candidates": [1],
            "trials": 3, "trial_s": 60, "miss": 0.01, "rate": 0.5, "ranking": [1, 2],
        }),
    )
}

fn style_cases() -> Vec<(&'static str, Event, Style)> {
    vec![
        ("failed trial end", trial_end("failure"), Style::Red),
        ("inconclusive trial end", trial_end("inconclusive"), Style::Dim),
        ("passed trial end", trial_end("pass"), Style::Plain),
        ("attributed failure", failure_event(), Style::Red),
        (
            "unattributed failure",
            event("failure", &json!({"signal": "stall", "attribution": "unattributed"})),
            Style::Red,
        ),
        ("crash", event("crash.detected", &json!({"previous_boot": "b"})), Style::Red),
        (
            "dead end",
            event("deadend", &json!({"condition": "failure_at_zero", "detail": "d"})),
            Style::RedBold,
        ),
        ("step deeper", decision("step_deeper"), Style::Green),
        ("deepen", decision("deepen"), Style::Green),
        ("backoff", decision("backoff"), Style::Yellow),
        ("yield", decision("yield"), Style::Yellow),
        ("check solo limit", decision("check_solo_limit"), Style::Plain),
        ("hold", decision("hold"), Style::Plain),
        ("search to room", transition(Some("search"), "has_room"), Style::GreenBold),
        ("search to limit", transition(Some("search"), "at_limit"), Style::GreenBold),
        ("room to limit", transition(Some("has_room"), "at_limit"), Style::GreenBold),
        ("initial limit", transition(None, "at_limit"), Style::GreenBold),
        ("initial room", transition(None, "has_room"), Style::Plain),
        ("room to room", transition(Some("has_room"), "has_room"), Style::Plain),
        ("limit back to search", transition(Some("at_limit"), "search"), Style::Plain),
        ("limit to room", transition(Some("at_limit"), "has_room"), Style::Plain),
        ("initial search", transition(None, "search"), Style::Plain),
        ("full passed cycle end", cycle("end", true, true), Style::GreenBold),
        ("non-full passed cycle end", cycle("end", true, false), Style::Plain),
        ("failed full cycle end", cycle("end", false, true), Style::Plain),
        ("full passed cycle start", cycle("start", true, true), Style::Plain),
        ("passed round end", round("end", true), Style::GreenBold),
        ("failed round end", round("end", false), Style::Plain),
        ("passed round start", round("start", true), Style::Plain),
        ("hunt cancelled", hunt_end("cancelled"), Style::Plain),
        ("hunt direct", hunt_end("direct"), Style::Green),
        ("hunt culprit", hunt_end("culprit"), Style::Green),
        ("hunt combination", hunt_end("combination"), Style::Green),
        ("hunt fallback", hunt_end("fallback"), Style::Plain),
        ("hunt loaded", hunt_end("loaded"), Style::Plain),
        ("hunt start", hunt_start(), Style::Yellow),
        ("hunt group without inference", hunt_group(None), Style::Plain),
        ("hunt group inferred pass", hunt_group(Some("pass")), Style::Plain),
        ("hunt group inferred failure", hunt_group(Some("failure")), Style::Plain),
        (
            "hunt skipped",
            event("hunt.skipped", &json!({"failure": 5, "reason": "held"})),
            Style::Plain,
        ),
        (
            "combination",
            event("combination", &json!({"combination": 1, "members": [], "hunt": 1, "reason": "r"})),
            Style::Yellow,
        ),
        (
            "tuner warning",
            event("tuner.warning", &json!({"warning": "monotonicity", "passes": [], "detail": "d"})),
            Style::Yellow,
        ),
        (
            "session warning",
            event("session.warning", &json!({"operation": "write state", "error": "disk full"})),
            Style::Yellow,
        ),
        (
            "defect found",
            event(
                "defect.found",
                &json!({
                    "id": 1, "title": "t", "detail": "d", "pr": 1,
                    "direction": "too_cautious", "cores": [2], "decisions": [],
                }),
            ),
            Style::Yellow,
        ),
        (
            "defect answered",
            event("defect.answered", &json!({"id": 1, "cores": [2], "answer": "yes"})),
            Style::Plain,
        ),
        (
            "backend retry",
            event(
                "backend.retry",
                &json!({"backend": "kernel_log", "attempt": 2, "wait_s": 30, "reason": "r"}),
            ),
            Style::Dim,
        ),
        ("host ranking", event("host.ranking", &json!({"ranking": [3, 1]})), Style::Plain),
        (
            "session archived",
            event("session.archived", &json!({"session": "20261002T011407Z"})),
            Style::Plain,
        ),
    ]
}

#[test]
fn style_follows_the_semantic_table() {
    for (name, event, expected) in style_cases() {
        assert_eq!(style_for(&event.payload), expected, "{name}");
    }
}

#[test]
fn style_is_decided_by_payload_not_message() {
    let hostile = event_with(
        "trial.end",
        "FAIL crash dead end",
        &json!({"trial": "1", "outcome": "pass", "duration_s": 1}),
    );
    assert_eq!(style_for(&hostile.payload), Style::Plain);
}

#[test]
fn every_nonplain_style_wraps_the_whole_line_in_owned_sgr() {
    let renderer = colored();
    for (style, open) in [
        (Style::Red, "\x1b[31m"),
        (Style::RedBold, "\x1b[1;31m"),
        (Style::Green, "\x1b[32m"),
        (Style::GreenBold, "\x1b[1;32m"),
        (Style::Yellow, "\x1b[33m"),
        (Style::Dim, "\x1b[2m"),
    ] {
        assert_eq!(renderer.text(style, "line").to_string(), format!("{open}line\x1b[0m"));
    }
}

#[test]
fn a_plain_style_gets_neither_sgr_nor_reset() {
    assert_eq!(colored().text(Style::Plain, "line").to_string(), "line");
    let line = colored().line(&trial_end("pass"), &utc()).to_string();
    assert!(!line.contains('\x1b'));
}

#[test]
fn a_colored_event_line_is_wrapped_once() {
    let line = colored().line(&failure_event(), &utc()).to_string();
    assert_eq!(line, "\x1b[31m01:14:07 failure        stored message\x1b[0m");
}

#[test]
fn the_default_renderer_is_plain() {
    assert_eq!(Renderer::default(), plain());
    assert_eq!(
        plain().line(&failure_event(), &utc()).to_string(),
        "01:14:07 failure        stored message"
    );
}

#[test]
fn stream_policy_as_data() {
    let policy = |is_terminal, no_color, journald| {
        Renderer::new(OutputFacts { is_terminal, no_color, journald })
    };
    let fail = failure_event();
    let pass = trial_end("pass");
    let zone = utc();
    let red = |renderer: Renderer| renderer.line(&fail, &zone).to_string();
    let body = "01:14:07 failure        stored message";

    assert_eq!(red(policy(false, false, false)), body);
    assert_eq!(red(policy(true, false, false)), format!("\x1b[31m{body}\x1b[0m"));
    assert_eq!(red(policy(true, true, false)), body);
    assert_eq!(red(policy(false, false, true)), format!("<3>\x1b[31m{body}\x1b[0m"));
    assert_eq!(red(policy(false, true, true)), format!("<3>{body}"));
    assert_eq!(red(policy(true, true, true)), format!("<3>{body}"));
    assert_eq!(red(policy(true, false, true)), format!("<3>\x1b[31m{body}\x1b[0m"));
    assert_eq!(
        policy(false, false, true).line(&pass, &zone).to_string(),
        "01:14:07 trial.end      stored message"
    );
}

#[test]
fn journald_priority_applies_to_red_and_red_bold_only() {
    let renderer = journald();
    let zone = utc();
    let dead_end = event("deadend", &json!({"condition": "smu", "detail": "d"}));
    assert!(renderer.line(&dead_end, &zone).to_string().starts_with("<3>\x1b[1;31m"));
    assert!(renderer.line(&failure_event(), &zone).to_string().starts_with("<3>\x1b[31m"));
    for other in [decision("step_deeper"), decision("backoff"), trial_end("inconclusive")] {
        let line = renderer.line(&other, &zone).to_string();
        assert!(!line.contains("<3>"), "{line}");
    }
    assert!(renderer.text(Style::RedBold, "refused").to_string().starts_with("<3>"));
    assert!(!renderer.text(Style::Yellow, "warn").to_string().contains("<3>"));
    assert_eq!(journald_no_color().text(Style::RedBold, "refused").to_string(), "<3>refused");
    assert_eq!(colored().text(Style::RedBold, "refused").to_string(), "\x1b[1;31mrefused\x1b[0m");
}

#[test]
fn no_color_keeps_the_priority_prefix_but_drops_ansi() {
    let line = journald_no_color().line(&failure_event(), &utc()).to_string();
    assert_eq!(line, "<3>01:14:07 failure        stored message");
}

#[test]
fn hostile_messages_cannot_break_the_owned_style() {
    let hostile = event_with(
        "failure",
        "boom\x1b[0m\x1b[32m\nforged line\r\x1b]0;x\x07",
        &json!({"signal": "crash", "attribution": "attributed"}),
    );
    let line = colored().line(&hostile, &utc()).to_string();
    assert!(line.starts_with("\x1b[31m01:14:07 failure        boom"));
    assert!(line.ends_with("\x1b[0m"));
    assert_eq!(line.matches('\x1b').count(), 2);
    assert!(!line.contains('\n') && !line.contains('\r') && !line.contains('\x07'));
    assert!(line.contains("boom\\x1b[0m\\x1b[32m\\nforged line\\r\\x1b]0;x\\x07"));
}

#[test]
fn hostile_text_is_escaped_before_styling() {
    let line = colored().text(Style::Yellow, "a\x1b[2Jb\n").to_string();
    assert_eq!(line, "\x1b[33ma\\x1b[2Jb\\n\x1b[0m");
    let journald_line = journald().text(Style::Red, "\x1b[0m<3>").to_string();
    assert_eq!(journald_line, "<3>\x1b[31m\\x1b[0m<3>\x1b[0m");
}

#[test]
fn the_prefix_is_escaped_once_and_inside_the_style() {
    let prefixed = colored()
        .prefixed_line(&failure_event(), &utc(), "  evidence\x1b[0m\\x: ")
        .to_string();
    assert_eq!(
        prefixed,
        "\x1b[31m  evidence\\x1b[0m\\x: 01:14:07 failure        stored message\x1b[0m"
    );
    assert_eq!(prefixed.matches("\x1b[0m").count(), 1);
    let plain_prefixed = plain().prefixed_line(&trial_end("pass"), &utc(), "  evidence: ").to_string();
    assert_eq!(plain_prefixed, "  evidence: 01:14:07 trial.end      stored message");
    assert_eq!(
        journald().prefixed_line(&failure_event(), &utc(), "p ").to_string(),
        "<3>\x1b[31mp 01:14:07 failure        stored message\x1b[0m"
    );
}

#[test]
fn local_time_uses_the_given_zone() {
    let zone = TimeZone::fixed(tz::offset(5));
    assert_eq!(
        plain().line(&failure_event(), &zone).to_string(),
        "06:14:07 failure        stored message"
    );
    let behind = TimeZone::fixed(tz::offset(-2));
    assert_eq!(
        plain().line(&failure_event(), &behind).to_string(),
        "23:14:07 failure        stored message"
    );
    let ahead = TimeZone::fixed(tz::offset(23));
    assert!(plain().line(&failure_event(), &ahead).to_string().starts_with("00:14:07 "));
}

#[test]
fn kinds_are_padded_to_fourteen_and_longer_ones_expand() {
    let short = plain().line(&trial_end("pass"), &utc()).to_string();
    assert_eq!(short, "01:14:07 trial.end      stored message");
    let exact = plain().line(&decision("hold"), &utc()).to_string();
    assert_eq!(exact, "01:14:07 tuner.decision stored message");
    let long = event("session.archived", &json!({"session": "20261002T011407Z"}));
    assert_eq!(
        plain().line(&long, &utc()).to_string(),
        "01:14:07 session.archived stored message"
    );
}

#[test]
fn the_stored_message_is_rendered_not_regenerated() {
    let stored = event_with(
        "trial.end",
        "operator edited text",
        &json!({"trial": "1", "outcome": "pass", "duration_s": 1}),
    );
    assert_eq!(stored.msg, "operator edited text");
    assert!(plain().line(&stored, &utc()).to_string().ends_with(" operator edited text"));
}

#[test]
fn the_stored_message_keeps_readable_unicode() {
    let unicode = event_with(
        "trial.end",
        "PASS 90s | Tctl max 71°C \u{fffd}",
        &json!({"trial": "1", "outcome": "pass", "duration_s": 1}),
    );
    assert!(plain().line(&unicode, &utc()).to_string().ends_with("PASS 90s | Tctl max 71°C \u{fffd}"));
}

#[test]
fn rendering_leaves_the_journal_serialisation_untouched() {
    let hostile = event_with(
        "failure",
        "boom\x1b[31m",
        &json!({"signal": "crash", "attribution": "attributed"}),
    );
    let before = hostile.encode().unwrap();
    let rendered = journald().line(&hostile, &utc()).to_string();
    assert!(rendered.starts_with("<3>"));
    let after = hostile.encode().unwrap();
    assert_eq!(before, after);
    assert!(!after.contains('\x1b'));
    assert!(!after.contains("<3>"));
    assert_eq!(hostile.msg, "boom\x1b[31m");
}

#[test]
fn write_event_ends_with_exactly_one_newline() {
    let hostile = event_with(
        "failure",
        "first\nsecond\n",
        &json!({"signal": "crash", "attribution": "attributed"}),
    );
    let mut out = Vec::new();
    colored().write_event(&mut out, &hostile, &utc()).unwrap();
    let text = String::from_utf8(out).unwrap();
    assert_eq!(text.matches('\n').count(), 1);
    assert!(text.ends_with("\x1b[0m\n"));
    assert!(text.contains("first\\nsecond\\n"));
}

#[test]
fn write_event_appends_one_line_per_call() {
    let mut out = Vec::new();
    plain().write_event(&mut out, &failure_event(), &utc()).unwrap();
    plain().write_event(&mut out, &trial_end("pass"), &utc()).unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "01:14:07 failure        stored message\n01:14:07 trial.end      stored message\n"
    );
}

struct FailingWriter {
    fail_after: usize,
}

impl io::Write for FailingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.fail_after == 0 {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "sink closed"));
        }
        let take = buf.len().min(self.fail_after);
        self.fail_after -= take;
        Ok(take)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn write_event_propagates_the_sink_error() {
    for fail_after in [0, 5, 30] {
        let mut sink = FailingWriter { fail_after };
        let error = plain().write_event(&mut sink, &failure_event(), &utc()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::BrokenPipe, "{fail_after}");
    }
}

#[test]
fn write_event_accepts_an_unsized_sink() {
    let mut out: Vec<u8> = Vec::new();
    let sink: &mut dyn io::Write = &mut out;
    plain().write_event(sink, &failure_event(), &utc()).unwrap();
    assert!(out.ends_with(b"\n"));
}

#[test]
fn display_adapters_do_not_borrow_the_renderer() {
    let fail = failure_event();
    let zone = utc();
    let line = {
        let renderer = colored();
        renderer.line(&fail, &zone)
    };
    assert!(line.to_string().starts_with("\x1b[31m"));
    assert_eq!(escape_text("x").to_string(), "x");
}
