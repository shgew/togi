use std::os::fd::OwnedFd;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::process::{Pid, Signal, kill_process};
use rustix::termios::{LocalModes, Winsize, tcgetattr, tcsetwinsize};
use togi_render::open_pty;
use togi_watchtest::Scenario;

use crate::support::{
    append_between_trial_mce, append_event, lines, rewrite_first_line, session, text, togi,
};

pub(crate) const BOUND: Duration = Duration::from_secs(30);
pub(crate) const HIDE_CURSOR: &[u8] = b"\x1b[?25l";
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";

#[test]
fn a_missing_journal_prints_no_session_yet_and_exits_zero() {
    let dir = crate::support::Session::empty();
    let ran = togi(&dir, &["watch", "--width", "80", "--height", "20"]);
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
    let ran = togi(&dir, &["watch", "--width", "100", "--height", "30"]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(text(&ran.stderr), "");
    let shown = text(&ran.stdout);
    assert_eq!(shown.lines().count(), 29);
    assert!(shown.lines().all(|line| line.chars().count() <= 99));
    assert!(!shown.contains('\u{1b}'), "one frame carries no styling");
}

#[test]
fn without_a_terminal_the_default_frame_is_240_by_67() {
    let dir = session(Scenario::Checking);
    let default = togi(&dir, &["watch"]);
    let explicit = togi(&dir, &["watch", "--width", "240", "--height", "67"]);
    assert_eq!(default.status.code(), Some(0));
    assert_eq!(text(&default.stderr), "");
    assert_eq!(default.stdout, explicit.stdout);
    assert_eq!(text(&default.stdout).lines().count(), 66);
}

#[test]
fn an_unreadable_journal_prints_the_problem_frame_and_exits_one() {
    // A header of this build, then a line that is not an event: unreadable, not another build's journal.
    let dir = session(Scenario::Search);
    let header = lines(&dir).swap_remove(0);
    crate::support::write_journal(&dir, format!("{header}\nnot json\n"));
    let ran = togi(&dir, &["watch", "--width", "100", "--height", "30"]);
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
    rewrite_first_line(&dir, |event| {
        event["data"]["build"]["schema"] = 99.into();
    });
    let ran = togi(&dir, &["watch", "--width", "240", "--height", "67"]);
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
        &dir,
        "session.warning",
        "warn \u{1b}[2Jsneaky",
        &serde_json::json!({"operation": "x\u{1b}[31m", "error": "e\u{7}"}),
    );
    let ran = togi(&dir, &["watch", "--width", "240", "--height", "67"]);
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
        &broken,
        "failure",
        "x",
        &serde_json::json!({"signal": "bad\u{1b}[2J", "attribution": "attributed"}),
    );
    let ran = togi(&broken, &["watch", "--width", "240", "--height", "67"]);
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
    append_between_trial_mce(&dir);
    let ran = togi(&dir, &["watch", "--width", "240", "--height", "67"]);
    assert_eq!(ran.status.code(), Some(0), "{}", text(&ran.stderr));
    assert!(
        text(&ran.stdout).contains("between trials"),
        "{}",
        text(&ran.stdout)
    );
    assert!(!text(&ran.stdout).contains('\u{1b}'));
}

#[test_case::test_case(["watch", "--width", "0"]; "zero width")]
#[test_case::test_case(["watch", "--height", "0"]; "zero height")]
#[test_case::test_case(["watch", "--width", "1001"]; "excess width")]
#[test_case::test_case(["watch", "--height", "1001"]; "excess height")]
fn the_frame_size_must_be_at_least_one(args: [&str; 3]) {
    let dir = crate::support::Session::empty();
    let ran = togi(&dir, &args);
    assert_eq!(ran.status.code(), Some(2), "{args:?}");
    assert_eq!(text(&ran.stdout), "");
}

