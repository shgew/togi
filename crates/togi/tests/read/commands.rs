//! What `status`, `events` and one-frame `watch` share: the `--config` refusal, the escaping of
//! what the journal holds, the journald styling of a refusal and the host lock they never take.

use togi_hostlock::{HostLock, Policy};
use togi_watchtest::Scenario;

use crate::host::Host;
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

#[test]
fn config_is_a_usage_error_before_or_after_the_command_name() {
    let dir = session(Scenario::Search);
    for command in ["status", "events", "watch"] {
        for help in [&[][..], &["--help"]] {
            let mut written = vec![command, "--config", "x"];
            written.extend(help);
            let mut leading = vec!["--config", "x", command];
            leading.extend(help);
            for args in [written, leading] {
                let ran = togi(dir.path(), &args);
                assert_eq!(ran.status.code(), Some(2), "{args:?}");
                assert_eq!(text(&ran.stdout), "", "{args:?}");
                assert_eq!(
                    text(&ran.stderr).lines().next(),
                    Some(SCOPE),
                    "{args:?}: {}",
                    text(&ran.stderr)
                );
            }
        }
    }
}

#[test]
fn the_config_error_comes_before_the_arguments_of_the_command() {
    let dir = session(Scenario::Search);
    for args in [
        &["--config", "x", "events", "--kind", "unknown"][..],
        &["events", "--kind", "unknown", "--config", "x"],
        &["--config", "x", "watch", "--width", "0"],
        &["watch", "--width", "0", "--config", "x"],
    ] {
        let ran = togi(dir.path(), args);
        assert_eq!(ran.status.code(), Some(2), "{args:?}");
        assert_eq!(text(&ran.stdout), "", "{args:?}");
        assert_eq!(
            text(&ran.stderr).lines().next(),
            Some(SCOPE),
            "{args:?}: {}",
            text(&ran.stderr)
        );
    }
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
    let known = crate::support::lines(dir.path()).len();
    let kind_line = insert_unknown_kind_before_last(dir.path(), UNKNOWN_KIND);
    let field_line = append_unknown_field(dir.path(), "extra");
    let before = state_files(dir.path());
    let report = skipped_report(UNKNOWN_KIND, kind_line, field_line);

    let ran = togi(dir.path(), &["status"]);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    assert!(!ran.stdout.is_empty());
    assert_eq!(text(&ran.stderr), format!("togi status: {report}\n"));

    let ran = togi(dir.path(), &["events"]);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    assert_eq!(text(&ran.stdout).lines().count(), known + 1);
    assert!(!text(&ran.stdout).contains(UNKNOWN_KIND));
    assert_eq!(text(&ran.stderr), format!("togi events: {report}\n"));

    let ran = togi(dir.path(), &["events", "--json"]);
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

    let ran = togi(dir.path(), &WIDE_WATCH);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    assert_eq!(text(&ran.stderr), "");
    assert!(text(&ran.stdout).contains(&report), "{}", text(&ran.stdout));

    assert_eq!(
        state_files(dir.path()),
        before,
        "the journal is left as it was"
    );
}

#[test]
fn an_unknown_kind_that_is_hostile_is_escaped_in_what_the_read_only_commands_say() {
    let dir = session(Scenario::Search);
    let line = append_unknown_kind(dir.path(), HOSTILE_KIND);
    let commands: [(&str, Vec<&str>); 3] = [
        ("status", vec!["status"]),
        ("events", vec!["events"]),
        ("watch", WIDE_WATCH.to_vec()),
    ];
    for (command, args) in commands {
        let ran = togi(dir.path(), &args);
        assert_eq!(ran.status.code(), Some(0), "{command}");
        let said = format!(
            "skipped 1 event of a kind this build does not know ({HOSTILE_KIND:?} at line {line})"
        );
        if command == "watch" {
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
}

#[test]
fn an_unknown_kind_as_the_first_event_is_never_skipped() {
    let dir = session(Scenario::Search);
    crate::support::rewrite_first_line(dir.path(), |event| {
        event["kind"] = UNKNOWN_KIND.into();
    });
    for command in ["status", "events", "watch"] {
        let args = if command == "watch" {
            one_frame_watch(&[])
        } else {
            vec![command]
        };
        let ran = togi(dir.path(), &args);
        assert_eq!(ran.status.code(), Some(1), "{command}");
        let said = text(&ran.stderr);
        assert!(
            said.contains("session.start") || said.contains(UNKNOWN_KIND),
            "{command}: {said}"
        );
    }
}

#[test]
fn a_schema_refusal_is_red_bold_priority_3_when_stderr_is_the_system_journal() {
    let dir = session(Scenario::Search);
    crate::support::rewrite_first_line(dir.path(), |event| {
        event["data"]["build"]["schema"] = 99.into();
    });
    for command in ["status", "events"] {
        let ran = togi_in_journal(dir.path(), &[command], Journald::Stderr);
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
}

#[test]
fn the_read_only_commands_succeed_while_another_process_holds_the_host_lock_and_never_take_it() {
    let session = crate::host::Session::of(Scenario::Search);
    let before = state_files(session.path());
    let commands: [(&str, Vec<&str>); 3] = [
        ("status", vec!["status"]),
        ("events", vec!["events"]),
        ("watch", one_frame_watch(&[])),
    ];
    for (command, args) in commands {
        let host = Host::new();
        {
            let _held = HostLock::acquire(&host.lock, Policy::Private).unwrap();
            let ran = host.run(session.path(), &args);
            assert_eq!(
                ran.exit,
                togi_cli::Exit::Ok,
                "{command} while held: {}",
                ran.err
            );
            assert_eq!(ran.err, "", "{command}");
            assert!(!ran.out.is_empty(), "{command}");
            assert_eq!(state_files(session.path()), before, "{command} while held");
        }
        let _ = std::fs::remove_file(&host.lock);
        let ran = host.run(session.path(), &args);
        assert_eq!(ran.exit, togi_cli::Exit::Ok, "{command}: {}", ran.err);
        assert!(!host.lock.exists(), "{command} created the host lock");
        assert_eq!(
            state_files(session.path()),
            before,
            "{command} without lock"
        );
    }
}

fn state_files(dir: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    crate::host::files(dir)
        .into_iter()
        .filter_map(|name| {
            let path = dir.join(&name);
            path.is_file().then(|| (name, std::fs::read(path).unwrap()))
        })
        .collect()
}
