#[path = "../support/mod.rs"]
mod support;

use std::fs;
use std::path::Path;

use support::{Host, Ran, Session, files};
use togi_cli::Exit;
use togi_hostlock::{HostLock, Policy};
use togi_journal::StateDir;
use togi_machine::CoreId;
use togi_watchtest::Scenario;

const SESSION: &str = "20260101T000000Z";

fn reset(host: &Host, state: &Path, args: &[&str]) -> Ran {
    let mut all = vec!["reset"];
    all.extend_from_slice(args);
    host.run(state, &all)
}

fn archive(state: &Path, name: &str) -> std::path::PathBuf {
    state.join("archive").join(name)
}

fn last_event(session: &Session) -> serde_json::Value {
    serde_json::from_str(session.text().lines().last().unwrap()).unwrap()
}

#[test]
fn reset_core_queues_one_command_in_the_current_boot() {
    let host = Host::new();
    let session = Session::new();
    let before = session.kinds();
    let ran = reset(&host, session.path(), &["--core", "3"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(
        ran.out,
        "reset of core 03 queued; the next togi run restarts its search from the baseline\n"
    );
    assert_eq!(ran.err, "");
    let after = session.kinds();
    assert_eq!(after[..before.len()], before[..]);
    assert_eq!(&after[before.len()..], ["command.reset"]);
    let event = last_event(&session);
    assert_eq!(event["data"], serde_json::json!({"target": {"core": 3}}), "{event}");
    assert_eq!(event["boot"], host.boot.as_str());
}

#[test]
fn reset_core_of_a_core_the_session_lacks_exits_2_and_changes_nothing() {
    let host = Host::new();
    let session = Session::new();
    let before = session.text();
    for core in ["16", "99"] {
        let ran = reset(&host, session.path(), &["--core", core]);
        assert_eq!(ran.exit, Exit::Usage, "{core}: {}", ran.err);
        assert_eq!(ran.out, "");
        assert!(ran.err.contains("togi reset:"), "{}", ran.err);
        assert_eq!(session.text(), before, "{core}");
    }
}

#[test]
fn exactly_one_of_core_and_all_and_a_core_number() {
    for args in [
        &[][..],
        &["--core", "1", "--all"],
        &["--core", "x"],
        &["--core", "-1"],
        &["--core", "300"],
    ] {
        let host = Host::new();
        let session = Session::new();
        let before = files(session.path());
        let ran = reset(&host, session.path(), args);
        assert_eq!(ran.exit, Exit::Usage, "{args:?}: {}", ran.err);
        assert_eq!(ran.out, "", "{args:?}");
        assert_eq!(files(session.path()), before, "{args:?}");
        assert!(!host.lock.exists(), "{args:?}: the host lock was taken");
    }
}

#[test]
fn no_journal_or_no_session_exits_1_without_creating_state() {
    for args in [&["--core", "0"][..], &["--all"]] {
        let host = Host::new();
        let state = tempfile::tempdir().unwrap();
        let ran = reset(&host, state.path(), args);
        assert_eq!(ran.exit, Exit::Error, "{args:?}");
        assert!(ran.err.contains("no journal at"), "{}", ran.err);
        assert_eq!(files(state.path()), Vec::<String>::new(), "{args:?}");

        fs::write(state.path().join("events.jsonl"), "").unwrap();
        let ran = reset(&host, state.path(), args);
        assert_eq!(ran.exit, Exit::Error, "{args:?}");
        assert!(ran.err.contains("no session in"), "{}", ran.err);
    }
}

#[test]
fn reset_all_archives_the_session_and_marks_the_boundary() {
    let host = Host::new();
    let session = Session::new();
    let original = session.text();
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(
        ran.out,
        format!("session {SESSION} archived to archive/{SESSION}.jsonl; the next togi run starts a new session\n")
    );
    assert_eq!(ran.err, "");
    assert!(!session.journal().exists());
    let archived = fs::read_to_string(archive(session.path(), &format!("{SESSION}.jsonl"))).unwrap();
    assert!(archived.starts_with(&original));
    let kinds: Vec<&str> = archived
        .lines()
        .map(|line| {
            let kind = line.split("\"kind\":\"").nth(1).unwrap();
            kind.split('"').next().unwrap()
        })
        .collect();
    assert_eq!(&kinds[kinds.len() - 2..], ["command.reset", "session.archived"]);
    assert!(archive(session.path(), &format!("{SESSION}-reset-all")).exists());
}

#[test]
fn reset_all_of_an_unrecognized_schema_archives_it_unread() {
    let host = Host::new();
    let session = Session::new();
    session.rewrite_first_line("\"schema\":1", "\"schema\":99");
    let original = session.text();
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(
        ran.out,
        format!("session {SESSION} archived to archive/{SESSION}.jsonl without appending to the incompatible journal; the next togi run starts a new session\n")
    );
    assert_eq!(
        fs::read_to_string(archive(session.path(), &format!("{SESSION}.jsonl"))).unwrap(),
        original
    );
    assert!(archive(session.path(), &format!("{SESSION}-reset-all")).exists());
    assert!(!session.journal().exists());
}

#[test]
fn reset_all_of_another_ruleset_archives_it_normally() {
    let host = Host::new();
    let session = Session::new();
    session.rewrite_first_line("\"ruleset\":1", "\"ruleset\":99");
    let ran = reset(&host, session.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert!(!ran.out.contains("without appending"), "{}", ran.out);
    let archived = fs::read_to_string(archive(session.path(), &format!("{SESSION}.jsonl"))).unwrap();
    assert!(archived.contains("\"kind\":\"command.reset\""));
}

#[test]
fn reset_core_refuses_every_stamp_mismatch_with_exit_16_and_appends_nothing() {
    for (name, from, to, advice) in [
        ("newer schema", "\"schema\":1", "\"schema\":99", "Install togi"),
        ("newer ruleset", "\"ruleset\":1", "\"ruleset\":99", "Install togi"),
        ("newer evidence", "\"evidence\":1}", "\"evidence\":99}", "Install togi"),
        ("older ruleset", "\"ruleset\":1", "\"ruleset\":0", "togi run archives it"),
    ] {
        let host = Host::new();
        let session = Session::new();
        session.rewrite_first_line(from, to);
        let before = session.text();
        let ran = reset(&host, session.path(), &["--core", "0"]);
        assert_eq!(ran.exit, Exit::Incompatible, "{name}: {}", ran.err);
        assert_eq!(ran.out, "", "{name}");
        assert!(ran.err.contains("this journal was written by"), "{name}: {}", ran.err);
        assert!(ran.err.contains(advice), "{name}: {}", ran.err);
        assert_eq!(session.text(), before, "{name}");
    }
}

#[test]
fn an_unreadable_journal_exits_1() {
    for args in [&["--core", "0"][..], &["--all"]] {
        let host = Host::new();
        let state = tempfile::tempdir().unwrap();
        fs::write(state.path().join("events.jsonl"), "not json\n").unwrap();
        let ran = reset(&host, state.path(), args);
        assert_eq!(ran.exit, Exit::Error, "{args:?}: {}", ran.err);
        assert_eq!(ran.out, "");
    }
}

#[test]
fn a_busy_host_lock_exits_3_naming_it_before_anything_changes() {
    for args in [&["--core", "0"][..], &["--all"]] {
        let host = Host::new();
        let session = Session::new();
        let before = files(session.path());
        let text = session.text();
        let _held = HostLock::acquire(&host.lock, Policy::Private).unwrap();
        let ran = reset(&host, session.path(), args);
        assert_eq!(ran.exit, Exit::Locked, "{args:?}: {}", ran.err);
        assert_eq!(ran.out, "");
        assert!(ran.err.contains(host.lock.to_str().unwrap()), "{}", ran.err);
        assert_eq!(files(session.path()), before);
        assert_eq!(session.text(), text);
    }
}

#[test]
fn a_held_state_directory_lock_exits_3_and_changes_nothing() {
    for args in [&["--core", "0"][..], &["--all"]] {
        let host = Host::new();
        let session = Session::new();
        let text = session.text();
        let held = StateDir::lock(session.path()).unwrap();
        let ran = reset(&host, session.path(), args);
        assert_eq!(ran.exit, Exit::Locked, "{args:?}: {}", ran.err);
        assert_eq!(ran.out, "");
        drop(held);
        assert_eq!(session.text(), text);
    }
}

#[test]
fn a_pending_carry_is_dropped_without_a_journal() {
    let host = Host::new();
    let state = tempfile::tempdir().unwrap();
    fs::create_dir(state.path().join("archive")).unwrap();
    fs::write(archive(state.path(), &format!("{SESSION}-carry-pending")), "").unwrap();
    let ran = reset(&host, state.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(
        ran.out,
        format!("carry from session {SESSION} dropped; the next togi run starts a new session with nothing carried\n")
    );
    assert!(!archive(state.path(), &format!("{SESSION}-carry-pending")).exists());
    assert!(archive(state.path(), &format!("{SESSION}-reset-all")).exists());
}

#[test]
fn reset_all_completes_an_interrupted_incompatible_archive() {
    let host = Host::new();
    let state = tempfile::tempdir().unwrap();
    fs::create_dir(state.path().join("archive")).unwrap();
    fs::write(archive(state.path(), &format!("{SESSION}.jsonl")), "moved\n").unwrap();
    fs::write(archive(state.path(), &format!("{SESSION}-compat-pending")), "").unwrap();
    let ran = reset(&host, state.path(), &["--all"]);
    assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
    assert_eq!(
        ran.out,
        format!("session {SESSION} archived to archive/{SESSION}.jsonl without appending to the incompatible journal; the next togi run starts a new session\n")
    );
    assert!(!archive(state.path(), &format!("{SESSION}-compat-pending")).exists());
    assert!(archive(state.path(), &format!("{SESSION}-reset-all")).exists());
}

fn failing_scenario() -> (Scenario, CoreId, i64) {
    for scenario in [
        Scenario::Hunt,
        Scenario::Combination,
        Scenario::Deadend,
        Scenario::Stopped,
        Scenario::Concluded,
    ] {
        let events = scenario.events().unwrap();
        let state = togi_tuner::replay(&events).unwrap();
        let points = state.constraints().unwrap().failure_points();
        if let Some((core, point)) = points.iter().next() {
            return (scenario, core, point.offset().get());
        }
    }
    panic!("no scenario records a failure point");
}

#[test]
fn reset_all_warns_for_each_solo_limit_at_or_past_its_failure_point() {
    let (scenario, core, point) = failing_scenario();
    let core = core.get();
    let host = Host::new();
    let cfg = tempfile::tempdir().unwrap();
    let write = |limit: i64| {
        let path = cfg.path().join(format!("config{limit}.json"));
        fs::write(&path, format!(r#"{{"candidate_solo_limits":{{"{core}":{limit}}}}}"#)).unwrap();
        path
    };
    let cases = [
        (point, true),
        (point - 1, true),
        (point + 1, false),
    ];
    for (limit, warns) in cases {
        if !(-50..=0).contains(&limit) {
            continue;
        }
        let session = Session::of(scenario);
        let path = write(limit);
        let ran = reset(&host, session.path(), &["--all", "--config", path.to_str().unwrap()]);
        assert_eq!(ran.exit, Exit::Ok, "{limit}: {}", ran.err);
        let want = format!(
            "warning: candidate solo limit {limit} for core {core:02} is at or deeper than its failure point {point} in the archived session; "
        );
        assert_eq!(ran.err.contains(&want), warns, "{limit}: {}", ran.err);
        if warns {
            let remedy = if point < 0 {
                format!("use {}, the failure point plus one, or remove it", point + 1)
            } else {
                "remove it".to_owned()
            };
            assert!(ran.err.contains(&format!("{want}{remedy}\n")), "{}", ran.err);
        }
    }
}

#[test]
fn an_invalid_or_missing_config_never_stops_the_archive() {
    let host = Host::new();
    let cfg = tempfile::tempdir().unwrap();
    let bad = cfg.path().join("bad.json");
    fs::write(&bad, "{").unwrap();
    let missing = cfg.path().join("missing.json");
    for path in [&bad, &missing] {
        let session = Session::new();
        let ran = reset(&host, session.path(), &["--all", "--config", path.to_str().unwrap()]);
        assert_eq!(ran.exit, Exit::Ok, "{}", ran.err);
        assert!(
            ran.err.starts_with("warning: cannot check candidate solo limits: "),
            "{}",
            ran.err
        );
        assert!(!session.journal().exists());
    }
}

#[test]
fn config_is_accepted_before_and_after_the_command() {
    let host = Host::new();
    let cfg = tempfile::tempdir().unwrap();
    let missing = cfg.path().join("missing.json");
    let missing = missing.to_str().unwrap();
    for args in [
        &["reset", "--all", "--config", missing][..],
        &["--config", missing, "reset", "--all"],
    ] {
        let session = Session::new();
        let ran = host.run(session.path(), args);
        assert_eq!(ran.exit, Exit::Ok, "{args:?}: {}", ran.err);
        assert!(ran.err.contains("cannot check candidate solo limits"), "{args:?}: {}", ran.err);
    }
}

#[test]
fn error_text_escapes_control_characters() {
    let host = Host::new();
    let state = tempfile::tempdir().unwrap();
    let nasty = state.path().join("a\x1b[31mb");
    let ran = reset(&host, &nasty, &["--core", "0"]);
    assert_eq!(ran.exit, Exit::Error);
    assert!(!ran.err.contains('\x1b'), "{:?}", ran.err);
}
