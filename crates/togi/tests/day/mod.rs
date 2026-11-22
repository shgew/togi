//! The day command refuses missing sessions and shares existing writer/hardware exclusion.

#![cfg(test)]

use crate::support;

use jiff::Timestamp;
use std::path::Path;
use togi_cli::Exit;
use togi_cli::system::System;
use togi_journal::{SessionId, SessionStart, Stamp};

fn seed(host: &support::Host) {
    let dir = host.lock_state_dir(Path::new("/state")).unwrap();
    let mut journal = dir
        .open_at("seed".to_owned().into(), Timestamp::UNIX_EPOCH)
        .unwrap();
    journal
        .append(
            Stamp {
                time: Timestamp::UNIX_EPOCH,
                mono: None,
            },
            SessionStart {
                build: togi_cli::report::current_build(),
                session: SessionId::new(Timestamp::UNIX_EPOCH, None).unwrap(),
                cores: host.topology.clone(),
            }
            .into(),
            &[],
        )
        .unwrap();
    journal.close().unwrap();
}

#[test]
fn no_session_refuses_without_creating_any_state() {
    let host = support::Host::new();
    let ran = host.run(Path::new("/state"), &["day"]);
    assert_eq!(ran.exit, Exit::Error);
    assert!(ran.err.contains("no session journal"), "{}", ran.err);
    assert!(!host.exists("/state"));
    assert_eq!(host.lock_acquisitions.get(), 0);
}

#[test]
fn day_refuses_a_busy_host_before_opening_hardware() {
    let host = support::Host::new();
    seed(&host);
    let _held = host.acquire_host_lock().unwrap();
    let ran = host.run(Path::new("/state"), &["day"]);
    assert_eq!(ran.exit, Exit::Locked);
    assert!(ran.err.contains("lock"), "{}", ran.err);
    assert_eq!(host.calls.get(), 0);
}

#[test]
fn day_refuses_a_busy_journal_before_opening_hardware() {
    let host = support::Host::new();
    seed(&host);
    let _held = host.lock_state_dir(Path::new("/state")).unwrap();
    let ran = host.run(Path::new("/state"), &["day"]);
    assert_eq!(ran.exit, Exit::Locked);
    assert_eq!(host.calls.get(), 0);
}

