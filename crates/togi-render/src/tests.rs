use std::io;

use jiff::tz::{self, TimeZone};
use serde_json::{Value, json};
use test_case::test_case;
use togi_journal::{Event, Payload};

use crate::{OutputFacts, Renderer, Style, escape_text, style_for};

mod ruleset14;

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
    Renderer::new(OutputFacts {
        is_terminal: true,
        no_color: false,
        journald: false,
    })
}

fn journald() -> Renderer {
    Renderer::new(OutputFacts {
        is_terminal: false,
        no_color: false,
        journald: true,
    })
}

fn journald_no_color() -> Renderer {
    Renderer::new(OutputFacts {
        is_terminal: false,
        no_color: true,
        journald: true,
    })
}

fn trial_end(outcome: &str) -> Event {
    event(
        "trial.end",
        &json!({"trial": "0413", "outcome": outcome, "duration_s": 90}),
    )
}

fn failure_event() -> Event {
    event(
        "failure",
        &json!({"signal": "crash", "attribution": "attributed"}),
    )
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
    event(
        "checking.cycle",
        &json!({"cycle": 1, "event": kind_event, "passed": passed, "full": full}),
    )
}

fn round(kind_event: &str, passed: bool) -> Event {
    event(
        "deepening.round",
        &json!({"round": 1, "event": kind_event, "passed": passed}),
    )
}

fn hunt_end(result: &str) -> Event {
    event(
        "hunt.end",
        &json!({"hunt": 1, "result": result, "groups": 2, "reason": "r"}),
    )
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

#[test_case(&trial_end("failure"), Style::Red; "failed trial end")]
#[test_case(&trial_end("inconclusive"), Style::Dim; "inconclusive trial end")]
#[test_case(&trial_end("pass"), Style::Plain; "passed trial end")]
#[test_case(&failure_event(), Style::Red; "attributed failure")]
#[test_case(&event(
                "failure",
                &json!({"signal": "stall", "attribution": "unattributed"}),
            ), Style::Red; "unattributed failure")]
#[test_case(&event("crash.detected", &json!({"previous_boot": "b"})), Style::Red; "crash")]
#[test_case(&event(
                "deadend",
                &json!({"condition": "failure_at_zero", "detail": "d"}),
            ), Style::RedBold; "dead end")]
#[test_case(&decision("step_deeper"), Style::Plain; "step deeper")]
#[test_case(&decision("deepen"), Style::Plain; "deepen")]
#[test_case(&decision("backoff"), Style::Plain; "backoff")]
#[test_case(&decision("yield"), Style::Plain; "yield")]
#[test_case(&decision("check_solo_limit"), Style::Plain; "check solo limit")]
#[test_case(&decision("hold"), Style::Plain; "hold")]
#[test_case(&transition(Some("search"), "has_room"), Style::Plain; "search to room")]
#[test_case(&transition(Some("search"), "at_limit"), Style::Plain; "search to limit")]
#[test_case(&transition(Some("has_room"), "at_limit"), Style::Plain; "room to limit")]
#[test_case(&transition(None, "at_limit"), Style::Plain; "initial limit")]
#[test_case(&transition(None, "has_room"), Style::Plain; "initial room")]
#[test_case(&transition(Some("has_room"), "has_room"), Style::Plain; "room to room")]
#[test_case(&transition(Some("at_limit"), "search"), Style::Plain; "limit back to search")]
#[test_case(&transition(Some("at_limit"), "has_room"), Style::Plain; "limit to room")]
#[test_case(&transition(None, "search"), Style::Plain; "initial search")]
#[test_case(&cycle("end", true, true), Style::Plain; "full passed cycle end")]
#[test_case(&cycle("end", true, false), Style::Plain; "non-full passed cycle end")]
#[test_case(&cycle("end", false, true), Style::Plain; "failed full cycle end")]
#[test_case(&cycle("start", true, true), Style::Plain; "full passed cycle start")]
#[test_case(&round("end", true), Style::Plain; "passed round end")]
#[test_case(&round("end", false), Style::Plain; "failed round end")]
#[test_case(&round("start", true), Style::Plain; "passed round start")]
#[test_case(&hunt_end("cancelled"), Style::Plain; "hunt cancelled")]
#[test_case(&hunt_end("direct"), Style::Plain; "hunt direct")]
#[test_case(&hunt_end("culprit"), Style::Plain; "hunt culprit")]
#[test_case(&hunt_end("combination"), Style::Plain; "hunt combination")]
#[test_case(&hunt_end("fallback"), Style::Plain; "hunt fallback")]
#[test_case(&hunt_end("loaded"), Style::Plain; "hunt loaded")]
#[test_case(&hunt_start(), Style::Plain; "hunt start")]
#[test_case(&hunt_group(None), Style::Plain; "hunt group without inference")]
#[test_case(&hunt_group(Some("pass")), Style::Plain; "hunt group inferred pass")]
#[test_case(&hunt_group(Some("failure")), Style::Plain; "hunt group inferred failure")]
#[test_case(&event(
                "hunt.group",
                &json!({
                    "hunt": 4, "group": 1, "cores": [3, 11], "profile": {}, "set": [3, 11],
                    "granularity": 2, "stage": "probe", "index": 0, "duration_s": 0,
                    "skipped": true, "reason": "already checked",
                }),
            ), Style::Plain; "hunt group skipped")]
