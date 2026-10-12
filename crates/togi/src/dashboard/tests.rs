use std::fs::File;
use std::os::fd::{AsFd as _, OwnedFd};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};
use rustix::termios::{LocalModes, tcgetattr};

use super::{QuietInput, discard_on};

fn terminal() -> (OwnedFd, File) {
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).unwrap();
    grantpt(&master).unwrap();
    unlockpt(&master).unwrap();
    let name = ptsname(&master, Vec::new()).unwrap();
    let slave = File::options()
        .read(true)
        .write(true)
        .open(std::str::from_utf8(name.as_bytes()).unwrap())
        .unwrap();
    (master, slave)
}

#[test]
fn echo_and_line_buffering_stay_off_until_the_input_is_given_back_and_the_signal_keys_stay_on() {
    let (_master, slave) = terminal();
    let before = tcgetattr(&slave).unwrap().local_modes;
    assert!(before.contains(LocalModes::ECHO | LocalModes::ICANON | LocalModes::ISIG));
    let quiet = QuietInput::enter(slave.as_fd()).unwrap().unwrap();
    let during = tcgetattr(&slave).unwrap().local_modes;
    assert!(!during.intersects(LocalModes::ECHO | LocalModes::ICANON));
    assert!(during.contains(LocalModes::ISIG));
    drop(quiet);
    assert_eq!(tcgetattr(&slave).unwrap().local_modes, before);
}

#[test]
fn a_stream_that_is_not_a_terminal_is_left_alone() {
    let null = File::open("/dev/null").unwrap();
    assert!(QuietInput::enter(null.as_fd()).unwrap().is_none());
}

#[test]
fn typed_ahead_input_is_dropped() {
    let (master, slave) = terminal();
    let _quiet = QuietInput::enter(slave.as_fd()).unwrap();
    rustix::io::write(&master, b"typed\n").unwrap();
    let readable = |file: &File| {
        let mut fds = [PollFd::new(file, PollFlags::IN)];
        let none = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        poll(&mut fds, Some(&none)).unwrap() > 0
    };
    assert!(readable(&slave), "the typed bytes reach the terminal input");
    discard_on(slave.as_fd());
    assert!(
        !readable(&slave),
        "nothing typed before the question remains"
    );
}

#[derive(Clone, Default)]
struct Sink(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct UntilStopped;

impl super::Surface for UntilStopped {
    fn draw(&self, _: &std::path::Path, _: u64, stop: &super::Stop) -> Result<(), String> {
        stop.wait();
        Ok(())
    }
}

#[test]
fn event_lines_are_dropped_only_while_the_dashboard_shows() {
    use std::io::Write as _;
    let sink = Sink::default();
    let dashboard = super::Dashboard::new(
        std::sync::Arc::new(UntilStopped),
        std::path::Path::new("."),
        0,
    );
    let mut lines = dashboard.gate(Box::new(sink.clone()));
    lines.write_all(b"startup\n").unwrap();
    dashboard.show();
    lines.write_all(b"hidden event\n").unwrap();
    dashboard.hide();
    lines.write_all(b"shutdown\n").unwrap();
    assert_eq!(&*sink.0.lock().unwrap(), b"startup\nshutdown\n");
}

#[test]
fn the_gate_says_for_each_event_line_whether_it_kept_it_off_the_screen() {
    use std::io::Write as _;
    let sink = Sink::default();
    let dashboard = super::Dashboard::new(
        std::sync::Arc::new(UntilStopped),
        std::path::Path::new("."),
        0,
    );
    let kept = std::rc::Rc::new(std::cell::Cell::new(false));
    let mut lines = dashboard.gate_with_status(Box::new(sink.clone()), std::rc::Rc::clone(&kept));
    writeln!(lines, "before").unwrap();
    assert!(!kept.get());
    dashboard.show();
    writeln!(lines, "while showing").unwrap();
    assert!(kept.get());
    dashboard.hide();
    writeln!(lines, "after").unwrap();
    assert!(!kept.get());
    assert_eq!(&*sink.0.lock().unwrap(), b"before\nafter\n");
}

#[test]
fn input_typed_while_the_dashboard_showed_is_dropped_when_the_input_is_given_back() {
    let (master, slave) = terminal();
    let quiet = QuietInput::enter(slave.as_fd()).unwrap().unwrap();
    rustix::io::write(&master, b"typed during the run\n").unwrap();
    let readable = |file: &File| {
        let mut fds = [PollFd::new(file, PollFlags::IN)];
        let none = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        poll(&mut fds, Some(&none)).unwrap() > 0
    };
    assert!(readable(&slave), "the typed bytes reach the terminal input");
    drop(quiet);
    assert!(
        !readable(&slave),
        "typed-ahead keys must not reach the shell after the run"
    );
}

fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !done() {
        assert!(
            std::time::Instant::now() < end,
            "timed out waiting for {what}"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// A surface built the way [`super::Watch`] is, with the scoped helper that waits for the dashboard's stop,
/// whose drawing panics at once.
struct PanicsWhileDrawing;

impl super::Surface for PanicsWhileDrawing {
    fn draw(&self, _: &std::path::Path, _: u64, stop: &super::Stop) -> Result<(), String> {
        let ended = togi_watch::terminal::Stop::new();
        super::draw_until(stop, &ended, || -> Result<(), String> {
            panic!("injected panic while drawing")
        })
    }
}

/// Every notice and every forwarded event line, in the order they reached the operator.
#[derive(Clone, Default)]
struct Said(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl Said {
    fn push(&self, what: &str, text: &str) {
        self.0.lock().unwrap().push(format!("{what}: {text}"));
    }

    fn all(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

impl std::io::Write for Said {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.push("line", String::from_utf8_lossy(buf).trim_end());
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_panic_while_drawing_falls_back_to_event_lines_at_once_not_when_the_dashboard_is_hidden() {
    use std::io::Write as _;
    let said = Said::default();
    let mut dashboard = super::Dashboard::new(
        std::sync::Arc::new(PanicsWhileDrawing),
        std::path::Path::new("."),
        0,
    );
    let notices = said.clone();
    dashboard.notice = std::sync::Arc::new(move |message| notices.push("notice", message));
    let mut lines = dashboard.gate(Box::new(said.clone()));
    dashboard.show();
    wait_until("the fallback to event lines", || !dashboard.is_showing());
    lines.write_all(b"event after the panic\n").unwrap();
    assert_eq!(
        said.all(),
        [
            "notice: injected panic while drawing",
            "line: event after the panic"
        ],
        "the fallback waited for the dashboard to be hidden"
    );
    dashboard.hide();
}

#[test]
fn drawing_that_returns_stops_the_helper_and_the_dashboard_is_still_showing_until_it_is_hidden() {
    let dashboard = super::Dashboard::new(
        std::sync::Arc::new(UntilStopped),
        std::path::Path::new("."),
        0,
    );
    assert!(!dashboard.is_showing());
    dashboard.show();
    assert!(dashboard.is_showing());
    dashboard.hide();
    assert!(!dashboard.is_showing());
}
