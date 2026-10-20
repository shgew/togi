//! What `status`, `events` and one-frame `watch` share: the `--config` refusal, the escaping of
//! what the journal holds, the journald styling of a refusal and the host lock they never take.

use togi_watchtest::Scenario;

use crate::support::{
    Journald, UNKNOWN_KIND, append_unknown_field, append_unknown_kind,
    insert_unknown_kind_before_last, session, text, togi, togi_in_journal,
};

const SCOPE: &str = "error: --config applies only to doctor, run and reset";

const WATCH_FRAME: [&str; 5] = ["watch", "--width", "120", "--height", "33"];

fn one_frame_watch<'a>(extra: &[&'a str]) -> Vec<&'a str> {
    let mut args = WATCH_FRAME.to_vec();
    args.extend(extra);
    args
}

#[test_case::test_case(&["status", "--config", "x"]; "status trailing")]
#[test_case::test_case(&["--config", "x", "status"]; "status leading")]
#[test_case::test_case(&["status", "--config", "x", "--help"]; "status trailing help")]
#[test_case::test_case(&["--config", "x", "status", "--help"]; "status leading help")]
#[test_case::test_case(&["events", "--config", "x"]; "events trailing")]
#[test_case::test_case(&["--config", "x", "events"]; "events leading")]
#[test_case::test_case(&["events", "--config", "x", "--help"]; "events trailing help")]
#[test_case::test_case(&["--config", "x", "events", "--help"]; "events leading help")]
#[test_case::test_case(&["watch", "--config", "x"]; "watch trailing")]
#[test_case::test_case(&["--config", "x", "watch"]; "watch leading")]
#[test_case::test_case(&["watch", "--config", "x", "--help"]; "watch trailing help")]
#[test_case::test_case(&["--config", "x", "watch", "--help"]; "watch leading help")]
fn config_is_a_usage_error_before_or_after_the_command_name(args: &[&str]) {
    let dir = session(Scenario::Search);
    let ran = togi(&dir, args);
    assert_eq!(ran.status.code(), Some(2), "{args:?}");
    assert_eq!(text(&ran.stdout), "", "{args:?}");
    assert_eq!(
        text(&ran.stderr).lines().next(),
        Some(SCOPE),
        "{args:?}: {}",
        text(&ran.stderr)
    );
}

#[test_case::test_case(&["--config", "x", "events", "--kind", "unknown"]; "events leading")]
#[test_case::test_case(&["events", "--kind", "unknown", "--config", "x"]; "events trailing")]
#[test_case::test_case(&["--config", "x", "watch", "--width", "0"]; "watch leading")]
#[test_case::test_case(&["watch", "--width", "0", "--config", "x"]; "watch trailing")]
fn the_config_error_comes_before_the_arguments_of_the_command(args: &[&str]) {
    let dir = session(Scenario::Search);
    let ran = togi(&dir, args);
    assert_eq!(ran.status.code(), Some(2), "{args:?}");
    assert_eq!(text(&ran.stdout), "", "{args:?}");
    assert_eq!(
        text(&ran.stderr).lines().next(),
        Some(SCOPE),
        "{args:?}: {}",
        text(&ran.stderr)
    );
}

/// The kind of the hostile event: ESC, a bidi override and a newline, as JSON escapes write them.
const HOSTILE_KIND: &str = "future.\u{1b}[2J\u{202e}\nforged";

/// A frame wide enough to show the whole report of what was skipped.
const WIDE_WATCH: [&str; 5] = ["watch", "--width", "400", "--height", "40"];

fn skipped_report(kind: &str, kind_line: usize, field_line: usize) -> String {
    format!(
        "skipped 1 event of a kind this build does not know ({kind} at line {kind_line}); ignored unknown fields: session.warning data.extra (line {field_line})"
    )
}

