#![cfg(target_os = "linux")]
#![expect(clippy::unwrap_used, reason = "helpers panic on a broken fixture")]

//! The dashboard in a real process on a pseudo-terminal: real signals, a real keyboard and real inotify.
//!
//! The helper process is this test binary run again on one test, `real_process_helper`, which does nothing
//! unless it is started by another test. The test that started it kills it on every path, and every wait is
//! bounded.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::Write as _;
use std::os::fd::{AsFd as _, OwnedFd};
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use ratatui::buffer::Buffer;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::process::{Pid, Signal, kill_process};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{LocalModes, Winsize, tcgetattr, tcsetwinsize};
use std::os::unix::process::ExitStatusExt as _;
use tempfile::TempDir;
use togi_journal::{Build, Event, Payload, SCHEMA, Seq, SessionStart, SessionWarning, Stamp};
use togi_machine::{BootId, CcdId, CoreId, CoreInfo, CpuId};
use togi_watch::screen::{Drawn, Hold, Journal, Screen, View};
use togi_watch::terminal::{Stop, run, run_after};

const STATE_DIR: &str = "TOGI_WATCH_TEST_STATE_DIR";
const MODE: &str = "TOGI_WATCH_TEST_MODE";
const FROZEN: &str = "TOGI_WATCH_TEST_FROZEN";
const RESTORED: &[u8] = b"SIGNALS-RESTORED";
const BOUND: Duration = Duration::from_secs(30);
const SHOW_CURSOR: &[u8] = b"\x1b[?25h";
const HIDE_CURSOR: &[u8] = b"\x1b[?25l";
const PALETTE_SET: &[u8] = b"\x1b]P0000000";
const PALETTE_RESET: &[u8] = b"\x1b]R";

const SIGNALS: [Signal; 4] = [Signal::INT, Signal::TERM, Signal::HUP, Signal::WINCH];

/// What a signal does, as far as the kernel tells: it is ignored, caught by a handler, or neither.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Default,
    Ignore,
    Catch,
}

/// What each of [`SIGNALS`] does now, read from the ignored and caught masks in `/proc/self/status`.
fn dispositions() -> [Action; 4] {
    let status = fs::read_to_string("/proc/self/status").unwrap();
    let mask = |name: &str| {
        let hex = status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .unwrap();
        u64::from_str_radix(hex.trim(), 16).unwrap()
    };
    let (ignored, caught) = (mask("SigIgn:"), mask("SigCgt:"));
    SIGNALS.map(|signal| {
        let bit = 1_u64 << (signal.as_raw() - 1);
        if ignored & bit != 0 {
            Action::Ignore
        } else if caught & bit != 0 {
            Action::Catch
        } else {
            Action::Default
        }
    })
}

/// The helper: shows the dashboard of the state directory it is given, drawing each state as a row of one
/// character, so that the test can tell which state is on screen from the bytes the terminal received. Its frames
/// are frozen when it is told so, so that only a signal, a key or a journal change draws again. It checks that
/// every signal action is what it was when the dashboard is gone, and in the mode `after_return` it then waits
/// for a signal.
#[test]
fn real_process_helper() {
    let Some(state_dir) = std::env::var_os(STATE_DIR) else {
        return;
    };
    let mode = std::env::var(MODE).unwrap_or_default();
    let hold = if std::env::var_os(FROZEN).is_some() {
        Hold::Frozen
    } else {
        Hold::Never
    };
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let before = dispositions();
    let mut during_ok = true;
    let render = |journal: &Journal<'_>, screen: &Screen, _: Timestamp, buf: &mut Buffer| {
        if mode == "integrated" {
            let now = dispositions();
            during_ok &= now[..3] == before[..3] && now[3] == Action::Catch;
        }
        let state = match journal {
            Journal::Missing => 'M',
            Journal::Starting => 'S',
            Journal::Problem(_) => 'P',
            Journal::Ready { log, .. } => {
                char::from_digit(u32::try_from(log.total).unwrap(), 10).unwrap()
            }
        };
        let view = match screen.view {
            View::Main => 'a',
            View::Help => 'b',
            View::Log => 'c',
        };
        for x in 0..buf.area.width {
            buf[(x, 0)].set_char(state);
            buf[(x, 1)].set_char(view);
        }
        Drawn { scroll: 0, hold }
    };
    let outcome = if mode == "integrated" {
        run_after(
            Path::new(&state_dir),
            0,
            &Stop::new(),
            stdout.as_fd(),
            Some(stdin.as_fd()),
            render,
        )
    } else {
        run(
            Path::new(&state_dir),
            stdout.as_fd(),
            Some(stdin.as_fd()),
            render,
        )
    };
    if let Err(error) = outcome {
        eprintln!("helper: {error}");
        std::process::exit(3);
    }
    if dispositions() != before {
        std::process::exit(4);
    }
    if !during_ok {
        std::process::exit(5);
    }
    if mode == "after_return" {
        let mut out = std::io::stdout();
        out.write_all(RESTORED).unwrap();
        out.flush().unwrap();
        let _ = std::io::Read::read(&mut std::io::stdin(), &mut [0_u8; 1]);
    }
}

