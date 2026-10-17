use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;

use test_case::test_case;

use super::go_status;

#[test_case(256, "exit status 1" ; "exit code")]
#[test_case(9, "signal: killed" ; "signal")]
#[test_case(9 | 0x80, "signal: killed (core dumped)" ; "signal with core dump")]
#[test_case(64, "signal: signal 64" ; "unnamed signal")]
fn renders_status_as_go_does(raw: i32, want: &str) {
    assert_eq!(go_status(ExitStatus::from_raw(raw)), want);
}
