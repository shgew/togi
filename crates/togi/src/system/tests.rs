use std::process::Command;

use jiff::Timestamp;

use super::{Real, System};

const HELPER: &str = "system::tests::zone_helper";
const MARK: &str = "TOGI_ZONE_TEST_MARK";

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