struct Pty {
    master: OwnedFd,
    slave_path: PathBuf,
    seen: Vec<u8>,
}

impl Pty {
    fn open(columns: u16, rows: u16) -> Self {
        let master =
            openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY | OpenptFlags::CLOEXEC).unwrap();
        grantpt(&master).unwrap();
        unlockpt(&master).unwrap();
        let slave_path = PathBuf::from(OsString::from_vec(
            ptsname(&master, Vec::new()).unwrap().into_bytes(),
        ));
        let pty = Self {
            master,
            slave_path,
            seen: Vec::new(),
        };
        pty.resize(columns, rows);
        pty
    }

    fn resize(&self, columns: u16, rows: u16) {
        tcsetwinsize(
            &self.master,
            Winsize {
                ws_row: rows,
                ws_col: columns,
                ws_xpixel: 0,
                ws_ypixel: 0,
            },
        )
        .unwrap();
    }

    fn slave(&self) -> File {
        File::options()
            .read(true)
            .write(true)
            .open(&self.slave_path)
            .unwrap()
    }

    /// Reads what the terminal has received, if anything arrives before `deadline`.
    fn pump(&mut self, deadline: Instant) -> Pumped {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Pumped::Deadline;
        }
        let timeout = Timespec {
            tv_sec: i64::try_from(remaining.as_secs()).unwrap(),
            tv_nsec: i64::from(remaining.subsec_nanos()),
        };
        let mut fds = [PollFd::new(&self.master, PollFlags::IN)];
        if poll(&mut fds, Some(&timeout)).unwrap() == 0 {
            return Pumped::Deadline;
        }
        let mut buf = [0_u8; 8192];
        match rustix::io::read(&self.master, &mut buf) {
            Ok(0) | Err(rustix::io::Errno::IO) => Pumped::End,
            Ok(read) => {
                self.seen.extend_from_slice(&buf[..read]);
                Pumped::Data
            }
            Err(errno) => panic!("reading the terminal failed: {errno}"),
        }
    }

    /// Waits until `found` holds for the bytes received after `from`, and returns how many bytes there are then.
    fn wait_for(&mut self, from: usize, what: &str, found: impl Fn(&[u8]) -> bool) -> usize {
        let deadline = Instant::now() + BOUND;
        loop {
            if found(&self.seen[from..]) {
                return self.seen.len();
            }
            let pumped = self.pump(deadline);
            assert!(
                pumped == Pumped::Data,
                "the terminal never showed {what} ({pumped:?}); it received {:?}",
                String::from_utf8_lossy(&self.seen[from..])
            );
        }
    }

    /// Reads until the helper has closed the terminal, which it must do before the bound passes.
    fn drain(&mut self) {
        let deadline = Instant::now() + BOUND;
        let from = self.seen.len();
        loop {
            match self.pump(deadline) {
                Pumped::Data => {}
                Pumped::End => return,
                Pumped::Deadline => panic!(
                    "the helper kept the terminal open for {BOUND:?}; it received {:?}",
                    String::from_utf8_lossy(&self.seen[from..])
                ),
            }
        }
    }
}

/// What one read of the terminal found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pumped {
    Data,
    /// Every descriptor of the terminal's other side is closed: the helper is gone or going.
    End,
    Deadline,
}

