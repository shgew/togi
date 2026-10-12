use std::ffi::OsString;
use std::fs::File;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStringExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::{FdFlags, fcntl_setfd};
use rustix::process::{Pid, Signal, kill_process};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{LocalModes, Winsize, tcgetattr, tcsetwinsize};
use togi_watchtest::Scenario;

use crate::support::{
    UNKNOWN_KIND, append_between_trial_mce, append_event, append_unknown_kind,
    assert_unknown_kind_refusal, journal, lines, rewrite_first_line, session, text, togi,
};

pub(crate) const BOUND: Duration = Duration::from_secs(30);
pub(crate) const HIDE_CURSOR: &[u8] = b"\x1b[?25l";
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";

#[test]
fn a_missing_journal_prints_no_session_yet_and_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let ran = togi(dir.path(), &["watch", "--width", "80", "--height", "20"]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(text(&ran.stderr), "");
    assert!(
        text(&ran.stdout).to_lowercase().contains("no session yet"),
        "{}",
        text(&ran.stdout)
    );
}

#[test]
fn without_a_terminal_one_frame_of_the_requested_size_is_printed() {
    let dir = session(Scenario::Checking);
    let ran = togi(dir.path(), &["watch", "--width", "100", "--height", "30"]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(text(&ran.stderr), "");
    let shown = text(&ran.stdout);
    assert_eq!(shown.lines().count(), 29);
    assert!(shown.lines().all(|line| line.chars().count() <= 99));
    assert!(!shown.contains('\u{1b}'), "one frame carries no styling");
}

#[test]
fn an_unreadable_journal_prints_the_problem_frame_and_exits_one() {
    // A header of this build, then a line that is not an event: unreadable, not another build's journal.
    let dir = session(Scenario::Search);
    let header = lines(dir.path()).swap_remove(0);
    std::fs::write(journal(dir.path()), format!("{header}\nnot json\n")).unwrap();
    let ran = togi(dir.path(), &["watch", "--width", "100", "--height", "30"]);
    assert_eq!(ran.status.code(), Some(1));
    assert!(!text(&ran.stdout).trim().is_empty(), "the frame is printed");
    assert!(
        text(&ran.stderr).starts_with("togi watch:"),
        "{}",
        text(&ran.stderr)
    );
}

#[test]
fn an_incompatible_journal_prints_the_refusal_frame_and_exits_one() {
    let dir = session(Scenario::Search);
    rewrite_first_line(dir.path(), |event| {
        event["data"]["build"]["schema"] = 99.into();
    });
    let ran = togi(dir.path(), &["watch", "--width", "240", "--height", "67"]);
    assert_eq!(ran.status.code(), Some(1));
    let refusal = format!(
        "the journal's schema is 99; this build's is {}",
        togi_journal::SCHEMA
    );
    assert!(
        text(&ran.stdout).contains(&refusal),
        "{}",
        text(&ran.stdout)
    );
    assert_eq!(text(&ran.stderr), format!("togi watch: {refusal}\n"));
}

#[test]
fn journal_controls_are_escaped_in_the_frame_and_the_diagnostic() {
    let dir = session(Scenario::Search);
    append_event(
        dir.path(),
        "session.warning",
        "warn \u{1b}[2Jsneaky",
        &serde_json::json!({"operation": "x\u{1b}[31m", "error": "e\u{7}"}),
    );
    let ran = togi(dir.path(), &["watch", "--width", "240", "--height", "67"]);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    assert!(!text(&ran.stdout).contains('\u{1b}'));
    assert!(
        text(&ran.stdout).contains("x\\x1b[31m: e\\x07"),
        "{}",
        text(&ran.stdout)
    );
    assert_eq!(text(&ran.stderr), "");

    let broken = session(Scenario::Search);
    append_event(
        broken.path(),
        "session.warning",
        "x",
        &serde_json::json!({"operation": "x", "bad\u{1b}[2J": 1}),
    );
    let ran = togi(
        broken.path(),
        &["watch", "--width", "240", "--height", "67"],
    );
    assert_eq!(ran.status.code(), Some(1));
    assert!(!text(&ran.stdout).contains('\u{1b}'));
    assert!(!text(&ran.stderr).contains('\u{1b}'));
    assert!(
        text(&ran.stdout).contains("bad\\x1b[2J"),
        "{}",
        text(&ran.stdout)
    );
    assert!(
        text(&ran.stderr).contains("bad\\x1b[2J"),
        "{}",
        text(&ran.stderr)
    );
}

#[test]
fn a_between_trial_mce_shows_in_the_frame() {
    let dir = session(Scenario::Search);
    append_between_trial_mce(dir.path());
    let ran = togi(dir.path(), &["watch", "--width", "240", "--height", "67"]);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    assert!(
        text(&ran.stdout).contains("between trials"),
        "{}",
        text(&ran.stdout)
    );
    assert!(!text(&ran.stdout).contains('\u{1b}'));
}

#[test]
fn the_frame_size_must_be_at_least_one() {
    let dir = tempfile::tempdir().unwrap();
    for args in [
        ["watch", "--width", "0"],
        ["watch", "--height", "0"],
        ["watch", "--width", "1001"],
        ["watch", "--height", "1001"],
    ] {
        let ran = togi(dir.path(), &args);
        assert_eq!(ran.status.code(), Some(2), "{args:?}");
        assert_eq!(text(&ran.stdout), "");
    }
}

#[test]
fn watch_help() {
    let ran = togi(std::path::Path::new("/nonexistent"), &["watch", "--help"]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(text(&ran.stderr), "");
    insta::assert_snapshot!(text(&ran.stdout));
}

pub(crate) struct Helper(pub(crate) Child);

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub(crate) fn read_more(master: &OwnedFd, seen: &mut Vec<u8>, deadline: Instant) -> bool {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return false;
    }
    let timeout = Timespec {
        tv_sec: i64::try_from(remaining.as_secs()).unwrap(),
        tv_nsec: i64::from(remaining.subsec_nanos()),
    };
    let mut fds = [PollFd::new(master, PollFlags::IN)];
    if poll(&mut fds, Some(&timeout)).unwrap() == 0 {
        return false;
    }
    let mut buf = [0_u8; 8192];
    match rustix::io::read(master, &mut buf) {
        Ok(0) | Err(rustix::io::Errno::IO) => false,
        Ok(read) => {
            seen.extend_from_slice(&buf[..read]);
            true
        }
        Err(errno) => panic!("reading the terminal failed: {errno}"),
    }
}

pub(crate) fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn sigint_ends_a_live_watch_with_the_terminal_restored() {
    a_signal_ends_a_live_watch_with_the_terminal_restored(Signal::INT);
}

#[test]
fn sigterm_ends_a_live_watch_with_the_terminal_restored() {
    a_signal_ends_a_live_watch_with_the_terminal_restored(Signal::TERM);
}

#[test]
fn sighup_ends_a_live_watch_with_the_terminal_restored() {
    a_signal_ends_a_live_watch_with_the_terminal_restored(Signal::HUP);
}

fn a_signal_ends_a_live_watch_with_the_terminal_restored(signal: Signal) {
    let dir = session(Scenario::Checking);
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    fcntl_setfd(&master, FdFlags::CLOEXEC).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let slave_path = PathBuf::from(OsString::from_vec(
        ptsname(&master, Vec::new()).unwrap().into_bytes(),
    ));
    tcsetwinsize(
        &master,
        Winsize {
            ws_row: 30,
            ws_col: 100,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .unwrap();
    let slave = File::options()
        .read(true)
        .write(true)
        .open(&slave_path)
        .unwrap();
    let before = tcgetattr(&slave).unwrap().local_modes;
    assert!(before.contains(LocalModes::ICANON | LocalModes::ECHO));

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
    let pid = Pid::from_raw(i32::try_from(child.id()).unwrap()).unwrap();
    let mut helper = Helper(child);

    let mut seen = Vec::new();
    let deadline = Instant::now() + BOUND;
    while !contains(&seen, HIDE_CURSOR) {
        assert!(
            read_more(&master, &mut seen, deadline),
            "the dashboard never hid the cursor; the terminal received {:?}",
            String::from_utf8_lossy(&seen)
        );
    }
    let shown_at = seen.len();

    kill_process(pid, signal).unwrap();
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
            "watch did not end within {BOUND:?} of {signal:?}"
        );
    };
    while !contains(&seen[shown_at..], SHOW_CURSOR) {
        assert!(
            read_more(&master, &mut seen, deadline),
            "the cursor is not shown again after {signal:?}; the terminal received {:?}",
            String::from_utf8_lossy(&seen)
        );
    }
    assert_eq!(status.code(), Some(0), "{signal:?} is a normal end");
    assert_eq!(tcgetattr(&slave).unwrap().local_modes, before);
}

#[test]
fn an_event_of_an_unknown_kind_is_refused_in_read_only_words_by_watch() {
    let dir = session(Scenario::Search);
    let line = append_unknown_kind(dir.path(), UNKNOWN_KIND);
    let ran = togi(dir.path(), &["watch", "--width", "100", "--height", "30"]);
    assert_unknown_kind_refusal("watch", &ran, line);
    assert_eq!(text(&ran.stdout).is_empty(), "watch" != "watch");
}
