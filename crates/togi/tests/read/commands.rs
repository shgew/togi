//! What `status`, `events` and one-frame `watch` share: the `--config` refusal, the escaping of
//! what the journal holds, the journald styling of a refusal and the host lock they never take.

use togi_hostlock::{HostLock, Policy};
use togi_watchtest::Scenario;

use crate::host::Host;
use crate::support::{Journald, append_unknown_kind, session, text, togi, togi_in_journal};

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

#[test]
fn a_hostile_unknown_kind_is_escaped_in_the_spec_form_on_stderr() {
    let dir = session(Scenario::Search);
    let line = append_unknown_kind(dir.path(), HOSTILE_KIND);
    let commands: [(&str, Vec<&str>); 3] = [
        ("status", vec!["status"]),
        ("events", vec!["events"]),
        ("watch", one_frame_watch(&[])),
    ];
    for (command, args) in commands {
        let ran = togi(dir.path(), &args);
        assert_eq!(ran.status.code(), Some(1), "{command}");
        let expected = format!(
            "togi {command}: line {line}: unknown event kind \"future.\\x1b[2J\\u202e\\nforged\"; this build does not know it\n"
        );
        assert_eq!(text(&ran.stderr), expected, "{command}");
        assert!(
            !ran.stderr
                .iter()
                .any(|byte| byte.is_ascii_control() && *byte != b'\n'),
            "{command}"
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
        }
        let _ = std::fs::remove_file(&host.lock);
        let ran = host.run(session.path(), &args);
        assert_eq!(ran.exit, togi_cli::Exit::Ok, "{command}: {}", ran.err);
        assert!(!host.lock.exists(), "{command} created the host lock");
    }
}
