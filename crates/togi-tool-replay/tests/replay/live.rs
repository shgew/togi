use std::ffi::OsString;
use std::fs::File;
use std::io::Write as _;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStringExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::{FdFlags, fcntl_setfd};
use rustix::process::{Pid, Signal, kill_process};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{Winsize, tcsetwinsize};
use tempfile::tempdir;
use togi_watchtest::{Scenario, install};

const BOUND: Duration = Duration::from_secs(30);

struct Terminal {
    master: OwnedFd,
    slave: PathBuf,
    seen: Vec<u8>,
}

impl Terminal {
    fn open() -> Self {
        let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
        fcntl_setfd(&master, FdFlags::CLOEXEC).unwrap();
        grantpt(&master).unwrap();
        unlockpt(&master).unwrap();
        let slave = PathBuf::from(OsString::from_vec(
            ptsname(&master, Vec::new()).unwrap().into_bytes(),
        ));
        Self {
            master,
            slave,
            seen: Vec::new(),
        }
    }

    /// A slave descriptor at 30 rows by 120 columns: macOS sets the window size only through the slave.
    fn slave(&self) -> File {
        let slave = File::options()
            .read(true)
            .write(true)
            .open(&self.slave)
            .unwrap();
        tcsetwinsize(
            &slave,
            Winsize {
                ws_row: 30,
                ws_col: 120,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )
        .unwrap();
        slave
    }

    /// Waits for the replay to exit while reading what it draws: a replay blocks while its terminal's output queue is
    /// full, and macOS discards what is still queued when the replay closes the slave with nobody reading.
    fn exit(&mut self, replay: &mut Replay) -> ExitStatus {
        let deadline = Instant::now() + BOUND;
        loop {
            if let Some(status) = replay.0.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "the replay never exited");
            let timeout = Timespec {
                tv_sec: 0,
                tv_nsec: 20_000_000,
            };
            let mut fds = [PollFd::new(&self.master, PollFlags::IN)];
            if poll(&mut fds, Some(&timeout)).unwrap() == 0 {
                continue;
            }
            let mut buf = [0_u8; 8192];
            if let Ok(read) = rustix::io::read(&self.master, &mut buf) {
                self.seen.extend_from_slice(&buf[..read]);
            }
        }
    }

    fn wait_for(&mut self, text: &str) {
        let deadline = Instant::now() + BOUND;
        while !String::from_utf8_lossy(&self.seen).contains(text) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "{text:?} never appeared in {:?}",
                String::from_utf8_lossy(&self.seen)
            );
            let timeout = Timespec {
                tv_sec: i64::try_from(remaining.as_secs()).unwrap(),
                tv_nsec: i64::from(remaining.subsec_nanos()),
            };
            let mut fds = [PollFd::new(&self.master, PollFlags::IN)];
            if poll(&mut fds, Some(&timeout)).unwrap() == 0 {
                continue;
            }
            let mut buf = [0_u8; 8192];
            let read = rustix::io::read(&self.master, &mut buf).unwrap();
            self.seen.extend_from_slice(&buf[..read]);
        }
    }
}

struct Replay(Child);

impl Drop for Replay {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn play(terminal: &Terminal, state: &str, speed: &str) -> Replay {
    Replay(
        Command::new(env!("CARGO_BIN_EXE_togi-tool-replay"))
            .args(["--state-dir", state, "--speed", speed])
            .env("TERM", "xterm")
            .env_remove("NO_COLOR")
            .env_remove("JOURNAL_STREAM")
            .stdin(Stdio::from(terminal.slave()))
            .stdout(Stdio::from(terminal.slave()))
            .spawn()
            .unwrap(),
    )
}

#[test]
fn real_process_playing_on_a_terminal_draws_the_dashboard_and_q_quits() {
    let dir = tempdir().unwrap();
    install(dir.path(), Scenario::Controls).unwrap();
    let mut terminal = Terminal::open();
    let mut replay = play(&terminal, dir.path().to_str().unwrap(), "1000000");
    terminal.wait_for("NIGHT");
    let mut master = File::from(terminal.master.try_clone().unwrap());
    master.write_all(b"q").unwrap();
    let status = terminal.exit(&mut replay);
    assert_eq!(status.code(), Some(0));
}

#[test]
fn real_process_playing_a_recording_with_an_unknown_kind_says_so_in_the_dashboard_and_q_quits() {
    let dir = tempdir().unwrap();
    install(dir.path(), Scenario::Between).unwrap();
    let mut recording = super::plant::recording(Scenario::Between);
    super::plant::plant(&mut recording);
    std::fs::write(dir.path().join("events.jsonl"), recording).unwrap();
    let mut terminal = Terminal::open();
    let mut replay = play(&terminal, dir.path().to_str().unwrap(), "1000000");
    terminal.wait_for("NIGHT");
    terminal.wait_for("skipped");
    let mut master = File::from(terminal.master.try_clone().unwrap());
    master.write_all(b"q").unwrap();
    assert_eq!(terminal.exit(&mut replay).code(), Some(0));
}

#[test]
fn real_process_sigterm_ends_the_dashboard_cleanly_and_restores_the_cursor() {
    let dir = tempdir().unwrap();
    install(dir.path(), Scenario::Controls).unwrap();
    let mut terminal = Terminal::open();
    let mut replay = play(&terminal, dir.path().to_str().unwrap(), "1");
    terminal.wait_for("togi");
    kill_process(Pid::from_child(&replay.0), Signal::TERM).unwrap();
    assert_eq!(terminal.exit(&mut replay).code(), Some(0));
    terminal.wait_for("\u{1b}[?25h");
}
