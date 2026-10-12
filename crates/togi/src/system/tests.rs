use std::fs::File;

use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

use super::both_terminals;

fn terminal() -> (rustix::fd::OwnedFd, File) {
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
fn a_character_device_that_is_not_a_terminal_is_not_a_terminal() {
    let null = File::open("/dev/null").unwrap();
    let (_master, slave) = terminal();
    assert!(!both_terminals(&null, &null));
    assert!(!both_terminals(&null, &slave));
    assert!(!both_terminals(&slave, &null));
}

#[test]
fn two_terminals_are_interactive() {
    let (_master, slave) = terminal();
    assert!(both_terminals(&slave, &slave));
}