/// A run of at least eight of the same character: a row of a state the helper drew.
fn state_shown(bytes: &[u8], state: char) -> bool {
    let mut needle = [0_u8; 4];
    let needle = state.encode_utf8(&mut needle).as_bytes();
    let wanted = needle.repeat(8);
    bytes.windows(wanted.len()).any(|window| window == wanted)
}

fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|window| window == needle)
}

fn count_of(bytes: &[u8], needle: &[u8]) -> usize {
    bytes
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

/// How a helper is started: the terminal type it is told, its mode, whether its frames are frozen, and whether
/// its keys come from the terminal or from `/dev/null`.
#[derive(Clone, Copy)]
struct Launch<'a> {
    term: &'a str,
    mode: &'a str,
    frozen: bool,
    keys: bool,
}

const PLAIN: Launch<'static> = Launch {
    term: "xterm-256color",
    mode: "",
    frozen: false,
    keys: true,
};

/// The helper process, killed and reaped on every path.
struct Helper(Child);

impl Helper {
    #[expect(
        clippy::disallowed_methods,
        reason = "the test's helper process: this test binary run again on a pseudo-terminal"
    )]
    fn start(pty: &Pty, state_dir: &Path, launch: Launch<'_>) -> Self {
        let slave = pty.slave();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "real_process_helper",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(STATE_DIR, state_dir)
            .env("TERM", launch.term)
            .env(MODE, launch.mode)
            .env_remove("NO_COLOR")
            .env_remove("JOURNAL_STREAM")
            .stdin(if launch.keys {
                Stdio::from(slave.try_clone().unwrap())
            } else {
                Stdio::null()
            })
            .stdout(Stdio::from(slave));
        if launch.frozen {
            command.env(FROZEN, "1");
        } else {
            command.env_remove(FROZEN);
        }
        Self(command.spawn().unwrap())
    }

    fn signal(&self, signal: Signal) {
        kill_process(Pid::from_child(&self.0), signal).unwrap();
    }

    fn finish(mut self) -> std::process::ExitStatus {
        self.0.wait().unwrap()
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn event(seq: u64, payload: Payload) -> Event {
    Event::new(
        Seq::new(seq).unwrap(),
        Stamp {
            time: Timestamp::from_second(1_700_000_000 + i64::try_from(seq).unwrap()).unwrap(),
            mono: None,
        },
        BootId::from("boot-1".to_owned()),
        Vec::new(),
        payload,
    )
}

/// The journal of a session start and `warnings` session warnings, as lines.
fn journal_lines(warnings: u64) -> Vec<u8> {
    let mut events = vec![event(
        1,
        Payload::SessionStart(SessionStart {
            build: Build {
                version: "0.1.0".into(),
                rev: None,
                ruleset: 1,
                schema: SCHEMA,
                fixes: 0,
                evidence: 1,
            },
            session: "20231114T221320Z".parse().unwrap(),
            cores: vec![CoreInfo {
                core: CoreId::new(0).unwrap(),
                ccd: CcdId(0),
                cpus: vec![CpuId(0)],
            }],
        }),
    )];
    for seq in 2..2 + warnings {
        events.push(event(
            seq,
            Payload::SessionWarning(SessionWarning {
                operation: "write state projection".into(),
                trial: None,
                error: format!("warning {seq}"),
            }),
        ));
    }
    let mut bytes = Vec::new();
    for event in &events {
        serde_json::to_writer(&mut bytes, event).unwrap();
        bytes.push(b'\n');
    }
    bytes
}

fn write_journal(dir: &Path, warnings: u64) {
    fs::write(dir.join("events.jsonl"), journal_lines(warnings)).unwrap();
}

fn assert_restored(pty: &Pty, status: std::process::ExitStatus, console: bool) {
    assert!(status.success(), "the helper failed: {status:?}");
    let last = |needle: &[u8]| {
        pty.seen
            .windows(needle.len())
            .rposition(|window| window == needle)
    };
    assert!(
        last(SHOW_CURSOR) > last(HIDE_CURSOR),
        "the cursor was not shown again after the last frame"
    );
    assert_eq!(
        contains(&pty.seen, PALETTE_SET),
        console,
        "the console palette"
    );
    assert_eq!(
        contains(&pty.seen, PALETTE_RESET),
        console,
        "the console palette was not restored"
    );
    let modes = tcgetattr(&pty.master).unwrap().local_modes;
    assert!(
        modes.contains(LocalModes::ICANON | LocalModes::ECHO),
        "the terminal was left raw"
    );
}

fn end_with(signal: Signal, term: &str, console: bool) {
    let state = TempDir::new().unwrap();
    let mut pty = Pty::open(80, 24);
    let helper = Helper::start(&pty, state.path(), Launch { term, ..PLAIN });
    pty.wait_for(0, "the first frame", |bytes| state_shown(bytes, 'M'));
    assert!(
        contains(&pty.seen, HIDE_CURSOR),
        "the cursor was not hidden"
    );
    assert_eq!(
        count_of(&pty.seen, b"\x1b[2J"),
        1,
        "the screen is cleared once on entry"
    );
    helper.signal(signal);
    pty.drain();
    let status = helper.finish();
    assert_restored(&pty, status, console);
}

#[test]
fn real_process_sigint_restores_the_terminal() {
    end_with(Signal::INT, "xterm-256color", false);
}

#[test]
fn real_process_sigterm_restores_the_terminal() {
    end_with(Signal::TERM, "xterm-256color", false);
}

#[test]
fn real_process_sighup_restores_the_terminal() {
    end_with(Signal::HUP, "xterm-256color", false);
}

#[test]
fn real_process_a_signal_on_the_linux_console_restores_the_palette_too() {
    end_with(Signal::TERM, "linux", true);
}

#[test]
fn real_process_sigwinch_repaints_at_the_new_size() {
    let state = TempDir::new().unwrap();
    let mut pty = Pty::open(80, 24);
    let frozen = Launch {
        frozen: true,
        ..PLAIN
    };
    let helper = Helper::start(&pty, state.path(), frozen);
    let first = pty.wait_for(0, "the first frame", |bytes| state_shown(bytes, 'M'));
    assert!(!contains(&pty.seen, &"M".repeat(99).into_bytes()));
    pty.resize(100, 30);
    helper.signal(Signal::WINCH);
    pty.wait_for(first, "a frame of the new width", |bytes| {
        contains(bytes, &"M".repeat(99).into_bytes())
    });
    assert!(
        count_of(&pty.seen[first..], b"\x1b[2J") >= 1,
        "a resize clears the screen"
    );
    helper.signal(Signal::TERM);
    pty.drain();
    let status = helper.finish();
    assert_restored(&pty, status, false);
}

#[test]
fn real_process_keys_switch_views_and_q_quits() {
    let state = TempDir::new().unwrap();
    let mut pty = Pty::open(80, 24);
    let helper = Helper::start(&pty, state.path(), PLAIN);
    let first = pty.wait_for(0, "the first frame", |bytes| state_shown(bytes, 'a'));
    rustix::io::write(&pty.master, b"?").unwrap();
    let help = pty.wait_for(first, "the help view", |bytes| state_shown(bytes, 'b'));
    assert!(
        count_of(&pty.seen[first..], b"\x1b[2J") >= 1,
        "a view change repaints the whole screen"
    );
    rustix::io::write(&pty.master, b"\x1b").unwrap();
    pty.wait_for(help, "the main view again after Esc", |bytes| {
        state_shown(bytes, 'a')
    });
    rustix::io::write(&pty.master, b"q").unwrap();
    pty.drain();
    let status = helper.finish();
    assert_restored(&pty, status, false);
}

/// Shows `before` in a helper, does `act` to the state directory, and expects the dashboard to show `after`.
fn journal_change(
    setup: impl FnOnce(&Path),
    expect_first: char,
    act: impl FnOnce(&Path),
    expect_then: char,
) {
    let root = TempDir::new().unwrap();
    let state = root.path().join("state");
    fs::create_dir(&state).unwrap();
    setup(&state);
    let mut pty = Pty::open(80, 24);
    let helper = Helper::start(&pty, &state, PLAIN);
    let first = pty.wait_for(0, "the first state", |bytes| {
        state_shown(bytes, expect_first)
    });
    act(&state);
    pty.wait_for(first, "the changed journal", |bytes| {
        state_shown(bytes, expect_then)
    });
    helper.signal(Signal::TERM);
    pty.drain();
    helper.finish();
}

fn append_warnings(dir: &Path, from_seq: u64, to_seq: u64) {
    let lines = journal_lines(to_seq - 1);
    let all = String::from_utf8(lines).unwrap();
    let tail: Vec<&str> = all
        .lines()
        .skip(usize::try_from(from_seq - 1).unwrap())
        .collect();
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(dir.join("events.jsonl"))
        .unwrap();
    for line in tail {
        writeln!(file, "{line}").unwrap();
    }
}

#[test]
fn real_process_inotify_an_append_is_shown() {
    journal_change(
        |dir| write_journal(dir, 0),
        '1',
        |dir| append_warnings(dir, 2, 3),
        '3',
    );
}

#[test]
fn real_process_inotify_a_replacement_is_shown() {
    journal_change(
        |dir| write_journal(dir, 2),
        '3',
        |dir| {
            let temporary = dir.parent().unwrap().join("replacement");
            fs::write(&temporary, journal_lines(0)).unwrap();
            fs::rename(temporary, dir.join("events.jsonl")).unwrap();
        },
        '1',
    );
}

#[test]
fn real_process_inotify_a_removal_is_shown_as_no_session() {
    journal_change(
        |dir| write_journal(dir, 0),
        '1',
        |dir| fs::remove_file(dir.join("events.jsonl")).unwrap(),
        'M',
    );
}

#[test]
fn real_process_inotify_a_creation_is_shown() {
    journal_change(|_| {}, 'M', |dir| write_journal(dir, 1), '2');
}

#[test]
fn real_process_inotify_a_missing_state_directory_is_waited_for() {
    let root = TempDir::new().unwrap();
    let state = root.path().join("state").join("nested");
    let mut pty = Pty::open(80, 24);
    let helper = Helper::start(&pty, &state, PLAIN);
    let first = pty.wait_for(0, "no session", |bytes| state_shown(bytes, 'M'));
    fs::create_dir_all(&state).unwrap();
    write_journal(&state, 1);
    pty.wait_for(first, "the journal", |bytes| state_shown(bytes, '2'));
    helper.signal(Signal::TERM);
    pty.drain();
    helper.finish();
}

#[test]
fn real_process_inotify_a_replaced_state_directory_is_noticed() {
    let root = TempDir::new().unwrap();
    let state = root.path().join("state");
    fs::create_dir(&state).unwrap();
    write_journal(&state, 0);
    let mut pty = Pty::open(80, 24);
    let helper = Helper::start(&pty, &state, PLAIN);
    let first = pty.wait_for(0, "the first journal", |bytes| state_shown(bytes, '1'));
    fs::rename(&state, root.path().join("state-old")).unwrap();
    fs::create_dir(&state).unwrap();
    write_journal(&state, 2);
    pty.wait_for(first, "the new directory's journal", |bytes| {
        state_shown(bytes, '3')
    });
    helper.signal(Signal::TERM);
    pty.drain();
    helper.finish();
}

/// Makes a directory listable again when dropped, so that a test that fails leaves no directory behind that
/// cannot be removed.
struct Restore<'a>(&'a Path);