#[test]
fn status_remains_lock_free_while_day_owns_both_locks() {
    let host = support::Host::new();
    seed(&host);
    let _hardware = host.acquire_host_lock().unwrap();
    let _writer = host.lock_state_dir(Path::new("/state")).unwrap();
    let before = host.lock_acquisitions.get();
    let ran = host.run(Path::new("/state"), &["status"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(host.lock_acquisitions.get(), before);
}

#[test]
fn destination_opens_the_existing_journal_without_starting_another_session() {
    let host = support::Host::new();
    seed(&host);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let mut context = togi_cli::cli::Context {
        state_dir: Path::new("/state"),
        out: &mut out,
        err: &mut err,
        system: &host,
    };
    let journal = togi_cli::day_destination::open(
        &mut context,
        "day".to_owned().into(),
        &togi_cli::report::current_build(),
    )
    .unwrap();
    assert_eq!(journal.events().len(), 1);
    assert!(journal.session().is_some());
    journal.close().unwrap();
}

#[test_case::test_case(&["run"] ; "run")]
#[test_case::test_case(&["reset", "--all"] ; "reset")]
fn tuning_writers_cannot_start_while_day_owns_both_locks(args: &[&str]) {
    let host = support::Host::new();
    seed(&host);
    let _hardware = host.acquire_host_lock().unwrap();
    let _writer = host.lock_state_dir(Path::new("/state")).unwrap();
    let ran = host.run(Path::new("/state"), args);
    assert_eq!(ran.exit, Exit::Locked, "{}", ran.err);
    assert_eq!(host.calls.get(), 0);
}

fn day(session: &support::Session) -> support::Ran {
    session.host().run(session.path(), &["day"])
}

/// Appends an event after the session's last one, the way a later or different build would.
fn append_line(session: &support::Session, kind: &str, data: &str) -> String {
    let text = format!(
        "{}{{\"seq\":{},\"time\":\"2026-01-01T00:00:00Z\",\"boot\":\"b1a8cebe-21d1-14db-3741-5b5df946de05\",\"kind\":\"{kind}\",\"msg\":\"x\",\"data\":{data}}}\n",
        session.text(),
        session.kinds().len() + 1
    );
    session.fs.put(session.journal(), text.clone().into_bytes());
    text
}

fn assert_refused_untouched(session: &support::Session, before: &str, ran: &support::Ran) {
    assert_eq!(ran.out, "");
    assert_eq!(session.text(), before);
    assert!(
        session.kinds().iter().all(|kind| !kind.starts_with("day.")),
        "{:?}",
        session.kinds()
    );
}

#[test_case::test_case("\"ruleset\":", togi_tuner::RULESET, 0, "uses ruleset", "togi run archives it"; "older ruleset")]
#[test_case::test_case("\"ruleset\":", togi_tuner::RULESET, togi_tuner::RULESET + 1, "uses ruleset", "Install togi"; "newer ruleset")]
#[test_case::test_case("\"schema\":", 1, 2, "uses schema", "Install togi"; "newer schema")]
fn a_journal_stamped_by_another_build_is_refused_as_incompatible_and_left_alone(
    field: &str,
    recorded: u32,
    written: u32,
    uses: &str,
    advice: &str,
) {
    let session = support::Session::new();
    session.rewrite_first_line(&format!("{field}{recorded}"), &format!("{field}{written}"));
    let before = session.text();
    let ran = day(&session);
    assert_eq!(ran.exit, Exit::Incompatible, "{}", ran.err);
    assert!(
        ran.err
            .starts_with("togi day: this journal was written by togi "),
        "{}",
        ran.err
    );
    assert!(ran.err.contains(uses), "{}", ran.err);
    assert!(ran.err.contains(advice), "{}", ran.err);
    assert_refused_untouched(&session, &before, &ran);
}

#[test]
fn a_journal_with_an_unknown_kind_is_refused_as_incompatible_and_left_alone() {
    let session = support::Session::new();
    let before = append_line(&session, "future.fact", "{}");
    let ran = day(&session);
    assert_eq!(ran.exit, Exit::Incompatible, "{}", ran.err);
    assert!(
        ran.err
            .starts_with("togi day: unknown kind \"future.fact\" in journal written by togi "),
        "{}",
        ran.err
    );
    assert!(
        ran.err
            .ends_with("cannot safely modify it; install the build that wrote it\n"),
        "{}",
        ran.err
    );
    assert_refused_untouched(&session, &before, &ran);
}

#[test]
fn an_archived_session_is_refused_and_left_alone() {
    let session = support::Session::new();
    let before = append_line(
        &session,
        "session.archived",
        "{\"session\":\"20260101T000000Z\"}",
    );
    let ran = day(&session);
    assert_eq!(ran.exit, Exit::Error, "{}", ran.err);
    assert_eq!(
        ran.err,
        "togi day: the session is archived; start a new session with togi run\n"
    );
    assert_refused_untouched(&session, &before, &ran);
}

#[test_case::test_case(&["pause"], togi_cli::day::socket::Control::Pause { state: togi_journal::PauseState::UntilBoot {}, uid: 1000 }; "pause until cold boot")]
#[test_case::test_case(&["pause", "30m"], togi_cli::day::socket::Control::Pause { state: togi_journal::PauseState::Until { deadline: "2026-01-01T00:30:00Z".parse().unwrap() }, uid: 1000 }; "duration")]
#[test_case::test_case(&["pause", "--until", "01:30"], togi_cli::day::socket::Control::Pause { state: togi_journal::PauseState::Until { deadline: "2026-01-01T01:30:00Z".parse().unwrap() }, uid: 1000 }; "local time")]
#[test_case::test_case(&["resume"], togi_cli::day::socket::Control::Resume { uid: 1000 }; "resume")]
#[test_case::test_case(&["freeze"], togi_cli::day::socket::Control::Freeze { frozen: true, uid: 1000 }; "freeze")]
#[test_case::test_case(&["unfreeze"], togi_cli::day::socket::Control::Freeze { frozen: false, uid: 1000 }; "unfreeze")]
fn controls_are_rootless_and_never_acquire_a_writer_or_hardware(
    args: &[&str],
    expected: togi_cli::day::socket::Control,
) {
    let mut host = support::Host::new();
    host.root = false;
    let command: Vec<_> = std::iter::once("day").chain(args.iter().copied()).collect();
    let ran = host.run(Path::new("/state"), &command);
    assert_eq!((ran.exit, ran.err.as_str()), (Exit::Ok, ""));
    if args == ["resume"] {
        assert_eq!(
            ran.out,
            "committed profile reapplied and verified; day resumed\n"
        );
    }
    assert_eq!(*host.day_controls.borrow(), [expected]);
    assert_eq!(host.lock_acquisitions.get(), 0);
    assert_eq!(host.calls.get(), 0);
    assert!(!host.exists("/state"));
}

#[test]
fn a_refused_control_does_not_claim_success() {
    let mut host = support::Host::new();
    host.root = false;
    *host.day_service.borrow_mut() = Some(Err(togi_cli::day::socket::Error::Refused(
        "not available".into(),
    )));
    let ran = host.run(Path::new("/state"), &["day", "resume"]);
    assert_eq!(ran.exit, Exit::Error);
    assert_eq!(ran.out, "");
    assert!(ran.err.contains("not available"), "{}", ran.err);
    assert_eq!(host.lock_acquisitions.get(), 0);
    assert!(!host.exists("/state"));
}
