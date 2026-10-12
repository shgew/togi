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