#[test]
fn the_read_only_commands_skip_an_unknown_kind_and_ignore_an_unknown_field_and_say_so() {
    let dir = session(Scenario::Search);
    let known = crate::support::lines(&dir).len();
    let kind_line = insert_unknown_kind_before_last(&dir, UNKNOWN_KIND);
    let field_line = append_unknown_field(&dir, "extra");
    let before = state_files(&dir);
    let report = skipped_report(UNKNOWN_KIND, kind_line, field_line);

    let ran = togi(&dir, &["status"]);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    assert!(!ran.stdout.is_empty());
    assert_eq!(text(&ran.stderr), format!("togi status: {report}\n"));

    let ran = togi(&dir, &["events"]);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    assert_eq!(text(&ran.stdout).lines().count(), known + 1);
    assert!(!text(&ran.stdout).contains(UNKNOWN_KIND));
    assert_eq!(text(&ran.stderr), format!("togi events: {report}\n"));

    let ran = togi(&dir, &["events", "--json"]);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    let kinds: Vec<String> = text(&ran.stdout)
        .lines()
        .map(|line| {
            let event: serde_json::Value = serde_json::from_str(line).unwrap();
            event["kind"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(kinds.len(), known + 1);
    assert!(!kinds.iter().any(|kind| kind == UNKNOWN_KIND));
    assert_eq!(text(&ran.stderr), format!("togi events: {report}\n"));

    let ran = togi(&dir, &WIDE_WATCH);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    assert_eq!(text(&ran.stderr), "");
    assert!(text(&ran.stdout).contains(&report), "{}", text(&ran.stdout));

    assert_eq!(state_files(&dir), before, "the journal is left as it was");
}

#[test_case::test_case("status", &["status"], false; "status")]
#[test_case::test_case("events", &["events"], false; "events")]
#[test_case::test_case("watch", &WIDE_WATCH, true; "watch")]
fn an_unknown_kind_that_is_hostile_is_escaped_in_what_the_read_only_commands_say(
    command: &str,
    args: &[&str],
    dashboard: bool,
) {
    let dir = session(Scenario::Search);
    let line = append_unknown_kind(&dir, HOSTILE_KIND);
    let ran = togi(&dir, args);
    assert_eq!(ran.status.code(), Some(0), "{command}");
    let said = format!(
        "skipped 1 event of a kind this build does not know ({HOSTILE_KIND:?} at line {line})"
    );
    if dashboard {
        assert_eq!(text(&ran.stderr), "", "{command}");
        assert!(text(&ran.stdout).contains(&said), "{command}");
    } else {
        assert_eq!(text(&ran.stderr), format!("togi {command}: {said}\n"));
    }
    for stream in [&ran.stdout, &ran.stderr] {
        assert!(
            !stream
                .iter()
                .any(|byte| byte.is_ascii_control() && *byte != b'\n'),
            "{command}"
        );
    }
    assert!(!ran.stderr.trim_ascii_end().contains(&b'\n'), "{command}");
}

#[test_case::test_case("status"; "status")]
#[test_case::test_case("events"; "events")]
#[test_case::test_case("watch"; "watch")]
fn an_unknown_kind_as_the_first_event_is_never_skipped(command: &str) {
    let dir = session(Scenario::Search);
    crate::support::rewrite_first_line(&dir, |event| {
        event["kind"] = UNKNOWN_KIND.into();
    });
    let before = crate::fixture::snapshot(dir.fs.as_ref(), dir.path());
    let args = if command == "watch" {
        one_frame_watch(&[])
    } else {
        vec![command]
    };
    let ran = togi(&dir, &args);
    assert_eq!(ran.status.code(), Some(1), "{command}");
    assert_eq!(text(&ran.stdout), "", "{command}");
    // The first line is not a header of this build, whatever its kind: the refusal does not say which build.
    crate::fixture::assert_refusal(command, text(&ran.stderr), dir.path());
    assert_eq!(
        crate::fixture::snapshot(dir.fs.as_ref(), dir.path()),
        before,
        "{command}"
    );
}

#[test_case::test_case("status"; "status")]
#[test_case::test_case("events"; "events")]
fn a_schema_refusal_is_red_bold_priority_3_when_stderr_is_the_system_journal(command: &str) {
    let dir = session(Scenario::Search);
    crate::support::rewrite_first_line(&dir, |event| {
        event["data"]["build"]["schema"] = 99.into();
    });
    let ran = togi_in_journal(&dir, &[command], Journald::Stderr);
    assert_eq!(ran.status.code(), Some(1), "{command}");
    assert_eq!(text(&ran.stdout), "", "{command}");
    let err = text(&ran.stderr);
    assert!(
        err.starts_with(&format!("<3>\x1b[1;31mtogi {command}: ")),
        "{command}: {err:?}"
    );
    assert!(err.ends_with("\x1b[0m\n"), "{command}: {err:?}");
    assert!(err.contains("schema 99"), "{command}: {err:?}");
}

#[test_case::test_case("status", &["status"]; "status")]
#[test_case::test_case("events", &["events"]; "events")]
#[test_case::test_case("watch", &WATCH_FRAME; "watch")]
fn the_read_only_commands_succeed_while_the_host_lock_is_held_and_never_take_it(
    command: &str,
    args: &[&str],
) {
    let session = session(Scenario::Search);
    let before = state_files(&session);
    let host = session.host();
    {
        let _held = host.locks.acquire(&host.lock).unwrap();
        let ran = host.run(session.path(), args);
        assert_eq!(
            ran.exit,
            togi_cli::Exit::Ok,
            "{command} while held: {}",
            ran.err
        );
        assert_eq!(ran.err, "", "{command}");
        assert!(!ran.out.is_empty(), "{command}");
        assert_eq!(state_files(&session), before, "{command} while held");
    }
    let ran = host.run(session.path(), args);
    assert_eq!(ran.exit, togi_cli::Exit::Ok, "{command}: {}", ran.err);
    assert_eq!(
        host.lock_acquisitions.get(),
        0,
        "{command} took the host lock"
    );
    assert_eq!(state_files(&session), before, "{command} without lock");
}

fn state_files(dir: &crate::support::Session) -> crate::fixture::Snapshot {
    crate::fixture::snapshot(dir.fs.as_ref(), dir.path())
}

#[test_case::test_case("frozen", togi_journal::DayAnswerKind::Frozen)]
#[test_case::test_case("graphics", togi_journal::DayAnswerKind::Graphics)]
#[test_case::test_case("power-off", togi_journal::DayAnswerKind::PowerOff)]
#[test_case::test_case("unsure", togi_journal::DayAnswerKind::Unsure)]
fn answering_uses_the_service_without_root_hardware_or_a_local_journal(
    argument: &str,
    expected: togi_journal::DayAnswerKind,
) {
    let mut host = crate::host::Host::new();
    host.root = false;
    host.linux = false;
    host.boot_fails = true;
    let ran = host.run(
        std::path::Path::new("/no-session"),
        &["day", "answer", "crashed-boot", argument],
    );
    assert_eq!(ran.exit, togi_cli::Exit::Ok, "{}", ran.err);
    assert_eq!(ran.out, "answer recorded for boot crashed-boot\n");
    assert_eq!(ran.err, "");
    assert_eq!(
        *host.day_answers.borrow(),
        [(
            togi_machine::BootId::from("crashed-boot".to_owned()),
            expected
        )]
    );
    assert_eq!(host.lock_acquisitions.get(), 0);
    assert_eq!(host.calls.get(), 0);
}