#[test_case(&event(
                "hunt.group",
                &json!({
                    "hunt": 4, "group": 5, "cores": [3], "profile": {"3": -40, "11": -22},
                    "set": [3, 11], "granularity": 1, "stage": "probe", "index": 0,
                    "duration_s": 120, "probe": {"core": 11, "offset": -22},
                    "held": [{"core": 3, "offset": -40}],
                }),
            ), Style::Plain; "hunt group with probe and held members")]
#[test_case(&event("hunt.skipped", &json!({"failure": 5, "reason": "held"})), Style::Plain; "hunt skipped")]
#[test_case(&event(
                "combination",
                &json!({"combination": 1, "members": [], "hunt": 1, "reason": "r"}),
            ), Style::Plain; "combination")]
#[test_case(&event(
                "tuner.warning",
                &json!({"warning": "monotonicity", "passes": [], "detail": "d"}),
            ), Style::Yellow; "tuner warning")]
#[test_case(&event(
                "session.warning",
                &json!({"operation": "write state", "error": "disk full"}),
            ), Style::Yellow; "session warning")]
#[test_case(&event(
                "defect.found",
                &json!({
                    "id": 1, "title": "t", "detail": "d", "pr": 1,
                    "direction": "too_cautious", "cores": [2], "decisions": [],
                }),
            ), Style::Yellow; "defect found")]
#[test_case(&event(
                "defect.answered",
                &json!({"id": 1, "cores": [2], "answer": "yes"}),
            ), Style::Plain; "defect answered")]
#[test_case(&event(
                "backend.retry",
                &json!({"backend": "kernel_log", "attempt": 2, "wait_s": 30, "reason": "r"}),
            ), Style::Dim; "backend retry")]