impl Drop for Restore<'_> {
    fn drop(&mut self) {
        let _ = fs::set_permissions(self.0, std::os::unix::fs::PermissionsExt::from_mode(0o755));
    }
}

#[test]
fn real_process_inotify_a_traverse_only_state_directory_is_watched_through_the_journal() {
    if rustix::process::geteuid().is_root() {
        return;
    }
    let root = TempDir::new().unwrap();
    let state = root.path().join("state");
    fs::create_dir(&state).unwrap();
    write_journal(&state, 0);
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(state.join("events.jsonl"))
        .unwrap();
    fs::set_permissions(&state, std::os::unix::fs::PermissionsExt::from_mode(0o311)).unwrap();
    let _restore = Restore(&state);
    let mut pty = Pty::open(80, 24);
    let helper = Helper::start(&pty, &state, PLAIN);
    let first = pty.wait_for(0, "the first journal", |bytes| state_shown(bytes, '1'));
    let rest = journal_lines(1);
    let line: Vec<&[u8]> = rest.split_inclusive(|byte| *byte == b'\n').collect();
    file.write_all(line[1]).unwrap();
    drop(file);
    pty.wait_for(first, "the appended event", |bytes| state_shown(bytes, '2'));
    helper.signal(Signal::TERM);
    pty.drain();
    helper.finish();
}

