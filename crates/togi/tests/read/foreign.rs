//! `status`, `events` and `watch` over the state directory of the Go build: the first line of its journal is not a
//! togi journal header, so each refuses with exit 1 and the cutover step, reads no further event, and changes no
//! file in the directory. The Go build's files are described in `fixture`.

use std::os::fd::OwnedFd;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rustix::termios::{Winsize, tcsetwinsize};
use togi_render::open_pty;

use crate::fixture::{self, Snapshot};
use crate::support::{Session, text, togi};
use crate::watch::{BOUND, HIDE_CURSOR, Helper, contains, read_more};

/// A state directory holding the Go build's files, and what it held.
fn go_state() -> (Session, Snapshot) {
    let dir = crate::support::Session::empty();
    fixture::install(dir.fs.as_ref(), dir.path());
    let before = fixture::snapshot(dir.fs.as_ref(), dir.path());
    (dir, before)
}

/// No lock file, no state file written by a Rust build, no archive move: the directory is what it was.
fn assert_untouched(state: &Session, before: &Snapshot) {
    use togi_journal::fs::Fs as _;
    assert_eq!(
        state.fs.kind(&state.path().join("lock")).unwrap(),
        None,
        "a refusal took the journal lock"
    );
    assert_eq!(
        state.fs.kind(&state.path().join("togi.lock")).unwrap(),
        None
    );
    assert_eq!(&fixture::snapshot(state.fs.as_ref(), state.path()), before);
}

#[test_case::test_case("status", &["status"]; "status")]
#[test_case::test_case("events", &["events"]; "events")]
#[test_case::test_case("events", &["events", "--kind", "session.start"]; "filtered events")]
#[test_case::test_case("watch", &["watch", "--width", "100", "--height", "30"]; "watch")]
fn each_read_command_refuses_the_go_journal_and_changes_no_file(command: &str, args: &[&str]) {
    let (dir, before) = go_state();
    let ran = togi(&dir, args);
    assert_eq!(
        ran.status.code(),
        Some(1),
        "{args:?}: {}",
        text(&ran.stderr)
    );
    assert_eq!(text(&ran.stdout), "", "{args:?}: no frame, no output");
    fixture::assert_refusal(command, text(&ran.stderr), dir.path());
    assert_untouched(&dir, &before);
}

#[test_case::test_case("status", &["status"]; "status")]
#[test_case::test_case("events", &["events"]; "events")]
#[test_case::test_case("events", &["events", "--kind", "session.start"]; "filtered events")]
#[test_case::test_case("watch", &["watch", "--width", "100", "--height", "30"]; "watch")]
fn the_refusal_comes_from_the_first_line_alone_whatever_follows_it(command: &str, args: &[&str]) {
    let dir = crate::support::Session::empty();
    fixture::install(dir.fs.as_ref(), dir.path());
    fixture::append_unreadable_line(dir.fs.as_ref(), dir.path());
    let before = fixture::snapshot(dir.fs.as_ref(), dir.path());
    let ran = togi(&dir, args);
    assert_eq!(ran.status.code(), Some(1), "{args:?}");
    fixture::assert_refusal(command, text(&ran.stderr), dir.path());
    let stderr = text(&ran.stderr);
    assert!(!stderr.contains("unknown event kind"), "{stderr}");
    assert!(!stderr.contains("line 13"), "{stderr}");
    assert_untouched(&dir, &before);
}

#[test_case::test_case("status", &["status"]; "status")]
#[test_case::test_case("events", &["events"]; "events")]
#[test_case::test_case("events", &["events", "--kind", "session.start"]; "filtered events")]
#[test_case::test_case("watch", &["watch", "--width", "100", "--height", "30"]; "watch")]
fn a_first_line_without_its_newline_is_refused_all_the_same(command: &str, args: &[&str]) {
    let dir = crate::support::Session::empty();
    fixture::install(dir.fs.as_ref(), dir.path());
    fixture::keep_only_the_unterminated_first_line(dir.fs.as_ref(), dir.path());
    let before = fixture::snapshot(dir.fs.as_ref(), dir.path());
    let ran = togi(&dir, args);
    assert_eq!(
        ran.status.code(),
        Some(1),
        "{args:?}: {}",
        text(&ran.stderr)
    );
    assert_eq!(text(&ran.stdout), "", "{args:?}: no frame, no output");
    fixture::assert_refusal(command, text(&ran.stderr), dir.path());
    let stderr = text(&ran.stderr);
    assert!(!stderr.contains("torn bytes"), "{stderr}");
    assert_untouched(&dir, &before);
}