#[test_case(&event("host.ranking", &json!({"ranking": [3, 1]})), Style::Plain; "host ranking")]
#[test_case(&event("host.ranking", &json!({"ranking": [], "detail": "missing"})), Style::Plain; "host ranking unavailable")]
#[test_case(&event("session.baseline", &json!({"offsets": {"0": 0, "1": -5}})), Style::Plain; "session baseline")]
#[test_case(&event("session.archived", &json!({"session": "20261002T011407Z"})), Style::Plain; "session archived")]
#[test_case(&event("ccd.edge", &json!({"ccd": 0, "edge": {"kind": "observed", "offset": -25, "witness": 40}, "old": {"kind": "unknown"}, "reason": "first failure"})), Style::Red; "observed edge")]
#[test_case(&event("ccd.edge", &json!({"ccd": 0, "edge": {"kind": "unknown"}, "old": {"kind": "observed", "offset": -25, "witness": 40}, "reason": "evidence withdrawn"})), Style::Plain; "unknown edge")]
#[test_case(&event("day.veto", &json!({"event": {"kind": "boot", "boot": "b"}, "profile": {"kind": "per_core", "cores": {"0": -15}}, "state": "active", "evidence": [7], "reason": "cpu suspect"})), Style::Yellow; "active veto")]
#[test_case(&event("day.veto", &json!({"event": {"kind": "boot", "boot": "b"}, "profile": {"kind": "per_core", "cores": {"0": -15}}, "state": "cleared", "evidence": [7], "reason": "cleared"})), Style::Plain; "cleared veto")]
#[test_case(&event("day.freeze", &json!({"choice": "freeze", "source": {"kind": "command", "uid": 1000}})), Style::Plain; "freeze")]
fn style_follows_the_semantic_table(event: &Event, expected: Style) {
    assert_eq!(style_for(&event.payload), expected);
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

#[test_case(Style::Red, "\x1b[31m"; "red")]
#[test_case(Style::RedBold, "\x1b[1;31m"; "red bold")]
#[test_case(Style::Green, "\x1b[32m"; "green")]
#[test_case(Style::Yellow, "\x1b[33m"; "yellow")]
#[test_case(Style::Dim, "\x1b[2m"; "dim")]
fn every_nonplain_style_wraps_the_whole_line_in_owned_sgr(style: Style, open: &str) {
    assert_eq!(
        colored().text(style, "line").to_string(),
        format!("{open}line\x1b[0m")
    );
}

#[test_case("cancelled"; "cancelled")]
#[test_case("direct"; "direct")]
#[test_case("culprit"; "culprit")]
#[test_case("combination"; "combination")]
#[test_case("fallback"; "fallback")]
#[test_case("loaded"; "loaded")]
fn hunt_end_lines_are_never_coloured(result: &str) {
    let event = hunt_end(result);
    let no_color = Renderer::new(OutputFacts {
        is_terminal: true,
        no_color: true,
        journald: false,
    });
    let plain_line = no_color.line(&event, &utc()).to_string();
    assert_eq!(
        plain().line(&event, &utc()).to_string(),
        plain_line,
        "{result}"
    );
    assert!(!plain_line.contains('\x1b'), "{result}");
    assert_eq!(
        colored().line(&event, &utc()).to_string(),
        plain_line,
        "{result}"
    );
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
    assert_eq!(
        line,
        "\x1b[31m01:14:07 failure        stored message\x1b[0m"
    );
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
        Renderer::new(OutputFacts {
            is_terminal,
            no_color,
            journald,
        })
    };
    let fail = failure_event();
    let pass = trial_end("pass");
    let zone = utc();
    let red = |renderer: Renderer| renderer.line(&fail, &zone).to_string();
    let body = "01:14:07 failure        stored message";

    assert_eq!(red(policy(false, false, false)), body);
    assert_eq!(
        red(policy(true, false, false)),
        format!("\x1b[31m{body}\x1b[0m")
    );
    assert_eq!(red(policy(true, true, false)), body);
    assert_eq!(
        red(policy(false, false, true)),
        format!("<3>\x1b[31m{body}\x1b[0m")
    );
    assert_eq!(red(policy(false, true, true)), format!("<3>{body}"));
    assert_eq!(red(policy(true, true, true)), format!("<3>{body}"));
    assert_eq!(
        red(policy(true, false, true)),
        format!("<3>\x1b[31m{body}\x1b[0m")
    );
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
    assert!(
        renderer
            .line(&dead_end, &zone)
            .to_string()
            .starts_with("<3>\x1b[1;31m")
    );
    assert!(
        renderer
            .line(&failure_event(), &zone)
            .to_string()
            .starts_with("<3>\x1b[31m")
    );
    assert!(
        renderer
            .text(Style::RedBold, "refused")
            .to_string()
            .starts_with("<3>")
    );
    assert!(
        !renderer
            .text(Style::Yellow, "warn")
            .to_string()
            .contains("<3>")
    );
    assert_eq!(
        journald_no_color()
            .text(Style::RedBold, "refused")
            .to_string(),
        "<3>refused"
    );
    assert_eq!(
        colored().text(Style::RedBold, "refused").to_string(),
        "\x1b[1;31mrefused\x1b[0m"
    );
}

#[test_case(&decision("step_deeper"); "step deeper")]
#[test_case(&decision("backoff"); "backoff")]
#[test_case(&trial_end("inconclusive"); "inconclusive trial end")]
fn journald_priority_skips_other_events(other: &Event) {
    let line = journald().line(other, &utc()).to_string();
    assert!(!line.contains("<3>"), "{line}");
}

