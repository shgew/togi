use std::ffi::OsString;
use std::fs::File;
use std::io::Write as _;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStringExt as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
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
        let master =
            openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC).unwrap();
        grantpt(&master).unwrap();
        unlockpt(&master).unwrap();
        let slave = PathBuf::from(OsString::from_vec(
            ptsname(&master, Vec::new()).unwrap().into_bytes(),
        ));
        tcsetwinsize(
            &master,
            Winsize {
                ws_row: 30,
                ws_col: 120,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )
        .unwrap();
        Self {
            master,
            slave,
            seen: Vec::new(),
        }
    }

    fn slave(&self) -> File {
        File::options()
            .read(true)
            .write(true)
            .open(&self.slave)
            .unwrap()
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
fn playing_on_a_terminal_draws_the_dashboard_and_q_quits() {
    let dir = tempdir().unwrap();
    install(dir.path(), Scenario::Checking).unwrap();
    let mut terminal = Terminal::open();
    let mut replay = play(&terminal, dir.path().to_str().unwrap(), "1000000");
    terminal.wait_for("CYCLE");
    let mut master = File::from(terminal.master.try_clone().unwrap());
    master.write_all(b"q").unwrap();
    let status = replay.0.wait().unwrap();
    assert_eq!(status.code(), Some(0));
}

#[test]
fn sigterm_ends_the_dashboard_cleanly_and_restores_the_cursor() {
    let dir = tempdir().unwrap();
    install(dir.path(), Scenario::Checking).unwrap();
    let mut terminal = Terminal::open();
    let mut replay = play(&terminal, dir.path().to_str().unwrap(), "1");
    terminal.wait_for("togi");
    kill_process(Pid::from_child(&replay.0), Signal::TERM).unwrap();
    assert_eq!(replay.0.wait().unwrap().code(), Some(0));
    terminal.wait_for("\u{1b}[?25h");
}