#[test]
fn watch_help() {
    let ran = togi(&crate::support::Session::empty(), &["watch", "--help"]);
    assert_eq!(ran.status.code(), Some(0));
    assert_eq!(text(&ran.stderr), "");
    crate::support::snapshot("watch__watch_help", text(&ran.stdout));
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

#[derive(Clone, Copy, Debug)]
enum Ending {
    Signal(Signal),
    /// Bytes typed on the terminal.
    Keys(&'static [u8]),
}

/// The four quit keys, as raw mode delivers them: `q`, `Q`, Ctrl-C and Ctrl-D.
const QUIT_KEYS: &[u8] = b"qQ\x03\x04";

#[test]
fn real_process_sigint_ends_a_live_watch_with_the_terminal_restored() {
    a_live_watch_ends_with_the_terminal_restored(false, Ending::Signal(Signal::INT));
}

#[test]
fn real_process_sigterm_ends_a_live_watch_with_the_terminal_restored() {
    a_live_watch_ends_with_the_terminal_restored(false, Ending::Signal(Signal::TERM));
}

#[test]
fn real_process_sighup_ends_a_live_watch_with_the_terminal_restored() {
    a_live_watch_ends_with_the_terminal_restored(false, Ending::Signal(Signal::HUP));
}

#[test]
fn real_process_q_ends_a_standalone_live_watch_with_the_terminal_restored() {
    a_live_watch_ends_with_the_terminal_restored(false, Ending::Keys(b"q"));
}

#[test_case::test_case(b"Q"; "uppercase q")]
#[test_case::test_case(b"\x03"; "control c")]
#[test_case::test_case(b"\x04"; "control d")]
fn real_process_every_quit_key_ends_a_standalone_live_watch(key: &'static [u8]) {
    a_live_watch_ends_with_the_terminal_restored(false, Ending::Keys(key));
}

#[test]
fn real_process_a_tuning_boot_watch_ignores_the_quit_keys_and_still_stops_on_sigterm() {
    a_live_watch_ends_with_the_terminal_restored(true, Ending::Signal(Signal::TERM));
}

fn a_live_watch_ends_with_the_terminal_restored(tuning_boot: bool, ending: Ending) {
    let dir = tempfile::tempdir().unwrap();
    togi_watchtest::install(dir.path(), Scenario::Checking).unwrap();
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
    let before = tcgetattr(&slave).unwrap().local_modes;
    assert!(before.contains(LocalModes::ICANON | LocalModes::ECHO));

    #[expect(
        clippy::disallowed_methods,
        reason = "the real_process test checks watch terminal restoration"
    )]
    let child = Command::new(env!("CARGO_BIN_EXE_togi"))
        .arg("--state-dir")
        .arg(dir.path())
        .arg("watch")
        .args(tuning_boot.then_some("--tuning-boot"))
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

    if tuning_boot {
        rustix::io::write(&master, QUIT_KEYS).unwrap();
        let quiet = Instant::now() + Duration::from_secs(1);
        while Instant::now() < quiet {
            read_more(&master, &mut seen, quiet);
            assert!(
                helper.0.try_wait().unwrap().is_none(),
                "the tuning boot dashboard ended on a quit key; the terminal received {:?}",
                String::from_utf8_lossy(&seen)
            );
        }
    }
    match ending {
        Ending::Signal(signal) => kill_process(pid, signal).unwrap(),
        Ending::Keys(keys) => {
            rustix::io::write(&master, keys).unwrap();
        }
    }
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
            "watch did not end within {BOUND:?} of {ending:?}"
        );
    };
    while !contains(&seen[shown_at..], SHOW_CURSOR) {
        assert!(
            read_more(&master, &mut seen, deadline),
            "the cursor is not shown again after {ending:?}; the terminal received {:?}",
            String::from_utf8_lossy(&seen)
        );
    }
    assert_eq!(status.code(), Some(0), "{ending:?} is a normal end");
    assert_eq!(tcgetattr(&slave).unwrap().local_modes, before);
}