#[test]
fn no_color_keeps_the_priority_prefix_but_drops_ansi() {
    let line = journald_no_color()
        .line(&failure_event(), &utc())
        .to_string();
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
    let plain_prefixed = plain()
        .prefixed_line(&trial_end("pass"), &utc(), "  evidence: ")
        .to_string();
    assert_eq!(
        plain_prefixed,
        "  evidence: 01:14:07 trial.end      stored message"
    );
    assert_eq!(
        journald()
            .prefixed_line(&failure_event(), &utc(), "p ")
            .to_string(),
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
    assert!(
        plain()
            .line(&failure_event(), &ahead)
            .to_string()
            .starts_with("00:14:07 ")
    );
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
    assert!(
        plain()
            .line(&stored, &utc())
            .to_string()
            .ends_with(" operator edited text")
    );
}

#[test]
fn the_stored_message_keeps_readable_unicode() {
    let unicode = event_with(
        "trial.end",
        "PASS 90s | Tctl max 71°C \u{fffd}",
        &json!({"trial": "1", "outcome": "pass", "duration_s": 1}),
    );
    assert!(
        plain()
            .line(&unicode, &utc())
            .to_string()
            .ends_with("PASS 90s | Tctl max 71°C \u{fffd}")
    );
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

const DIAGNOSTIC_DETAIL: &str = "日本語\x1b[2J\x1b]52;c;data\x07\r\n\u{9b}31m\u{202e}\u{2028}";

fn diagnostic() -> Event {
    event_with(
        "trial.progress",
        &format!("trial 0001 {DIAGNOSTIC_DETAIL}"),
        &json!({"trial": "0001", "detail": DIAGNOSTIC_DETAIL}),
    )
}

#[test_case(colored(); "colored")]
#[test_case(plain(); "plain")]
fn write_event_escapes_a_diagnostic(renderer: Renderer) {
    let escaped = r"trial 0001 日本語\x1b[2J\x1b]52;c;data\x07\r\n\u009b31m\u202e\u2028";
    let mut out = Vec::new();
    renderer
        .write_event(&mut out, &diagnostic(), &utc())
        .unwrap();
    let text = String::from_utf8(out).unwrap();
    assert_eq!(text, format!("01:14:07 trial.progress {escaped}\n"));
    assert!(!text.contains(['\x1b', '\r', '\u{9b}', '\u{202e}', '\u{2028}']));
}

#[test]
fn write_event_does_not_sanitize_the_evidence_of_a_diagnostic() {
    let diagnostic = diagnostic();
    let msg = format!("trial 0001 {DIAGNOSTIC_DETAIL}");
    let before = diagnostic.encode().unwrap();
    let mut out = Vec::new();
    colored()
        .write_event(&mut out, &diagnostic, &utc())
        .unwrap();
    plain().write_event(&mut out, &diagnostic, &utc()).unwrap();
    assert_eq!(diagnostic.msg, msg);
    let Payload::TrialProgress(progress) = &diagnostic.payload else {
        panic!("not a trial progress: {:?}", diagnostic.payload);
    };
    assert_eq!(progress.detail, DIAGNOSTIC_DETAIL);
    let after = diagnostic.encode().unwrap();
    assert_eq!(before, after);
    let decoded = Event::decode(&after).unwrap();
    assert_eq!(decoded.msg, msg);
    let Payload::TrialProgress(progress) = &decoded.payload else {
        panic!("not a trial progress: {:?}", decoded.payload);
    };
    assert_eq!(progress.detail, DIAGNOSTIC_DETAIL);
}

#[test]
fn write_event_appends_one_line_per_call() {
    let mut out = Vec::new();
    plain()
        .write_event(&mut out, &failure_event(), &utc())
        .unwrap();
    plain()
        .write_event(&mut out, &trial_end("pass"), &utc())
        .unwrap();
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

#[test_case(0; "fails at once")]
#[test_case(5; "fails inside the timestamp")]
#[test_case(30; "fails later in the line")]
fn write_event_propagates_the_sink_error(fail_after: usize) {
    let mut sink = FailingWriter { fail_after };
    let error = plain()
        .write_event(&mut sink, &failure_event(), &utc())
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
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