#[test]
fn real_process_keys_that_are_not_a_terminal_leave_the_dashboard_running() {
    let state = TempDir::new().unwrap();
    let mut pty = Pty::open(80, 24);
    let helper = Helper::start(
        &pty,
        state.path(),
        Launch {
            keys: false,
            ..PLAIN
        },
    );
    let first = pty.wait_for(0, "no session", |bytes| state_shown(bytes, 'M'));
    write_journal(state.path(), 1);
    pty.wait_for(
        first,
        "the journal written after the first frame",
        |bytes| state_shown(bytes, '2'),
    );
    helper.signal(Signal::TERM);
    pty.drain();
    let status = helper.finish();
    assert_restored(&pty, status, false);
}

#[test]
fn real_process_run_puts_every_signal_action_back_and_a_later_sigint_takes_the_default() {
    let state = TempDir::new().unwrap();
    let mut pty = Pty::open(80, 24);
    let helper = Helper::start(
        &pty,
        state.path(),
        Launch {
            mode: "after_return",
            ..PLAIN
        },
    );
    let first = pty.wait_for(0, "the first frame", |bytes| state_shown(bytes, 'M'));
    rustix::io::write(&pty.master, b"q").unwrap();
    pty.wait_for(
        first,
        "the helper's report that the actions are back",
        |bytes| contains(bytes, RESTORED),
    );
    helper.signal(Signal::INT);
    pty.drain();
    let status = helper.finish();
    assert_eq!(
        status.signal(),
        Some(Signal::INT.as_raw()),
        "a SIGINT after the dashboard took no default action: {status:?}"
    );
}

