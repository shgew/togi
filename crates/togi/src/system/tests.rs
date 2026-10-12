use std::fs::File;
use std::process::Command;

use jiff::Timestamp;
use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

use super::{Real, System, both_terminals};

const HELPER: &str = "system::tests::zone_helper";
const MARK: &str = "TOGI_ZONE_TEST_MARK";

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

/// The helper: prints the offset `Real.zone()` has at the epoch, between marks, when a test started
/// it; it does nothing otherwise.
#[test]
fn zone_helper() {
    if std::env::var_os(MARK).is_none() {
        return;
    }
    let offset = Timestamp::UNIX_EPOCH
        .to_zoned(Real.zone())
        .strftime("%:z")
        .to_string();
    println!("{MARK}{offset}{MARK}");
}

#[test]
fn the_helper_does_nothing_unless_started_by_a_test() {
    assert!(std::env::var_os(MARK).is_none());
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "the test's helper process: this test binary run again with TZ set"
)]
fn the_zone_follows_tz() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args([HELPER, "--exact", "--nocapture", "--test-threads=1"])
        .env(MARK, "1")
        .env("TZ", "EST5")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains(&format!("{MARK}-05:00{MARK}")), "{stdout}");
}