#[test_case::test_case("status", &["status"], false; "status unterminated")]
#[test_case::test_case("events", &["events"], false; "events unterminated")]
#[test_case::test_case("events", &["events", "--kind", "session.start"], false; "filtered events unterminated")]
#[test_case::test_case("watch", &["watch", "--width", "100", "--height", "30"], false; "watch unterminated")]
#[test_case::test_case("status", &["status"], true; "status terminated")]
#[test_case::test_case("events", &["events"], true; "events terminated")]
#[test_case::test_case("events", &["events", "--kind", "session.start"], true; "filtered events terminated")]
#[test_case::test_case("watch", &["watch", "--width", "100", "--height", "30"], true; "watch terminated")]
fn a_truncated_header_of_this_build_is_refused_in_the_same_words(
    command: &str,
    args: &[&str],
    newline: bool,
) {
    let dir = crate::support::Session::empty();
    fixture::keep_only_a_truncated_journal_header(dir.fs.as_ref(), dir.path(), newline);
    let before = fixture::snapshot(dir.fs.as_ref(), dir.path());
    let ran = togi(&dir, args);
    let stderr = text(&ran.stderr);
    assert_eq!(
        ran.status.code(),
        Some(1),
        "{args:?}, newline {newline}: {stderr}"
    );
    assert_eq!(text(&ran.stdout), "", "{args:?}: no frame, no output");
    fixture::assert_refusal(command, stderr, dir.path());
    assert!(!stderr.contains("torn bytes"), "{stderr}");
    assert_untouched(&dir, &before);
}

#[test]
fn real_process_live_watch_refuses_before_it_takes_the_terminal() {
    let dir = tempfile::tempdir().unwrap();
    fixture::install_real_fs(dir.path());
    let before = fixture::snapshot_real_fs(dir.path());
    let (master, slave) = open_pty().unwrap();
    tcsetwinsize(
        &slave,
        Winsize {
            ws_row: 30,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .unwrap();
    #[expect(
        clippy::disallowed_methods,
        reason = "the real_process test checks refusal on a PTY"
    )]
    let child = Command::new(env!("CARGO_BIN_EXE_togi"))
        .arg("--state-dir")
        .arg(dir.path())
        .arg("watch")
        .env_remove("NO_COLOR")
        .env_remove("JOURNAL_STREAM")
        .env("TERM", "xterm")
        .stdin(Stdio::from(slave.try_clone().unwrap()))
        .stdout(Stdio::from(slave.try_clone().unwrap()))
        .stderr(Stdio::from(slave.try_clone().unwrap()))
        .spawn()
        .unwrap();
    let mut helper = Helper(child);

    let mut seen = Vec::new();
    let deadline = Instant::now() + BOUND;
    let status = loop {
        if let Some(status) = helper.0.try_wait().unwrap() {
            break status;
        }
        read_more(
            &master,
            &mut seen,
            Instant::now() + Duration::from_millis(50),
        );
        assert!(
            Instant::now() < deadline,
            "watch did not refuse within {BOUND:?}; the terminal received {:?}",
            String::from_utf8_lossy(&seen)
        );
    };
    drain(&master, &mut seen);
    assert_eq!(
        status.code(),
        Some(1),
        "{:?}",
        String::from_utf8_lossy(&seen)
    );
    assert!(
        !contains(&seen, HIDE_CURSOR),
        "the dashboard took the terminal: {:?}",
        String::from_utf8_lossy(&seen)
    );
    let shown = String::from_utf8_lossy(&seen);
    fixture::assert_refusal("watch", &shown, dir.path());
    assert!(
        !dir.path().join("lock").exists(),
        "a refusal took the journal lock"
    );
    assert!(!dir.path().join("togi.lock").exists());
    assert_eq!(fixture::snapshot_real_fs(dir.path()), before);
}

/// Reads what the child left on the terminal after it ended.
fn drain(master: &OwnedFd, seen: &mut Vec<u8>) {
    while read_more(master, seen, Instant::now() + Duration::from_millis(200)) {}
}