#[test]
fn real_process_run_after_handles_only_sigwinch() {
    let state = TempDir::new().unwrap();
    let mut pty = Pty::open(80, 24);
    let helper = Helper::start(
        &pty,
        state.path(),
        Launch {
            mode: "integrated",
            frozen: true,
            ..PLAIN
        },
    );
    let first = pty.wait_for(0, "the first frame", |bytes| state_shown(bytes, 'S'));
    pty.resize(100, 30);
    helper.signal(Signal::WINCH);
    pty.wait_for(first, "a frame of the new width", |bytes| {
        contains(bytes, &"S".repeat(99).into_bytes())
    });
    rustix::io::write(&pty.master, b"q").unwrap();
    pty.drain();
    let status = helper.finish();
    assert_restored(&pty, status, false);
}

#[test]
fn real_process_run_after_leaves_sigint_to_its_caller() {
    let state = TempDir::new().unwrap();
    let mut pty = Pty::open(80, 24);
    let helper = Helper::start(
        &pty,
        state.path(),
        Launch {
            mode: "integrated",
            ..PLAIN
        },
    );
    pty.wait_for(0, "the first frame", |bytes| state_shown(bytes, 'S'));
    helper.signal(Signal::INT);
    pty.drain();
    let status = helper.finish();
    assert_eq!(status.signal(), Some(Signal::INT.as_raw()));
}

#[test]
fn real_process_the_helper_does_nothing_unless_started_by_a_test() {
    assert!(std::env::var_os(STATE_DIR).is_none());
}

#[test]
fn real_process_a_helper_that_cannot_open_its_terminal_fails_instead_of_hanging() {
    let state = TempDir::new().unwrap();
    let mut pty = Pty::open(80, 24);
    #[expect(
        clippy::disallowed_methods,
        reason = "the test's helper process: this test binary run again"
    )]
    let helper = Helper(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "real_process_helper",
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(STATE_DIR, state.path())
            .stdin(Stdio::from(pty.slave()))
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    pty.drain();
    assert!(!helper.finish().success());
}
