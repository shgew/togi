use std::rc::Rc;

use test_case::test_case;

use super::*;
use crate::chain;
use crate::fakes::{FakeClock, fake_api, repository, respond, uris};

const NONE: &str = "[]";
const QUEUED: &str = r#"[{"html_url":"u","status":"queued","conclusion":null}]"#;
const IN_PROGRESS: &str = r#"[{"html_url":"u","status":"in_progress","conclusion":null}]"#;
const SUCCESS: &str = r#"[{"html_url":"u","status":"completed","conclusion":"success"}]"#;
const FAILURE: &str = r#"[{"html_url":"u","status":"completed","conclusion":"failure"}]"#;
const CANCELLED: &str = r#"[{"html_url":"u","status":"completed","conclusion":"cancelled"}]"#;
const LOOKUP: &str = "GET /repos/o/r/actions/workflows/check.yml/runs?branch=main&event=push&head_sha=base&per_page=1";

/// Waits on a fake API serving `runs` in order, repeating the last, each lookup taking `lookup` of the clock.
fn wait(runs: &[&'static str], lookup: Duration) -> Waited {
    let mut clock = FakeClock::new();
    let start = clock.now.get();
    let now = Rc::clone(&clock.now);
    let runs = runs.to_vec();
    let mut served = 0;
    let (mut api, requests) = fake_api(move |_| {
        let runs = runs[served.min(runs.len() - 1)];
        served += 1;
        now.set(now.get() + lookup);
        respond(
            200,
            &format!(r#"{{"total_count":1,"workflow_runs":{runs}}}"#),
        )
    });
    let mut out = Vec::new();
    let result = wait_for_green_check(&mut api, &repository(), "base", &mut clock, &mut out);
    let lookups = uris(&requests);
    assert!(lookups.iter().all(|uri| uri == LOOKUP), "{lookups:?}");
    Waited {
        result,
        out: String::from_utf8(out).unwrap(),
        elapsed: clock.now.get() - start,
        sleeps: clock.sleeps,
        lookups: lookups.len(),
    }
}

struct Waited {
    result: Result<(), CheckError>,
    out: String,
    sleeps: Vec<Duration>,
    lookups: usize,
    elapsed: Duration,
}

#[test_case(&[SUCCESS], "", "", &[] ; "success")]
#[test_case(&[FAILURE], "check run u for base concluded failure", "", &[] ; "failure")]
#[test_case(&[CANCELLED], "check run u for base concluded cancelled", "", &[] ; "cancelled")]
#[test_case(
    &[NONE, IN_PROGRESS, IN_PROGRESS, SUCCESS],
    "",
    "no check.yml run on main for base; waiting up to 1h0m0s for it to pass\ncheck run u for base is in_progress; waiting up to 59m30s for it to pass\n",
    &[30, 30, 30] ;
    "missing then in progress then success"
)]
#[test_case(
    &[QUEUED, FAILURE],
    "check run u for base concluded failure",
    "check run u for base is queued; waiting up to 1h0m0s for it to pass\n",
    &[30] ;
    "fails while waited on"
)]
#[test_case(
    &[IN_PROGRESS, CANCELLED],
    "check run u for base concluded cancelled",
    "check run u for base is in_progress; waiting up to 1h0m0s for it to pass\n",
    &[30] ;
    "cancelled while waited on"
)]
fn waits_for_green_check(runs: &[&'static str], error: &str, out: &str, sleeps: &[u64]) {
    let waited = wait(runs, Duration::ZERO);
    assert_eq!(
        waited.result.err().map(|e| chain(&e)).unwrap_or_default(),
        error
    );
    assert_eq!(waited.out, out);
    let sleeps: Vec<Duration> = sleeps.iter().map(|s| Duration::from_secs(*s)).collect();
    assert_eq!(waited.sleeps, sleeps);
    assert_eq!(waited.lookups, runs.len());
}

// Each lookup takes 7s: 97 rounds of lookup and 30s sleep reach 59m49s, the 98th lookup leaves 4s for the last
// sleep, and the 99th ends the wait.
#[test_case(NONE, "no check.yml run on main for base after waiting 1h0m0s" ; "missing")]
#[test_case(QUEUED, "check run u for base is queued after waiting 1h0m0s" ; "queued")]
fn wait_for_green_check_times_out(runs: &'static str, want: &str) {
    let waited = wait(&[runs], Duration::from_secs(7));
    assert_eq!(chain(&waited.result.unwrap_err()), want);
    assert_eq!(waited.lookups, 99);
    assert_eq!(waited.sleeps.last(), Some(&Duration::from_secs(4)));
    assert_eq!(waited.elapsed, CHECK_TIMEOUT + Duration::from_secs(7));
}

#[test]
fn check_lookup_failure_reaches_operator() {
    let (mut api, _) = fake_api(|_| respond(403, "denied"));
    let mut clock = FakeClock::new();
    let error = wait_for_green_check(&mut api, &repository(), "base", &mut clock, &mut Vec::new())
        .unwrap_err();
    assert_eq!(
        chain(&error),
        "look up the check.yml run for base: GET /repos/o/r/actions/workflows/check.yml/runs?branch=main&event=push&head_sha=base&per_page=1: HTTP 403: denied"
    );
    assert_eq!(clock.sleeps, []);
}

#[test]
fn queue_failure_reaches_operator() {
    let (mut api, _) = fake_api(|_| respond(403, "denied"));
    assert_eq!(
        chain(&require_no_hardware_wait(&mut api, &repository()).unwrap_err()),
        "list needs-hardware issues: GET /repos/o/r/issues?labels=needs-hardware&page=1&per_page=100&state=all: HTTP 403: denied"
    );
}

#[test_case(Duration::from_secs(3600), "1h0m0s" ; "hour")]
#[test_case(Duration::from_secs(3570), "59m30s" ; "minutes")]
#[test_case(Duration::from_secs(4), "4s" ; "seconds")]
#[test_case(Duration::ZERO, "0s" ; "zero")]
fn prints_durations_as_go_does(duration: Duration, want: &str) {
    assert_eq!(go_duration(duration), want);
}

#[test_case(Duration::from_millis(59_499), Duration::from_secs(59) ; "down")]
#[test_case(Duration::from_millis(59_500), Duration::from_secs(60) ; "half up")]
fn rounds_to_seconds(duration: Duration, want: Duration) {
    assert_eq!(round_to_second(duration), want);
}
